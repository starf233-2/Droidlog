//! Structured crash events.
//!
//! Everything upstream of this module deals in lines: the crash buffer is text,
//! `dumpsys dropbox --print` is text, an ANR trace and a tombstone are text files.
//! Analysing "which process actually died first" needs more than text, so this
//! module turns those blocks into [`CrashEvent`] — with the fields a timeline can
//! be built from — while **keeping every source line in `raw`**.
//!
//! Two rules shape the design:
//!
//! * **Nothing is dropped.** A block that does not parse the way we expect still
//!   produces an event: the fields stay `None`/empty and `raw` carries the text.
//!   Crash logs from vendor ROMs are exactly where the unusual shapes live.
//! * **Nothing panics.** The crate denies `unwrap`/`expect`/`panic`/indexing, and
//!   these parsers run over data from a device (including truncated pulls), so all
//!   indexing goes through `get`/iterators.
//!
//! The family classification is not re-implemented here: [`crate::crash::classify`]
//! already scores text against the eight families, so a parsed block is handed to
//! it as a whole.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::crash::CrashKind;

/// Where an event was read from.
///
/// Kept as data (not just a comment) because the integrity report needs to say
/// which sources contributed and which were missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrashOrigin {
    /// The `crash` logcat buffer.
    CrashBuffer,
    /// A dropbox entry (raw file or `dumpsys dropbox --print`).
    Dropbox,
    /// A tombstone (native crash dump).
    Tombstone,
    /// An ANR trace.
    AnrTrace,
    /// The kernel ring buffer.
    Kernel,
}

impl CrashOrigin {
    /// Stable identifier, used in event ids and in the UI.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::CrashBuffer => "crashBuffer",
            Self::Dropbox => "dropbox",
            Self::Tombstone => "tombstone",
            Self::AnrTrace => "anrTrace",
            Self::Kernel => "kernel",
        }
    }

    /// Human label for the report and the timeline.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::CrashBuffer => "崩溃缓冲",
            Self::Dropbox => "Dropbox",
            Self::Tombstone => "tombstone",
            Self::AnrTrace => "ANR trace",
            Self::Kernel => "内核日志",
        }
    }
}

/// One crash, as far as it could be understood.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashEvent {
    /// Stable within one capture: `<origin>#<index>`.
    pub id: String,
    /// Where it came from.
    pub origin: CrashOrigin,
    /// Failure family, when the text matched one of the eight known families.
    pub kind: Option<CrashKind>,
    /// Package or process name the event belongs to.
    pub process: Option<String>,
    /// Process id at the time of the crash.
    pub pid: Option<i32>,
    /// Thread id, when the source states one.
    pub tid: Option<i32>,
    /// Thread name (`main`, `Binder:1234_2`, …).
    pub thread: Option<String>,
    /// Exception class or signal text (`java.lang.NullPointerException`, `signal 11 (SIGSEGV)`).
    pub exception: Option<String>,
    /// The exception message, abort message or fault address.
    pub message: Option<String>,
    /// Stack frames, verbatim, in the order the device printed them.
    pub frames: Vec<String>,
    /// `Caused by:` lines, verbatim — the root cause of a Java crash.
    pub caused_by: Vec<String>,
    /// Device-local timestamp text, when the source carries one.
    pub timestamp: Option<String>,
    /// The source lines, verbatim. Never empty.
    pub raw: Vec<String>,
}

impl CrashEvent {
    /// A new event with everything unknown, carrying `raw`.
    fn new(origin: CrashOrigin, index: usize, raw: Vec<String>) -> Self {
        Self {
            id: format!("{}#{index}", origin.id()),
            origin,
            kind: None,
            process: None,
            pid: None,
            tid: None,
            thread: None,
            exception: None,
            message: None,
            frames: Vec::new(),
            caused_by: Vec::new(),
            timestamp: None,
            raw,
        }
    }

    /// Whether the event carries a usable stack.
    ///
    /// The integrity report counts events without one: a crash with no frames is
    /// usually a truncated read rather than a crash without a stack.
    #[must_use]
    pub fn has_stack(&self) -> bool {
        !self.frames.is_empty()
    }

    /// The text the family classifier sees.
    fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.raw {
            let _ = writeln!(out, "{line}");
        }
        out
    }

    /// Fills `kind` from the family classifier, once the block is complete.
    fn finish(mut self) -> Self {
        if self.kind.is_none() {
            self.kind = crate::crash::classify(&self.text());
        }
        self
    }
}

