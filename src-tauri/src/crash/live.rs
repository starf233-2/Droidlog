//! Live crash watching for an active capture.
//!
//! The analysis in this module tree (`structured`, `ams`, `resources`, `correlate`)
//! is written for batches: give it lines, get events. A live session is different —
//! it must decide, for **every** line of a log storm, whether that line is worth a
//! second look, and it must do so without becoming the reason the app drops frames.
//!
//! So the hot path here has exactly two steps:
//!
//! 1. [`tag_is_interesting`] — an equality/prefix check against a fixed tag list.
//!    A record whose tag is not in the list costs a few string comparisons and stops
//!    there; nothing is allocated, nothing is parsed. Ordinary chatter (`dalvikvm`,
//!    `SurfaceFlinger`, an app's own tags) never reaches step 2.
//! 2. The line is parsed by the *existing* parsers for its family. Those are only
//!    reached by the tags that can actually carry a crash, AMS event or resource
//!    anomaly, which on a real device is a few lines per second at most.
//!
//! Crash blocks are accumulated incrementally, and each addition re-emits the event
//! with a **stable id** (`crashBuffer#<index of the marker line>`) plus
//! `complete: false`; the closing line emits the same id with `complete: true`. The
//! frontend therefore shows a stack as it grows and then settles, and a consumer
//! that only wants finished crashes can filter on the flag. Re-parsing the block per
//! line is deliberate: a block is bounded (see [`MAX_BLOCK_LINES`]) and small, and
//! re-parsing cannot drift out of step with the batch parser the way an incremental
//! hand-rolled state machine would.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::state::LockExt;

use crate::crash::ams::{self, AmsSignal};
use crate::crash::resources::{self, ResourceAnomaly};
use crate::crash::structured::{self, CrashEvent};

/// Tags that can carry something worth analysing.
///
/// Kept as data so the gate stays a pure comparison and the list is visible in one
/// place. `am_` is checked as a prefix because the events buffer names the tag after
/// the event (`am_crash`, `am_anr`, `am_kill`, `am_proc_died`, …), and ROMs add
/// their own.
const INTERESTING_TAGS: &[&str] = &[
    "ActivityManager",
    "AndroidRuntime",
    "DEBUG",
    "libc",
    "art",
    "fdsan",
    "lowmemorykiller",
    "lmkd",
    "JavaBinder",
    "Binder",
    "Watchdog",
    "system_server",
    "am_",
];

/// Upper bound on the lines kept for one crash block.
///
/// A Java crash is tens of lines and a tombstone a few hundred; the cap only exists
/// so a pathological stream cannot grow the block without bound. When it is hit the
/// event is emitted as complete and a fresh one starts.
pub const MAX_BLOCK_LINES: usize = 400;

/// True when a log tag can carry a crash, an AMS event or a resource anomaly.
///
/// This is the hot-path gate: no allocation, no parsing, and cheap enough to run on
/// every line of a 10 万行 capture.
#[must_use]
pub fn tag_is_interesting(tag: &str) -> bool {
    if tag.starts_with("am_") {
        return true;
    }
    INTERESTING_TAGS.contains(&tag)
}

/// The payload of a live event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum LivePayload {
    /// A structured crash (possibly still growing).
    Crash(Box<CrashEvent>),
    /// An ActivityManager signal.
    Ams(Box<AmsSignal>),
    /// A resource anomaly.
    Anomaly(Box<ResourceAnomaly>),
}

/// Something the live watch decided was worth reporting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveEvent {
    /// Stable id: crash blocks keep it while they grow, so a consumer can replace.
    pub id: String,
    /// False while a crash block is still receiving lines.
    pub complete: bool,
    /// Index of the line (within the capture) this event came from.
    pub line_index: usize,
    /// The event itself.
    pub payload: LivePayload,
}

