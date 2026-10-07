//! Per-session integrity counters.
//!
//! Deliberately separate from [`crate::crash::live`]: `live` decides what a line
//! *means* and is gated on the tag, while this only counts. A capture that is not
//! watching for crashes must still be able to say how much of it was unparsed, and
//! folding the counters into the crash watch would make those numbers depend on
//! whether the watch happened to be on.
//!
//! The registry is keyed by session id and holds nothing else, so a session that
//! ends without calling [`forget`] leaves one small struct behind rather than any
//! log content.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};

use crate::crash::integrity::CaptureCounters;
use crate::state::LockExt;

static SESSIONS: OnceLock<StdMutex<HashMap<String, CaptureCounters>>> = OnceLock::new();

fn sessions() -> &'static StdMutex<HashMap<String, CaptureCounters>> {
    SESSIONS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Counts one accepted record.
///
/// `parsed` is what the reader infers as `record.message != record.raw`: a record
/// whose message is still the whole line had no grammar to take it apart, which is
/// the same signal the log table uses to mark a row unparsed. `timestamp_ms` is
/// whatever the record's own time text resolved to — device timestamps are local
/// text with no year, so callers pass `None` until a gap source exists.
pub fn note_record(session_id: &str, parsed: bool, timestamp_ms: Option<i64>) {
    let mut guard = sessions().lock_ignore_poison();
    guard
        .entry(session_id.to_owned())
        .or_default()
        .observe(parsed, timestamp_ms);
}

/// The session's counters, for the integrity checks.
///
/// Reading does not consume: a report is rebuilt whenever a probe finishes, and the
/// answer must not change because somebody looked at it. [`forget`] is what ends it.
#[must_use]
pub fn counters(session_id: &str) -> Option<CaptureCounters> {
    let guard = sessions().lock_ignore_poison();
    guard.get(session_id).cloned()
}

/// Counts lines that were recognised as binary and left out of the text stream.
pub fn note_binary(session_id: &str, count: u64) {
    if count == 0 {
        return;
    }
    let mut guard = sessions().lock_ignore_poison();
    guard
        .entry(session_id.to_owned())
        .or_default()
        .note_binary(count);
}

/// Drops a session's counters, when the capture is done with them.
pub fn forget(session_id: &str) {
    let mut guard = sessions().lock_ignore_poison();
    guard.remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_can_be_read_more_than_once() {
        let id = "session-counters-1";
        note_record(id, true, None);
        note_record(id, false, None);
        note_binary(id, 3);
        note_binary(id, 0);

        let first = counters(id).expect("counters");
        assert_eq!(first.records(), 2);
        assert_eq!(first.unparsed(), 1);
        // The drop count is *not* accumulated here: the ring buffer owns that number and the
        // report reads it directly (`RingStats::total_dropped`). A second additive counter
        // would have doubled it the moment both paths ran.
        assert_eq!(first.dropped(), 0);
        assert_eq!(first.binary(), 3);
        // Reading is not destructive.
        let second = counters(id).expect("counters again");
        assert_eq!(second.records(), 2);
        forget(id);
        assert!(counters(id).is_none());
    }

    #[test]
    fn an_unknown_session_has_no_counters() {
        assert!(counters("session-counters-unknown").is_none());
        // Forgetting twice is harmless.
        forget("session-counters-unknown");
        assert!(counters("session-counters-unknown").is_none());
    }
}