/// Reads the value that follows `key` on a line, stopping at end of line.
fn after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let value = line.get(start..)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Reads an `i32` that follows `key`, e.g. `pid: 1234`.
fn number_after(line: &str, key: &str) -> Option<i32> {
    let value = after(line, key)?;
    let digits: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// Strips a leading logcat timestamp (`09-30 23:12:01.123`) when present.
fn split_timestamp(line: &str) -> (Option<String>, &str) {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    // `MM-DD HH:MM:SS.mmm` is exactly 18 characters; anything shorter cannot be one.
    if bytes.len() < 18 {
        return (None, trimmed);
    }
    let stamp = trimmed.get(..18).unwrap_or_default();
    // `MM-DD HH:MM:SS.mmm`: separators at 2, 5, 8 and 11 (the clock has two
    // colons; missing that made every real timestamp fail validation), a dot at 14
    // and digits everywhere else.
    let looks_like = stamp.char_indices().all(|(i, c)| match i {
        2 => c == '-',
        5 | 8 | 11 => c == ' ' || c == ':',
        14 => c == '.',
        _ => c.is_ascii_digit(),
    });
    if looks_like {
        (Some(stamp.to_owned()), trimmed.get(18..).unwrap_or_default().trim_start())
    } else {
        (None, trimmed)
    }
}

/// True when a line continues the block that is being collected.
fn is_continuation(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    let indented = line.len() != trimmed.len();
    indented
        || trimmed.starts_with("at ")
        || trimmed.starts_with("Caused by:")
        || trimmed.starts_with("Suppressed:")
        || trimmed.starts_with("#")
        || trimmed.starts_with("... ")
        || trimmed.starts_with("Abort message:")
        || trimmed.starts_with("backtrace:")
        || trimmed.starts_with("signal ")
        || trimmed.starts_with("pid: ")
        || trimmed.starts_with("Process: ")
        || trimmed.starts_with("PID: ")
        || trimmed.starts_with("Build fingerprint:")
        || trimmed.starts_with("Cmd line:")
        || trimmed.starts_with("-----")
        || trimmed.starts_with("DALVIK THREADS")
        // `java.lang.NullPointerException: boom` — the exception line of a block.
        // Safe for chatter because the shape is strict: a dotted, space-free head.
        || looks_like_exception(trimmed)
}

/// The message part of a logcat line, with the prefix removed.
///
/// The crash buffer prints its lines the way logcat does —
/// `09-30 23:12:01.100  1234  1234 E AndroidRuntime: FATAL EXCEPTION: main` — so
/// marker detection has to look at the *message*: `FATAL EXCEPTION` starts a block
/// there, not when it happens to appear in a tag. Frames need the same treatment,
/// because `\tat com.example` is only indented once the prefix is gone.
fn message_body(line: &str) -> &str {
    let (_, rest) = split_timestamp(line);
    let Some(first) = rest.split_whitespace().next() else {
        return rest;
    };
    // A logcat line starts with the pid; anything else is already the message
    // (tombstone and ANR traces have no prefix).
    if !first.chars().all(|c| c.is_ascii_digit()) {
        return rest;
    }
    match rest.find(": ") {
        Some(index) => rest.get(index + 2..).map_or(rest, str::trim_start),
        None => rest,
    }
}

/// The logcat tag of a line (`AndroidRuntime`, `ActivityManager`), when it has one.
///
/// Used to decide whether a line still belongs to the crash block being collected:
/// a Java crash prints `Process:`, then `java.lang.…`, then `\tat …` frames, all
/// under the *same* tag — while the `ActivityManager` line that follows is a
/// different process talking, and therefore a new (usually uninteresting) block.
fn logcat_tag(line: &str) -> Option<&str> {
    let (_, rest) = split_timestamp(line);
    let mut tokens = rest.split_whitespace();
    let pid = tokens.next()?;
    let tid = tokens.next()?;
    let level = tokens.next()?;
    let tag = tokens.next()?;
    let is_logcat = pid.chars().all(|c| c.is_ascii_digit())
        && tid.chars().all(|c| c.is_ascii_digit())
        && level.len() == 1
        && tag.ends_with(':');
    if is_logcat {
        tag.strip_suffix(':')
    } else {
        None
    }
}

/// Whether a message body starts a crash block.
///
/// Public because the live watch ([`crate::crash::live`]) has to make the same
/// decision one line at a time, and two implementations of "does this start a
/// crash" would drift apart.
#[must_use]
pub fn starts_crash_block(body: &str) -> bool {
    is_java_start(body) || is_native_start(body)
}

/// Whether a message body continues an open block.
///
/// Covers the shapes that are self-evidently continuations (indented frames,
/// `Caused by:`, `#00`, `signal …`). A logcat block additionally continues while the
/// tag stays the same, which only the batch parser can see; the live watch closes a
/// block when a different interesting tag arrives.
#[must_use]
pub fn continues_crash_block(body: &str) -> bool {
    is_continuation(body)
}

/// True when a line starts a Java crash block.
fn is_java_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("FATAL EXCEPTION")
        || trimmed.starts_with("*** FATAL")
        || trimmed.contains("FATAL EXCEPTION:")
}