/// Per-session state of the live watch.
#[derive(Debug, Default)]
pub struct LiveWatch {
    /// Lines of the crash block currently open.
    block: Vec<String>,
    /// Id of the open block, fixed when it opened.
    block_id: Option<String>,
    /// Line index of the marker that opened the block.
    block_line: usize,
    /// Counter used to give each block a distinct id even if line indices repeat.
    blocks_seen: u64,
}

impl LiveWatch {
    /// A fresh watch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of crash blocks opened so far (used by the report).
    #[must_use]
    pub fn blocks_seen(&self) -> u64 {
        self.blocks_seen
    }

    /// Whether a crash block is currently open.
    #[must_use]
    pub fn has_open_block(&self) -> bool {
        self.block_id.is_some()
    }

    /// Feeds one line in, returning whatever it produced (usually nothing).
    ///
    /// `tag` is the already-parsed log tag and `body` the message, so the caller pays
    /// for parsing once; `line_index` is the capture-local sequence number used for
    /// ids and for jumping back to the row in the table.
    pub fn observe(&mut self, tag: &str, body: &str, line_index: usize) -> Vec<LiveEvent> {
        // Step 1: the gate. Anything ordinary stops here.
        if !tag_is_interesting(tag) {
            return Vec::new();
        }

        let mut events = Vec::new();

        // Step 2a: does this line start a crash block?
        let starts_block = structured::starts_crash_block(body);
        if starts_block {
            // A new marker while a block is open closes the previous one.
            if let Some(event) = self.close_block() {
                events.push(event);
            }
            self.block_id = Some(format!("crashBuffer#{line_index}"));
            self.block_line = line_index;
            self.blocks_seen = self.blocks_seen.saturating_add(1);
            self.block.clear();
        }

        if self.block_id.is_some() {
            if structured::continues_crash_block(body) || starts_block {
                self.block.push(body.to_owned());
                // Bounded growth; a runaway block is closed rather than kept.
                let at_cap = self.block.len() >= MAX_BLOCK_LINES;
                let parsed = self.parse_block(false);
                if let Some(event) = parsed {
                    events.push(event);
                }
                if at_cap {
                    if let Some(event) = self.close_block() {
                        events.push(event);
                    }
                }
                // Frames are already reported; the AMS/resource families cannot also
                // match an `AndroidRuntime` frame line, so stop here.
                return events;
            }
            if let Some(event) = self.close_block() {
                events.push(event);
            }
        }

        // Step 2b: AMS signals and resource anomalies, on the same line.
        //
        // Rebuilt as `<tag>: <body>` because those parsers recognise their families
        // by the tag word (`am_crash:`, `lowmemorykiller:`) — a record arrives with
        // the tag already split off, so the word has to be put back. This only
        // happens for tags the gate accepted, and it is one small allocation per
        // interesting line, not per line.
        let rebuilt = format!("{tag}: {body}");
        if let Some(signal) = ams::parse_line(&rebuilt, line_index) {
            events.push(LiveEvent {
                id: format!("{}#{}", signal.kind.id(), line_index),
                complete: true,
                line_index,
                payload: LivePayload::Ams(Box::new(signal)),
            });
        }
        if let Some(anomaly) = resources::parse_line(&rebuilt, line_index) {
            events.push(LiveEvent {
                id: format!("{}#{}", anomaly.kind.id(), line_index),
                complete: true,
                line_index,
                payload: LivePayload::Anomaly(Box::new(anomaly)),
            });
        }

        events
    }

    /// Parses the open block into a crash event.
    fn parse_block(&self, complete: bool) -> Option<LiveEvent> {
        let id = self.block_id.clone()?;
        let lines: Vec<&str> = self.block.iter().map(String::as_str).collect();
        let event = structured::parse_crash_buffer(&lines).into_iter().next()?;
        Some(LiveEvent {
            id,
            complete,
            line_index: self.block_line,
            payload: LivePayload::Crash(Box::new(event)),
        })
    }

