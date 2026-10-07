//! One file on disk that lets the timeline survive a restart.
//!
//! Deliberately *one* file holding the *latest* capture, not a database of every session: the
//! point is that reopening the app does not throw away what the user was just looking at, and a
//! growing archive of 100k-row captures would need retention rules, a schema migration story and
//! a size limit that nobody asked for. Overwriting is the honest version of "light".
//!
//! What is stored is what the view renders — the live events and the analysis — so a reopened
//! app shows the same story, including the parts that cost adb round trips to produce. What is
//! *not* stored is the log rows themselves: they live in the ring buffer, they are large, and
//! the table is empty again after a restart by design.
//!
//! The file carries a version. A file written by a future version is ignored rather than
//! half-read: a wrong timeline is worse than no timeline.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::crash::live::LiveEvent;
use crate::crash::pipeline::ForensicsView;

/// Schema version of the snapshot file.
pub const SNAPSHOT_VERSION: u32 = 1;

/// File name inside the app data directory.
pub const SNAPSHOT_FILE: &str = "timeline.json";

/// Upper bound on stored live events: a capture can produce thousands of AMS signals, and the
/// timeline is read by a person.
pub const MAX_SNAPSHOT_EVENTS: usize = 1_000;

/// The latest capture, as the timeline needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSnapshot {
    /// Schema version — see the module comment.
    pub version: u32,
    /// Session the snapshot belongs to.
    pub session_id: String,
    /// Host wall-clock time it was written, in milliseconds since the Unix epoch.
    pub saved_at_ms: u64,
    /// Live events, newest state of each.
    pub events: Vec<LiveEvent>,
    /// The analysis, complete with the sequence map.
    pub view: ForensicsView,
}

impl TimelineSnapshot {
    /// Builds a snapshot, trimming the event list to what is worth storing.
    #[must_use]
    pub fn new(
        session_id: &str,
        saved_at_ms: u64,
        mut events: Vec<LiveEvent>,
        view: ForensicsView,
    ) -> Self {
        if events.len() > MAX_SNAPSHOT_EVENTS {
            // Keep the newest: a timeline is read for what just happened, and dropping the tail
            // would drop the crash that prompted the capture.
            let drop = events.len() - MAX_SNAPSHOT_EVENTS;
            events.drain(0..drop);
        }
        Self {
            version: SNAPSHOT_VERSION,
            session_id: session_id.to_owned(),
            saved_at_ms,
            events,
            view,
        }
    }

    /// The capture-level remark, when the snapshot has one.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.view.notice.as_deref()
    }
}

/// Full path of the snapshot file inside `dir`.
#[must_use]
pub fn snapshot_path(dir: &Path) -> PathBuf {
    dir.join(SNAPSHOT_FILE)
}

/// Writes the snapshot, replacing whatever was there.
///
/// Errors are the caller's to report: a capture that cannot be remembered is a lost convenience,
/// not a failed capture, so the command layer decides how loud to be about it.
pub fn save(dir: &Path, snapshot: &TimelineSnapshot) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_string(snapshot)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(snapshot_path(dir), json)
}

/// Reads the snapshot, or `None` when there is nothing usable there.
///
/// Every failure is "no snapshot": a missing file, unreadable JSON, or a version this build
/// does not understand. None of them is worth an error dialog on startup, and half-understanding
/// an old file would be worse than starting clean.
#[must_use]
pub fn load(dir: &Path) -> Option<TimelineSnapshot> {
    let text = std::fs::read_to_string(snapshot_path(dir)).ok()?;
    let snapshot: TimelineSnapshot = serde_json::from_str(&text).ok()?;
    if snapshot.version != SNAPSHOT_VERSION {
        return None;
    }
    Some(snapshot)
}

/// Removes the snapshot, when the user no longer wants it.
pub fn clear(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(snapshot_path(dir)) {
        Ok(()) => Ok(()),
        // Already gone is the outcome the caller wanted.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::correlate::Correlation;
    use crate::crash::pipeline::Forensics;

    fn empty_view() -> ForensicsView {
        ForensicsView {
            analysis: Forensics {
                signals: Vec::new(),
                links: Vec::new(),
                anomalies: Vec::new(),
                correlation: Correlation {
                    stories: Vec::new(),
                    unlinked_events: 0,
                    unlinked_links: 0,
                },
                anomalies_by_story: crate::crash::resources::ResourceAttachment {
                    by_story: Vec::new(),
                    unattached: 0,
                },
                checks: Vec::new(),
                known: Vec::new(),
            },
            seq_at: Vec::new(),
            notice: Some("检测到历史日志混入".to_owned()),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        // A directory of our own per test: two tests writing one file would race.
        let dir = std::env::temp_dir().join(format!("droidlog-store-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_snapshot_round_trips() {
        let dir = temp_dir("round-trip");
        let snapshot = TimelineSnapshot::new("session-1", 1_700_000_000_000, Vec::new(), empty_view());
        save(&dir, &snapshot).expect("save");
        let loaded = load(&dir).expect("load");
        assert_eq!(loaded, snapshot);
        assert_eq!(loaded.notice(), Some("检测到历史日志混入"));
        clear(&dir).expect("clear");
        assert!(load(&dir).is_none(), "cleared means gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_usable_reads_as_no_snapshot() {
        let dir = temp_dir("absent");
        assert!(load(&dir).is_none(), "missing directory");
        std::fs::create_dir_all(&dir).expect("dir");
        assert!(load(&dir).is_none(), "missing file");
        // Unreadable JSON must not panic or half-load.
        std::fs::write(snapshot_path(&dir), "{ not json").expect("write");
        assert!(load(&dir).is_none(), "broken json");
        // A file from a future version is ignored rather than guessed at.
        let future = TimelineSnapshot {
            version: SNAPSHOT_VERSION + 1,
            ..TimelineSnapshot::new("s", 0, Vec::new(), empty_view())
        };
        std::fs::write(snapshot_path(&dir), serde_json::to_string(&future).expect("json")).expect("write");
        assert!(load(&dir).is_none(), "future version");
        // Clearing something that is not there is fine.
        clear(&dir).expect("clear missing is fine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_newest_events_are_kept() {
        use crate::crash::live::{LiveEvent, LivePayload};
        use crate::crash::structured::{CrashEvent, CrashOrigin};

        let event = |index: usize| LiveEvent {
            id: format!("crashBuffer#{index}"),
            complete: true,
            line_index: index,
            payload: LivePayload::Crash(Box::new(CrashEvent {
                id: format!("crashBuffer#{index}"),
                origin: CrashOrigin::CrashBuffer,
                kind: None,
                process: Some("com.example".to_owned()),
                pid: Some(index as i32),
                tid: None,
                thread: None,
                exception: Some("java.lang.RuntimeException".to_owned()),
                message: None,
                frames: Vec::new(),
                caused_by: Vec::new(),
                timestamp: None,
                raw: vec!["x".to_owned()],
            })),
        };
        let events: Vec<LiveEvent> = (0..MAX_SNAPSHOT_EVENTS + 5).map(event).collect();
        let snapshot = TimelineSnapshot::new("s", 0, events, empty_view());
        assert_eq!(snapshot.events.len(), MAX_SNAPSHOT_EVENTS);
        assert_eq!(
            snapshot.events.first().map(|event| event.line_index),
            Some(5),
            "the oldest five are dropped, not the newest"
        );
        assert_eq!(
            snapshot.events.last().map(|event| event.line_index),
            Some(MAX_SNAPSHOT_EVENTS + 4)
        );
    }
}