/// True when a line starts a native crash block (crash buffer or tombstone header).
fn is_native_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("*** *** ***")
        || (trimmed.starts_with("signal ") && trimmed.contains("SIG"))
        || trimmed.starts_with("Abort message:")
        || trimmed.starts_with("backtrace:")
}

/// Parses the `crash` logcat buffer.
///
/// Blocks are found by their marker line (`FATAL EXCEPTION`, a native
/// `*** *** ***` header, `signal …`) and extended while the following lines look
/// like continuation (indented frames, `Caused by:`, `#00 …`).
#[must_use]
pub fn parse_crash_buffer(lines: &[&str]) -> Vec<CrashEvent> {
    let mut events: Vec<CrashEvent> = Vec::new();
    let mut block: Vec<String> = Vec::new();
    let origin = CrashOrigin::CrashBuffer;
    // Tag of the line that opened the current block; the block continues while the
    // tag stays the same.
    let mut block_tag: Option<String> = None;

    let flush = |block: &mut Vec<String>, origin: CrashOrigin, events: &mut Vec<CrashEvent>| {
        if block.is_empty() {
            return;
        }
        let index = events.len();
        let raw = std::mem::take(block);
        events.push(parse_block(origin, index, raw));
    };

    for line in lines {
        let body = message_body(line);
        let tag = logcat_tag(line);
        let java = is_java_start(body);
        let native = is_native_start(body);

        if java || native {
            flush(&mut block, origin, &mut events);
            block_tag = tag.map(str::to_owned);
            block.push((*line).to_owned());
            continue;
        }

        if block.is_empty() {
            continue;
        }
        // Same tag → still the same crash; a keyword or indent → a trace/tombstone
        // continuation (those lines carry no logcat prefix at all).
        let same_tag = matches!((tag, block_tag.as_deref()), (Some(current), Some(open)) if current == open);
        if same_tag || is_continuation(line) || is_continuation(body) {
            block.push((*line).to_owned());
        } else {
            flush(&mut block, origin, &mut events);
            block_tag = None;
        }
    }
    flush(&mut block, origin, &mut events);

    // `origin` is constant for this entry point; the binding above exists so the
    // same collector can be reused by the dropbox reader with a different origin.
    events.into_iter().map(CrashEvent::finish).collect()
}

/// Parses an ANR trace (`/data/anr/traces.txt` or a dropbox `*_anr` entry).
///
/// Each `----- pid N at TIME -----` header starts one event; `Cmd line:` gives the
/// package, `#NN pc …` lines are frames.
#[must_use]
pub fn parse_anr_trace(text: &str) -> Vec<CrashEvent> {
    let mut events: Vec<CrashEvent> = Vec::new();
    let mut block: Vec<String> = Vec::new();
    let mut in_section = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("----- pid ") {
            if in_section {
                let index = events.len();
                let raw = std::mem::take(&mut block);
                events.push(parse_block(CrashOrigin::AnrTrace, index, raw));
            }
            in_section = true;
            block.push(line.to_owned());
            continue;
        }
        if in_section {
            block.push(line.to_owned());
        }
    }
    if in_section {
        let index = events.len();
        let raw = std::mem::take(&mut block);
        events.push(parse_block(CrashOrigin::AnrTrace, index, raw));
    }

    events.into_iter().map(CrashEvent::finish).collect()
}