    /// Closes the open block, if any, and returns its final event.
    fn close_block(&mut self) -> Option<LiveEvent> {
        let event = self.parse_block(true);
        self.block.clear();
        self.block_id = None;
        event
    }

    /// Flushes whatever is open, for the end of a capture.
    pub fn finish(&mut self) -> Vec<LiveEvent> {
        self.close_block().into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_rejects_ordinary_chatter() {
        assert!(!tag_is_interesting("dalvikvm"));
        assert!(!tag_is_interesting("SurfaceFlinger"));
        assert!(!tag_is_interesting("com.example.app"));
        assert!(!tag_is_interesting(""));
        assert!(!tag_is_interesting("Activity")); // near-miss on purpose
        // ...and accepts the families that matter.
        assert!(tag_is_interesting("ActivityManager"));
        assert!(tag_is_interesting("AndroidRuntime"));
        assert!(tag_is_interesting("libc"));
        assert!(tag_is_interesting("lowmemorykiller"));
        assert!(tag_is_interesting("lmkd"));
        assert!(tag_is_interesting("am_crash"));
        assert!(tag_is_interesting("am_whatever_this_rom_added"));
    }

    #[test]
    fn ordinary_lines_produce_nothing_at_all() {
        let mut watch = LiveWatch::new();
        for index in 0..10_000 {
            let events = watch.observe("SurfaceFlinger", "nothing to see here", index);
            assert!(events.is_empty(), "chat must not produce events");
        }
        assert!(!watch.has_open_block());
        assert_eq!(watch.blocks_seen(), 0);
        assert!(watch.finish().is_empty());
    }

    #[test]
    fn a_java_crash_grows_and_then_completes_with_one_id() {
        let mut watch = LiveWatch::new();
        let mut ids = Vec::new();
        let mut complete_flags = Vec::new();
        let lines = [
            "FATAL EXCEPTION: main",
            "Process: com.example.app, PID: 1234",
            "java.lang.NullPointerException: boom",
            "\tat com.example.app.Main.onCreate(Main.java:42)",
        ];
        for (index, line) in lines.iter().enumerate() {
            for event in watch.observe("AndroidRuntime", line, index) {
                ids.push(event.id.clone());
                complete_flags.push(event.complete);
                if let LivePayload::Crash(crash) = &event.payload {
                    // Only the block that already contains the `Process:` line can
                    // know the pid; the marker line on its own cannot.
                    if index >= 1 {
                        assert_eq!(crash.pid, Some(1234));
                    }
                }
            }
        }
        assert!(watch.has_open_block());
        assert_eq!(ids.len(), 4, "each line re-emits the growing block");
        assert!(ids.iter().all(|id| *id == "crashBuffer#0"), "ids stay stable: {ids:?}");
        assert!(complete_flags.iter().all(|complete| !complete), "still open");

        // A line from another tag closes it.
        let closing = watch.observe("ActivityManager", "Killing 1234:com.example.app/u0a12 (adj 0): crash", 4);
        assert!(!watch.has_open_block());
        let finished: Vec<&LiveEvent> = closing
            .iter()
            .filter(|event| matches!(event.payload, LivePayload::Crash(_)))
            .collect();
        assert_eq!(finished.len(), 1);
        assert!(finished.first().is_some_and(|event| event.complete));
        assert_eq!(finished.first().map(|event| event.id.as_str()), Some("crashBuffer#0"));
        // The same line also yields the AMS signal.
        assert!(closing.iter().any(|event| matches!(event.payload, LivePayload::Ams(_))));
    }

    #[test]
    fn a_second_crash_gets_a_second_id() {
        let mut watch = LiveWatch::new();
        let first: Vec<LiveEvent> = watch.observe("AndroidRuntime", "FATAL EXCEPTION: main", 10);
        assert_eq!(first.first().map(|event| event.id.as_str()), Some("crashBuffer#10"));
        let second: Vec<LiveEvent> = watch
            .observe("AndroidRuntime", "FATAL EXCEPTION: main", 20)
            .into_iter()
            .filter(|event| matches!(event.payload, LivePayload::Crash(_)))
            .collect();
        // Two crash events: the block opened at 10 is closed, and the new marker
        // opens one that reports itself immediately.
        assert_eq!(second.len(), 2, "{second:#?}");
        assert_eq!(second.first().map(|event| event.id.as_str()), Some("crashBuffer#10"));
        assert!(second.first().is_some_and(|event| event.complete));
        assert_eq!(second.get(1).map(|event| event.id.as_str()), Some("crashBuffer#20"));
        assert!(second.get(1).is_some_and(|event| !event.complete));
        // The block that opened at 20 is now the open one.
        assert!(watch.has_open_block());
        assert_eq!(watch.blocks_seen(), 2);
    }

    #[test]
    fn ams_and_resource_lines_are_reported_without_opening_blocks() {
        let mut watch = LiveWatch::new();
        let ams_events = watch.observe(
            "am_crash",
            "[0,5555,com.example,0,java.lang.RuntimeException,boom,Main.java,1]",
            0,
        );
        assert_eq!(ams_events.len(), 1);
        assert!(matches!(ams_events.first().map(|e| &e.payload), Some(LivePayload::Ams(_))));
        assert!(!watch.has_open_block());

        let resource = watch.observe(
            "lowmemorykiller",
            "Killing 'com.example.bg' (1234), uid 10123, oom_score_adj=900",
            1,
        );
        assert_eq!(resource.len(), 1);
        assert!(matches!(resource.first().map(|e| &e.payload), Some(LivePayload::Anomaly(_))));

        // An ActivityManager line that is neither carries nothing.
        assert!(watch.observe("ActivityManager", "Start proc com.example for activity", 2).is_empty());
    }

    #[test]
    fn a_runaway_block_is_capped_rather_than_unbounded() {
        let mut watch = LiveWatch::new();
        let _ = watch.observe("AndroidRuntime", "FATAL EXCEPTION: main", 0);
        for index in 1..(MAX_BLOCK_LINES + 50) {
            let _ = watch.observe("AndroidRuntime", "\tat com.example.Frame.run(Frame.java:1)", index);
        }
        // The cap closes blocks instead of growing one forever; nothing panics and
        // the watch is still usable.
        assert!(watch.blocks_seen() >= 1);
        let _ = watch.finish();
    }

    #[test]
    fn malformed_input_is_handled() {
        let mut watch = LiveWatch::new();
        for (index, line) in ["", "   ", "FATAL EXCEPTION: ", "[", "am_crash: [", "signal "].iter().enumerate() {
            let _ = watch.observe("AndroidRuntime", line, index);
            let _ = watch.observe("am_crash", line, index);
        }
        let _ = watch.finish();
    }

    #[test]
    fn finish_reports_the_open_block() {
        let mut watch = LiveWatch::new();
        let _ = watch.observe("AndroidRuntime", "FATAL EXCEPTION: main", 3);
        let _ = watch.observe("AndroidRuntime", "Process: com.example, PID: 7", 4);
        let finished = watch.finish();
        assert_eq!(finished.len(), 1);
        let event = finished.first().expect("event");
        assert!(event.complete);
        assert_eq!(event.line_index, 3);
        if let LivePayload::Crash(crash) = &event.payload {
            assert_eq!(crash.pid, Some(7));
        } else {
            panic!("expected a crash payload");
        }
        assert!(!watch.has_open_block());
        assert!(watch.finish().is_empty());
    }
}

/// Event carrying live crash events to the frontend.
pub const EVENT_CRASH: &str = "droidlog://crash-event";

/// Payload of [`EVENT_CRASH`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashEventsEvent {
    /// Session the events belong to.
    pub session_id: String,
    /// The events produced by this batch.
    pub events: Vec<LiveEvent>,
}