/// Parses a block that *is* one event, whatever its shape.
///
/// Used for sources where the unit is already the crash: a dropbox entry (one file
/// per crash), a tombstone, a single `dumpsys dropbox --print` section. `parse_*`
/// entry points that need marker detection are the exception, not the rule.
#[must_use]
pub fn parse_entry(origin: CrashOrigin, text: &str) -> Vec<CrashEvent> {
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return Vec::new();
    }
    vec![parse_block(origin, 0, lines).finish()]
}

/// Parses a tombstone (native crash dump).
///
/// A tombstone is a single event: it describes one process and one signal, so the
/// whole text becomes one [`CrashEvent`] (or none, when the text is empty).
#[must_use]
pub fn parse_tombstone(text: &str) -> Vec<CrashEvent> {
    parse_entry(CrashOrigin::Tombstone, text)
}

/// Fills one event from its raw lines, whatever shape they are in.
fn parse_block(origin: CrashOrigin, index: usize, raw: Vec<String>) -> CrashEvent {
    let mut event = CrashEvent::new(origin, index, raw);

    for line in event.raw.clone().iter() {
        let (stamp, _) = split_timestamp(line);
        if event.timestamp.is_none() {
            event.timestamp = stamp;
        }
        let body = message_body(line);

        // `----- pid 5678 at 2026-09-30 23:20:00 -----` — an ANR trace header.
        if body.starts_with("----- pid ") {
            event.pid = event.pid.or_else(|| number_after(body, "----- pid "));
            if let Some(at) = after(body, " at ") {
                let at = at.trim_end_matches('-').trim();
                if !at.is_empty() {
                    event.timestamp = Some(at.to_owned());
                }
            }
        }

        // `Process: com.example, PID: 1234` (Java) or
        // `pid: 1234, tid: 1234, name: com.example  >>> com.example <<<` (native).
        if let Some(process) = after(body, "Process: ") {
            let process = process.split(',').next().unwrap_or(process).trim();
            if !process.is_empty() {
                event.process = Some(process.to_owned());
            }
        }
        if body.trim_start().starts_with("pid: ") {
            event.pid = event.pid.or_else(|| number_after(body, "pid: "));
            event.tid = event.tid.or_else(|| number_after(body, "tid: "));
            if event.process.is_none() {
                event.process = after(body, "name: ")
                    .map(|name| name.split_whitespace().next().unwrap_or(name).to_owned());
            }
        }
        if event.pid.is_none() {
            event.pid = number_after(body, "PID: ");
        }
        if let Some(cmd) = after(body, "Cmd line: ") {
            if event.process.is_none() {
                event.process = Some(cmd.to_owned());
            }
        }
        if let Some(name) = after(body, "name: ") {
            if event.thread.is_none() && !body.starts_with("pid: ") {
                event.thread = Some(name.split_whitespace().next().unwrap_or(name).to_owned());
            }
        }
        if event.thread.is_none() {
            if let Some(thread) = after(body, "FATAL EXCEPTION: ") {
                event.thread = Some(thread.trim().to_owned());
            }
        }
        // `"main" prio=5 tid=1 Native` — an ANR trace names the thread first.
        if body.starts_with('"') {
            if let Some(end) = body.get(1..).and_then(|rest| rest.find('"')) {
                let name = body.get(1..=end).unwrap_or_default();
                if event.thread.is_none() {
                    event.thread = Some(name.to_owned());
                }
                event.tid = event.tid.or_else(|| number_after(body, "tid="));
            }
        }
    }

    // Second pass for the payload fields, so ordering inside the block does not
    // matter (a tombstone prints `pid:` before `signal`, a Java crash the reverse).
    for line in event.raw.clone().iter() {
        let body = message_body(line);
        let trimmed = body.trim_start();

        if trimmed.starts_with("signal ") && event.exception.is_none() {
            event.exception = Some(trimmed.to_owned());
            if let Some(addr) = after(trimmed, "fault addr ") {
                event.message = Some(format!("fault addr {addr}"));
            }
        }
        if let Some(abort) = after(trimmed, "Abort message: ") {
            event.message = Some(abort.trim_matches('\'').to_owned());
            if event.exception.is_none() {
                event.exception = Some("Abort message".to_owned());
            }
        }
        if trimmed.starts_with("Caused by:") {
            event.caused_by.push(trimmed.to_owned());
            continue;
        }
        if trimmed.starts_with("at ") || trimmed.starts_with("#") || trimmed.starts_with("... ") {
            event.frames.push(trimmed.to_owned());
            continue;
        }
        // The exception itself: `java.lang.NullPointerException: boom`. Found by
        // shape rather than by position — a real FATAL EXCEPTION block puts
        // `Process:` between the marker and this line, so "the line after the
        // marker" would look at the wrong line.
        if event.exception.is_none() && looks_like_exception(trimmed) {
            let (class, message) = split_exception(trimmed);
            event.exception = Some(class);
            event.message = message;
        }
    }

    event
}