/// Upper bound on the events remembered per session.
///
/// The timeline is a summary, not a log: crash blocks re-emit as they grow, so the
/// list is replaced by id rather than appended to, and this cap only exists so a
/// pathological capture cannot grow it without bound.
const MAX_SESSION_EVENTS: usize = 512;

/// Per-session watch plus what it has produced so far.
#[derive(Default)]
struct SessionWatch {
    watch: LiveWatch,
    events: Vec<LiveEvent>,
}

static SESSIONS: OnceLock<StdMutex<HashMap<String, SessionWatch>>> = OnceLock::new();

fn sessions() -> &'static StdMutex<HashMap<String, SessionWatch>> {
    SESSIONS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Feeds one record of a session to its watch and remembers what came out.
///
/// Called from the batch flush, so the cost is per *batch* entry and, inside
/// [`LiveWatch::observe`], gated on the tag: ordinary records stop at a comparison.
pub fn observe_session(session_id: &str, tag: &str, body: &str, line_index: usize) -> Vec<LiveEvent> {
    let mut guard = sessions().lock_ignore_poison();
    let entry = guard.entry(session_id.to_owned()).or_default();
    let events = entry.watch.observe(tag, body, line_index);
    for event in &events {
        // A growing crash block keeps its id, so it replaces its earlier self.
        match entry.events.iter_mut().find(|known| known.id == event.id) {
            Some(existing) => *existing = event.clone(),
            None => {
                if entry.events.len() < MAX_SESSION_EVENTS {
                    entry.events.push(event.clone());
                }
            }
        }
    }
    events
}

/// Takes everything accumulated for a session, leaving the watch in place.
#[must_use]
pub fn take_events(session_id: &str) -> Vec<LiveEvent> {
    let mut guard = sessions().lock_ignore_poison();
    match guard.get_mut(session_id) {
        Some(entry) => std::mem::take(&mut entry.events),
        None => Vec::new(),
    }
}

/// Flushes the open block and forgets the session (called when a capture ends).
pub fn finish_session(session_id: &str) -> Vec<LiveEvent> {
    let mut guard = sessions().lock_ignore_poison();
    let Some(mut entry) = guard.remove(session_id) else {
        return Vec::new();
    };
    let mut events = entry.watch.finish();
    events.append(&mut entry.events);
    events
}

/// The structured crashes a session has seen, without consuming them.
///
/// [`take_events`] drains, which is right for a one-shot fetch; a report is rebuilt
/// whenever a probe finishes and must see the same crashes every time, so this only
/// clones. Only crash payloads are returned — the timeline reads all event kinds, but
/// the integrity check asks one question: did the crashes carry a stack?
#[must_use]
pub fn crash_events(session_id: &str) -> Vec<crate::crash::structured::CrashEvent> {
    let guard = sessions().lock_ignore_poison();
    let Some(entry) = guard.get(session_id) else {
        return Vec::new();
    };
    entry
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            LivePayload::Crash(crash) => Some((**crash).clone()),
            _ => None,
        })
        .collect()
}

/// Every live event a session has produced, newest state of each.
///
/// A snapshot rather than a drain: the timeline can be opened, closed and reopened,
/// and each opening must see the same story. Growing crash blocks are already
/// replaced by id inside the registry, so this is the settled view.
#[must_use]
pub fn events_snapshot(session_id: &str) -> Vec<LiveEvent> {
    let guard = sessions().lock_ignore_poison();
    guard
        .get(session_id)
        .map(|entry| entry.events.clone())
        .unwrap_or_default()
}