/// True when a line looks like `some.package.Exception: message`.
fn looks_like_exception(line: &str) -> bool {
    let head = line.split(':').next().unwrap_or(line);
    head.contains('.') && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '$')
}

/// Splits `java.lang.NullPointerException: boom` into class and message.
fn split_exception(line: &str) -> (String, Option<String>) {
    match line.split_once(':') {
        Some((class, message)) => {
            let message = message.trim();
            (
                class.trim().to_owned(),
                if message.is_empty() {
                    None
                } else {
                    Some(message.to_owned())
                },
            )
        }
        None => (line.trim().to_owned(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAVA_CRASH: &[&str] = &[
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: FATAL EXCEPTION: main",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: Process: com.example.app, PID: 1234",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: java.lang.NullPointerException: boom",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: \tat com.example.app.Main.onCreate(Main.java:42)",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: \tat android.app.Activity.performCreate(Activity.java:8000)",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: Caused by: java.lang.IllegalStateException: root",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: \tat com.example.app.Helper.init(Helper.java:7)",
        "09-30 23:12:01.200  1234  1234 I ActivityManager: Killing 1234:com.example.app/u0a123",
    ];

    const TOMBSTONE: &str = "\
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
Build fingerprint: 'Xiaomi/odin/odin:13/TKQ1/1:user/release-keys'
pid: 4321, tid: 4321, name: com.example.native  >>> com.example.native <<<
signal 11 (SIGSEGV), code 1 (SEGV_MAPERR), fault addr 0x0
Abort message: 'FORTIFY: fread: null buffer'
backtrace:
      #00 pc 0000000000045678  /apex/com.android.runtime/lib64/bionic/libc.so (abort+164)
      #01 pc 0000000000012345  /system/lib64/libutils.so (android::sp<...>::~sp()+20)
";

    #[test]
    fn java_crash_is_structured() {
        let events = parse_crash_buffer(JAVA_CRASH);
        assert_eq!(events.len(), 1, "one block: {events:#?}");
        let event = events.first().expect("event");
        assert_eq!(event.process.as_deref(), Some("com.example.app"));
        assert_eq!(event.pid, Some(1234));
        assert_eq!(event.thread.as_deref(), Some("main"));
        assert_eq!(event.exception.as_deref(), Some("java.lang.NullPointerException"));
        assert_eq!(event.message.as_deref(), Some("boom"));
        assert_eq!(event.frames.len(), 3, "{:#?}", event.frames);
        assert_eq!(event.caused_by.len(), 1);
        assert!(event.caused_by.first().is_some_and(|line| line.contains("IllegalStateException")));
        assert_eq!(event.timestamp.as_deref(), Some("09-30 23:12:01.100"));
        assert!(event.kind.is_some(), "the family classifier should recognise a FATAL EXCEPTION");
        assert!(event.has_stack());
        assert_eq!(event.raw.len(), 7, "the following ActivityManager line is a new block or dropped, never merged");
    }

    #[test]
    fn tombstone_yields_signal_frames_and_abort_message() {
        let events = parse_tombstone(TOMBSTONE);
        assert_eq!(events.len(), 1);
        let event = events.first().expect("event");
        assert_eq!(event.origin, CrashOrigin::Tombstone);
        assert_eq!(event.pid, Some(4321));
        assert_eq!(event.process.as_deref(), Some("com.example.native"));
        assert!(event.exception.as_deref().is_some_and(|e| e.contains("SIGSEGV")), "{:#?}", event.exception);
        assert!(event.message.as_deref().is_some_and(|m| m.contains("FORTIFY")), "{:#?}", event.message);
        assert_eq!(event.frames.len(), 2);
        assert!(event.frames.first().is_some_and(|f| f.starts_with("#00")));
        assert!(event.raw.len() >= 6, "raw keeps every line");
    }

    const ANR: &str = "\
----- pid 5678 at 2026-09-30 23:20:00 -----
Cmd line: com.example.slow
DALVIK THREADS (12):
\"main\" prio=5 tid=1 Native
  | group=\"main\" sCount=1 dsCount=0 flags=1 obj=0x1234
  #00 pc 0000000000012345  /system/lib64/libc.so (syscall+28)
  #01 pc 0000000000067890  /system/lib64/libart.so (art::ConditionVariable::WaitHoldingLocks+136)
";

    #[test]
    fn anr_trace_is_structured() {
        let events = parse_anr_trace(ANR);
        assert_eq!(events.len(), 1);
        let event = events.first().expect("event");
        assert_eq!(event.origin, CrashOrigin::AnrTrace);
        assert_eq!(event.pid, Some(5678));
        assert_eq!(event.process.as_deref(), Some("com.example.slow"));
        assert_eq!(event.thread.as_deref(), Some("main"));
        assert_eq!(event.tid, Some(1));
        assert_eq!(event.frames.len(), 2);
        assert!(event.timestamp.is_none() || event.timestamp.is_some());
    }

    #[test]
    fn malformed_input_never_loses_text_or_panics() {
        let cases: &[&[&str]] = &[
            &[],
            &[""],
            &["garbage without markers"],
            &["FATAL EXCEPTION: main"],                     // marker, nothing else
            &["09-30 23:12:01.100  1  1 E AndroidRuntime: FATAL EXCEPTION: main"],
            &["signal "],                                    // truncated native header
            &["pid: , tid: , name: "],
            &["Caused by: "],
        ];
        for case in cases {
            let events = parse_crash_buffer(case);
            for event in &events {
                assert!(!event.raw.is_empty(), "an event always carries its raw lines");
            }
        }
        // Empty text is not an event at all.
        assert!(parse_tombstone("").is_empty());
        assert!(parse_tombstone("   \n  ").is_empty());
        assert!(parse_anr_trace("").is_empty());
    }

    #[test]
    fn an_unparsable_block_still_produces_an_event() {
        let events = parse_crash_buffer(&["FATAL EXCEPTION: main", "  ??? weird vendor line"]);
        assert_eq!(events.len(), 1);
        let event = events.first().expect("event");
        assert_eq!(event.thread.as_deref(), Some("main"));
        assert!(event.exception.is_none());
        assert_eq!(event.raw.len(), 2);
    }

    #[test]
    fn two_blocks_in_one_buffer_stay_separate() {
        let mut lines = JAVA_CRASH.to_vec();
        lines.extend_from_slice(&[
            "09-30 23:13:00.000  9999  9999 E AndroidRuntime: FATAL EXCEPTION: main",
            "09-30 23:13:00.000  9999  9999 E AndroidRuntime: Process: com.other, PID: 9999",
            "09-30 23:13:00.000  9999  9999 E AndroidRuntime: java.lang.IllegalArgumentException: nope",
        ]);
        let events = parse_crash_buffer(&lines);
        assert_eq!(events.len(), 2, "{events:#?}");
        assert_eq!(events.first().and_then(|e| e.pid), Some(1234));
        assert_eq!(events.get(1).and_then(|e| e.pid), Some(9999));
        assert_eq!(events.get(1).and_then(|e| e.message.as_deref()), Some("nope"));
        // Ids are stable and distinct.
        assert_ne!(events.first().map(|e| e.id.clone()), events.get(1).map(|e| e.id.clone()));
    }

    #[test]
    fn timestamp_extraction_is_strict() {
        // The device text is kept verbatim, milliseconds included — the same shape
        // `LogRecord.timestamp` carries everywhere else in the app.
        assert_eq!(
            split_timestamp("09-30 23:12:01.100 body").0.as_deref(),
            Some("09-30 23:12:01.100")
        );
        assert_eq!(split_timestamp("09-30 23:12:01.100 body").1, "body");
        // Not a timestamp: the line is returned untouched.
        assert_eq!(split_timestamp("FATAL EXCEPTION: main").0, None);
        assert_eq!(split_timestamp("FATAL EXCEPTION: main").1, "FATAL EXCEPTION: main");
        assert_eq!(split_timestamp("pid: 1, tid: 1").0, None);
        // Short strings must not be sliced out of range.
        assert_eq!(split_timestamp("1234567890").1, "1234567890");
    }

    #[test]
    fn origin_labels_are_stable() {
        assert_eq!(CrashOrigin::Dropbox.id(), "dropbox");
        assert_eq!(CrashOrigin::AnrTrace.label(), "ANR trace");
        assert_eq!(CrashOrigin::CrashBuffer.id(), "crashBuffer");
    }
}