/// Records a crash that was already parsed — a probe's read of a dropbox entry, an ANR
/// trace or a tombstone.
///
/// The line-by-line path cannot serve these: a probe hands over a whole text, and the
/// table rows it produced are not the thing the timeline should show. The id is prefixed
/// with `probe:` so such an event can never collide with a live block's id, even when
/// both describe the same crash.
///
/// `line_index` is 0 because a probe event has no row of its own in the table — the text
/// came from a file on the device, not from the log stream. The timeline reads that as
/// "no line to jump to" rather than as "row zero".
pub fn note_crash_event(
    session_id: &str,
    crash: crate::crash::structured::CrashEvent,
) -> LiveEvent {
    let event = LiveEvent {
        id: format!("probe:{}", crash.id),
        complete: true,
        line_index: 0,
        payload: LivePayload::Crash(Box::new(crash)),
    };
    let mut guard = sessions().lock_ignore_poison();
    let entry = guard.entry(session_id.to_owned()).or_default();
    if entry.events.len() < MAX_SESSION_EVENTS {
        entry.events.push(event.clone());
    }
    event
}

/// [`observe_session`], plus the row's own identity for crashes that name no process.
///
/// A `FATAL EXCEPTION` block frequently carries no `Process:` line, and the row it came
/// from knows both the package and the pid. Filling them here is the difference between
/// a timeline row that reads `com.android.settings · 3 帧` and one that says "未知进程" —
/// the information was there all along, one level up.
pub fn observe_session_record(
    session_id: &str,
    tag: &str,
    body: &str,
    line_index: usize,
    package: Option<&str>,
    pid: Option<i32>,
) -> Vec<LiveEvent> {
    let mut events = observe_session(session_id, tag, body, line_index);
    if package.is_none() && pid.is_none() {
        return events;
    }
    for event in &mut events {
        if let LivePayload::Crash(crash) = &mut event.payload {
            if crash.process.is_none() {
                crash.process = package.map(str::to_owned);
            }
            if crash.pid.is_none() {
                crash.pid = pid;
            }
        }
    }
    // Keep the registry in step with what is returned: the same ids are replaced, so this
    // is idempotent rather than a second set of events.
    let mut guard = sessions().lock_ignore_poison();
    if let Some(entry) = guard.get_mut(session_id) {
        for event in &events {
            match entry.events.iter_mut().find(|known| known.id == event.id) {
                Some(existing) => *existing = event.clone(),
                None => {
                    if entry.events.len() < MAX_SESSION_EVENTS {
                        entry.events.push(event.clone());
                    }
                }
            }
        }
    }
    events
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn a_session_accumulates_then_can_be_taken_and_forgotten() {
        let id = "session-registry-1";
        let produced = observe_session(id, "am_crash", "[0,5555,com.example,10123,java.lang.RuntimeException,boom,Main.java,1]", 1);
        assert_eq!(produced.len(), 1);
        let taken = take_events(id);
        assert_eq!(taken.len(), 1);
        assert!(matches!(taken.first().map(|event| &event.payload), Some(LivePayload::Ams(_))));
        // Taking is destructive...
        assert!(take_events(id).is_empty());
        // ...and finishing forgets the session entirely.
        let _ = finish_session(id);
        assert!(take_events(id).is_empty());
    }

    #[test]
    fn a_growing_crash_block_replaces_itself_instead_of_piling_up() {
        let id = "session-registry-2";
        let _ = observe_session(id, "AndroidRuntime", "FATAL EXCEPTION: main", 10);
        let _ = observe_session(id, "AndroidRuntime", "Process: com.example, PID: 1234", 11);
        let _ = observe_session(id, "AndroidRuntime", "java.lang.NullPointerException: boom", 12);
        let taken = take_events(id);
        assert_eq!(taken.len(), 1, "one block, one entry: {taken:#?}");
        assert_eq!(taken.first().map(|event| event.id.as_str()), Some("crashBuffer#10"));
        // Finishing flushes the open block with its final shape.
        let finished = finish_session(id);
        assert!(finished.iter().any(|event| event.complete));
    }

    #[test]
    fn ordinary_records_never_create_a_session() {
        let id = "session-registry-3";
        for index in 0..1_000 {
            assert!(observe_session(id, "SurfaceFlinger", "nothing", index).is_empty());
        }
        assert!(take_events(id).is_empty());
        assert!(finish_session(id).is_empty());
    }
}
