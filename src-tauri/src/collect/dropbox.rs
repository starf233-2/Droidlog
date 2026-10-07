//! Dropbox entries: the system's own crash records.
//!
//! When `system_server` or a system app dies, Android writes a record into
//! `/data/system/dropbox` before the process disappears. That record is often the
//! *only* place the real stack survives: logcat's crash buffer is a ring, and the
//! victim process is usually killed by ActivityManager moments later (which is the
//! whole problem this feature set exists for).
//!
//! Two ways in, and the code prefers whichever the device actually allows:
//!
//! * **no root** — `dumpsys dropbox --print`, which the shell user may read;
//! * **root** — read the files directly, where one file *is* one entry and the tag
//!   and time are in the file name (`system_app_crash@1696112345678.txt`).
//!
//! Both paths end in the same [`DropboxEntry`], whose body is handed to
//! [`crate::crash::structured`] so a dropbox crash and a logcat crash produce the
//! same [`CrashEvent`] shape — that sameness is what lets the timeline and the
//! correlation code treat them as one stream.

use serde::{Deserialize, Serialize};

use crate::crash::structured::{self, CrashEvent, CrashOrigin};

/// Which flavour of record an entry is.
///
/// Kept separate from [`crate::crash::CrashKind`] on purpose: the tag says *who*
/// recorded it (system app, system server, a data app, a native process), while the
/// family classifier says *what* went wrong. Both are needed — "system_server" plus
/// "watchdog" is a different story from "data app" plus "ANR".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DropboxKind {
    /// `system_app_crash` — a platform app died.
    SystemAppCrash,
    /// `system_server_crash` — the system server itself died.
    SystemServerCrash,
    /// `data_app_crash` — an installed app died.
    DataAppCrash,
    /// Any `*_anr` record.
    Anr,
    /// Any `*_native_crash` record.
    NativeCrash,
    /// `SYSTEM_TOMBSTONE` — the native tombstone written by debuggerd.
    Tombstone,
    /// `*_wtf` — a caught-but-logged failure.
    Wtf,
    /// `system_server_watchdog` — the watchdog killed the system server.
    Watchdog,
    /// Anything else the ROM records.
    Other,
}

impl DropboxKind {
    /// Classifies a dropbox tag.
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        let lower = tag.to_ascii_lowercase();
        if lower.contains("watchdog") {
            Self::Watchdog
        } else if lower.contains("tombstone") {
            Self::Tombstone
        } else if lower.ends_with("native_crash") || lower.contains("native_crash") {
            Self::NativeCrash
        } else if lower.ends_with("_anr") || lower.contains("_anr") {
            Self::Anr
        } else if lower.contains("system_server_crash") {
            Self::SystemServerCrash
        } else if lower.contains("system_app_crash") {
            Self::SystemAppCrash
        } else if lower.contains("data_app_crash") {
            Self::DataAppCrash
        } else if lower.ends_with("_wtf") || lower.contains("_wtf") {
            Self::Wtf
        } else {
            Self::Other
        }
    }

    /// Human label, used in the report and the timeline.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemAppCrash => "系统应用崩溃",
            Self::SystemServerCrash => "系统服务崩溃",
            Self::DataAppCrash => "应用崩溃",
            Self::Anr => "无响应（ANR）",
            Self::NativeCrash => "Native 崩溃",
            Self::Tombstone => "Tombstone",
            Self::Wtf => "WTF 记录",
            Self::Watchdog => "看门狗",
            Self::Other => "其它记录",
        }
    }

    /// Whether this kind usually has a stack worth showing.
    ///
    /// A `*_wtf` is frequently one line long; the integrity report uses this to
    /// avoid flagging "no stack" on records that never had one.
    #[must_use]
    pub fn usually_has_stack(self) -> bool {
        !matches!(self, Self::Wtf | Self::Other)
    }
}

/// One dropbox record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropboxEntry {
    /// Tag as the device spells it (`system_app_crash`).
    pub tag: String,
    /// What the tag means.
    pub kind: DropboxKind,
    /// Device-supplied time text, verbatim (epoch millis or `YYYY-MM-DD-HH-MM-SS`).
    pub timestamp: Option<String>,
    /// Byte count the header declared, when it did.
    pub size_bytes: Option<usize>,
    /// The parsed crash.
    pub event: CrashEvent,
}

/// True when a line is a `====…` separator.
fn is_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 8 && trimmed.chars().all(|c| c == '=')
}

/// Parses a dropbox header: `<date> <time> <tag> (<type>, <n> bytes)`.
///
/// Returns `(timestamp, tag, size)`. Tolerant by construction: ROMs vary in
/// spacing and in whether a size is printed at all, so anything that cannot be read
/// is simply `None`.
#[must_use]
pub fn parse_header(line: &str) -> Option<(Option<String>, String, Option<usize>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_separator(trimmed) {
        return None;
    }
    // `dumpsys dropbox` lists `2026-09-30 23:12:01 system_app_crash (text, 1234 bytes)`;
    // the enclosing parentheses hold the type and size.
    let (head, tail) = match trimmed.split_once('(') {
        Some((head, tail)) => (head.trim(), Some(tail)),
        None => (trimmed, None),
    };
    let mut parts = head.split_whitespace().collect::<Vec<_>>();
    let tag = parts.pop()?;
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let stamp = if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    };
    let size = tail.and_then(|tail| {
        let tail = tail.to_ascii_lowercase();
        let index = tail.find("bytes")?;
        let before = tail.get(..index)?.trim_end();
        let digits: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse().ok()
    });
    Some((stamp, tag.to_owned(), size))
}

/// Parses `dumpsys dropbox --print` output.
///
/// Every separator-delimited section becomes one entry; a section whose header
/// cannot be read is skipped rather than guessed at, because a wrong tag would put
/// a fake crash on the timeline.
#[must_use]
pub fn parse_dumpsys(output: &str) -> Vec<DropboxEntry> {
    parse_dumpsys_with_stats(output).0
}

/// [`parse_dumpsys`], plus how many records were seen but could not become an event.
///
/// A record with an empty body is a real thing — the file exists, the content was
/// rotated away or the write was cut short — and the integrity check needs to be
/// able to say "3 records, 1 empty" instead of silently reporting two.
#[must_use]
pub fn parse_dumpsys_with_stats(output: &str) -> (Vec<DropboxEntry>, usize) {
    let mut entries = Vec::new();
    let mut skipped = 0_usize;
    let mut header: Option<(Option<String>, String, Option<usize>)> = None;
    let mut body: Vec<String> = Vec::new();

    let flush = |header: &mut Option<(Option<String>, String, Option<usize>)>,
                 body: &mut Vec<String>,
                 entries: &mut Vec<DropboxEntry>,
                 skipped: &mut usize| {
        let Some((timestamp, tag, size_bytes)) = header.take() else {
            body.clear();
            return;
        };
        let text = std::mem::take(body).join("\n");
        let kind = DropboxKind::from_tag(&tag);
        match structured::parse_entry(CrashOrigin::Dropbox, &text).into_iter().next() {
            Some(event) => entries.push(DropboxEntry {
                tag,
                kind,
                timestamp,
                size_bytes,
                event,
            }),
            // The header was readable but the body was not: counted, not invented.
            None => *skipped += 1,
        }
    };

    for line in output.lines() {
        if is_separator(line) {
            flush(&mut header, &mut body, &mut entries, &mut skipped);
            continue;
        }
        if header.is_none() {
            // Between the separator and the body sits exactly one header line.
            if let Some(parsed) = parse_header(line) {
                header = Some(parsed);
            }
            continue;
        }
        body.push(line.to_owned());
    }
    flush(&mut header, &mut body, &mut entries, &mut skipped);

    (entries, skipped)
}

/// Splits a dropbox file name into `(timestamp, tag)`.
///
/// `system_app_crash@1696112345678.txt` and `data_app_anr@2026-09-30-23-12-01.txt`
/// are both in the wild; the time is kept as text either way.
#[must_use]
pub fn parse_file_name(name: &str) -> Option<(Option<String>, String)> {
    let stem = name.strip_suffix(".txt").unwrap_or(name);
    let (tag, stamp) = match stem.split_once('@') {
        Some((tag, stamp)) => (tag, Some(stamp)),
        None => (stem, None),
    };
    let tag = tag.trim();
    // A tag is a machine name: anything with spaces or punctuation is a different
    // kind of file that happens to sit in the same directory.
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((stamp.map(str::to_owned), tag.to_owned()))
}

/// Turns file contents read from `/data/system/dropbox` into entries.
///
/// `files` is `(name, contents)` — the names carry the tag and time, the bodies the
/// crash. Files whose name cannot be understood are skipped, and the count of those
/// is returned so the report can say so instead of silently dropping them.
#[must_use]
pub fn entries_from_files<'a, I>(files: I) -> (Vec<DropboxEntry>, usize)
where
    I: IntoIterator<Item = (&'a str, String)>,
{
    let mut entries = Vec::new();
    let mut skipped = 0_usize;
    for (name, contents) in files {
        let Some((timestamp, tag)) = parse_file_name(name) else {
            skipped += 1;
            continue;
        };
        let kind = DropboxKind::from_tag(&tag);
        let events = structured::parse_entry(CrashOrigin::Dropbox, &contents);
        let Some(event) = events.into_iter().next() else {
            skipped += 1;
            continue;
        };
        entries.push(DropboxEntry {
            tag,
            kind,
            timestamp,
            size_bytes: Some(contents.len()),
            event,
        });
    }
    (entries, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMPSYS: &str = "\
Drop box contents: 3 entries
===========================================================
2026-09-30 23:12:01 system_app_crash (text, 1234 bytes)
Process: com.android.settings
PID: 1234
java.lang.NullPointerException: boom
\tat com.android.settings.Main.onCreate(Main.java:42)
\tat android.app.Activity.performCreate(Activity.java:8000)
Caused by: java.lang.IllegalStateException: root cause
\tat com.android.settings.Helper.init(Helper.java:7)
===========================================================
2026-09-30 23:13:00 system_server_watchdog (text, 99 bytes)
Watchdog killing system process
===========================================================
2026-09-30 23:14:00 data_app_anr (text, 555 bytes)
----- pid 5678 at 2026-09-30 23:14:00 -----
Cmd line: com.example.slow
\"main\" prio=5 tid=1 Native
  #00 pc 0000000000012345  /system/lib64/libc.so (syscall+28)
";

    #[test]
    fn dumpsys_entries_are_parsed_with_their_tags() {
        let entries = parse_dumpsys(DUMPSYS);
        assert_eq!(entries.len(), 3, "{entries:#?}");

        let first = entries.first().expect("first");
        assert_eq!(first.tag, "system_app_crash");
        assert_eq!(first.kind, DropboxKind::SystemAppCrash);
        assert_eq!(first.timestamp.as_deref(), Some("2026-09-30 23:12:01"));
        assert_eq!(first.size_bytes, Some(1234));
        assert_eq!(first.event.pid, Some(1234));
        assert_eq!(first.event.process.as_deref(), Some("com.android.settings"));
        assert_eq!(
            first.event.exception.as_deref(),
            Some("java.lang.NullPointerException")
        );
        assert_eq!(first.event.frames.len(), 3, "two frames plus the one under Caused by");
        assert_eq!(first.event.caused_by.len(), 1);

        let watchdog = entries.get(1).expect("second");
        assert_eq!(watchdog.kind, DropboxKind::Watchdog);
        assert_eq!(watchdog.event.raw.len(), 1);

        let anr = entries.get(2).expect("third");
        assert_eq!(anr.kind, DropboxKind::Anr);
        assert_eq!(anr.event.pid, Some(5678));
        assert_eq!(anr.event.frames.len(), 1);
    }

    #[test]
    fn kinds_are_classified_from_tags() {
        assert_eq!(DropboxKind::from_tag("system_server_crash"), DropboxKind::SystemServerCrash);
        assert_eq!(DropboxKind::from_tag("data_app_crash"), DropboxKind::DataAppCrash);
        assert_eq!(DropboxKind::from_tag("data_app_native_crash"), DropboxKind::NativeCrash);
        assert_eq!(DropboxKind::from_tag("system_app_anr"), DropboxKind::Anr);
        assert_eq!(DropboxKind::from_tag("SYSTEM_TOMBSTONE"), DropboxKind::Tombstone);
        assert_eq!(DropboxKind::from_tag("system_app_wtf"), DropboxKind::Wtf);
        assert_eq!(DropboxKind::from_tag("system_server_watchdog"), DropboxKind::Watchdog);
        assert_eq!(DropboxKind::from_tag("vendor_mystery"), DropboxKind::Other);
        // A watchdog tag that also mentions crash stays a watchdog: the tag is the
        // authority, and "watchdog" is the more specific story.
        assert_eq!(DropboxKind::from_tag("system_server_watchdog_crash"), DropboxKind::Watchdog);
        assert!(DropboxKind::Anr.usually_has_stack());
        assert!(!DropboxKind::Wtf.usually_has_stack());
    }

    #[test]
    fn file_names_carry_tag_and_time() {
        assert_eq!(
            parse_file_name("system_app_crash@1696112345678.txt"),
            Some((Some("1696112345678".to_owned()), "system_app_crash".to_owned()))
        );
        assert_eq!(
            parse_file_name("data_app_anr@2026-09-30-23-12-01.txt"),
            Some((Some("2026-09-30-23-12-01".to_owned()), "data_app_anr".to_owned()))
        );
        assert_eq!(
            parse_file_name("SYSTEM_TOMBSTONE"),
            Some((None, "SYSTEM_TOMBSTONE".to_owned()))
        );
        assert_eq!(parse_file_name(".txt"), None);
        assert_eq!(parse_file_name(""), None);
    }

    #[test]
    fn files_become_entries_and_bad_names_are_counted() {
        let files = vec![
            (
                "system_server_crash@1696112345678.txt",
                "Process: system_server\nPID: 999\njava.lang.RuntimeException: dead\n\tat com.android.server.SystemServer.run(SystemServer.java:1)\n".to_owned(),
            ),
            ("not a dropbox file", "junk".to_owned()),
            ("empty@1.txt", "   \n".to_owned()),
        ];
        let (entries, skipped) = entries_from_files(files);
        assert_eq!(entries.len(), 1);
        assert_eq!(skipped, 2);
        let entry = entries.first().expect("entry");
        assert_eq!(entry.kind, DropboxKind::SystemServerCrash);
        assert_eq!(entry.timestamp.as_deref(), Some("1696112345678"));
        assert_eq!(entry.event.pid, Some(999));
        assert_eq!(entry.event.frames.len(), 1);
    }

    #[test]
    fn malformed_input_is_tolerated() {
        // No separator at all: nothing is invented.
        assert!(parse_dumpsys("just some text").is_empty());
        // Separators only.
        assert!(parse_dumpsys("=====\n=====\n").is_empty());
        // A header whose body never arrived is *counted*, not turned into an empty
        // event: an event always carries raw lines, and a record with no contents is
        // something the integrity report has to be able to state.
        let (entries, skipped) =
            parse_dumpsys_with_stats("==========\n2026-09-30 23:12:01 data_app_crash (text, 1 bytes)\n");
        assert!(entries.is_empty());
        assert_eq!(skipped, 1);
        // Garbage header lines are skipped rather than guessed.
        assert!(parse_header("=================================").is_none());
        assert!(parse_header("").is_none());
        assert!(parse_header("   ").is_none());
        // A header with no size still works.
        let parsed = parse_header("2026-09-30 23:12:01 system_app_crash (text)");
        assert_eq!(parsed, Some((Some("2026-09-30 23:12:01".to_owned()), "system_app_crash".to_owned(), None)));
        // ...and one with no date at all.
        let parsed = parse_header("data_app_crash (text, 10 bytes)");
        assert_eq!(parsed, Some((None, "data_app_crash".to_owned(), Some(10))));
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(DropboxKind::SystemServerCrash.label(), "系统服务崩溃");
        assert_eq!(DropboxKind::NativeCrash.label(), "Native 崩溃");
    }
}

/// What a probe read, judged by its shape.
///
/// The collector hands over text without saying what it is: a dropbox file, an ANR
/// trace and a tombstone all arrive the same way. Deciding here — in one tested
/// function — keeps the collector free of that judgement, and keeps the decision
/// visible instead of buried in a probe table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeTextKind {
    /// An ANR trace section (`----- pid N at … -----`).
    AnrTrace,
    /// A native tombstone (`*** *** ***` header or a `signal 11 (SIGSEGV)` line).
    Tombstone,
    /// A dropbox entry, or anything else that is one crash in one text.
    Dropbox,
    /// Nothing to report: empty or whitespace only.
    Empty,
}

/// Classifies a probe read.
#[must_use]
pub fn classify_probe_text(text: &str) -> ProbeTextKind {
    if text.trim().is_empty() {
        return ProbeTextKind::Empty;
    }
    if text.contains("----- pid ") {
        return ProbeTextKind::AnrTrace;
    }
    if text.contains("*** *** ***") || text.contains("signal ") && text.contains("(SIG") {
        return ProbeTextKind::Tombstone;
    }
    ProbeTextKind::Dropbox
}

/// Splits a directory read's `== <file name>` header from its body.
///
/// The collector writes that header before every file it reads from a directory probe, so
/// that the file name — which for Dropbox carries the tag and the time
/// (`system_app_crash@1696112345678.txt`) — is not lost the way `cat *` loses it. This is
/// what turns that header back into information.
#[must_use]
fn split_file_header(text: &str) -> Option<(&str, &str)> {
    let mut lines = text.lines();
    let head = lines.next()?;
    let name = head.strip_prefix("== ")?.trim();
    if name.is_empty() || name.contains(' ') {
        // Not a file header: a tombstone line could start with `== ` on a strange ROM, and
        // a name with spaces would not be one either.
        return None;
    }
    // The body keeps every remaining line, including its newlines.
    let body_start = head.len() + 1;
    Some((name, text.get(body_start..).unwrap_or_default()))
}

/// Turns a probe read into unified crash events.
///
/// One text can yield several events (an ANR trace holds one section per process), and
/// the origin is kept so the report can say where a crash came from. An unreadable text
/// still yields one event carrying its raw lines rather than nothing — the same rule the
/// rest of the crash code follows.
///
/// A `== <file name>` header is honoured first: it identifies a Dropbox entry, whose tag
/// and time live in the *name*, and losing them is losing the only thing that says which
/// system component wrote the crash and when.
#[must_use]
pub fn events_from_probe_text(text: &str) -> Vec<CrashEvent> {
    if let Some((name, body)) = split_file_header(text) {
        // The *body* decides which parser owns the file: a tombstone sitting in a directory
        // listing carries a name header too, and parsing it as a Dropbox entry would put the
        // wrong origin on it. Only a body that reads as a Dropbox entry goes down the path
        // that also harvests the tag and time from the name.
        let mut events = match classify_probe_text(body) {
            ProbeTextKind::Empty => Vec::new(),
            ProbeTextKind::Dropbox => {
                let (entries, _skipped) =
                    entries_from_files(std::iter::once((name, body.to_owned())));
                entries.into_iter().map(|entry| entry.event).collect()
            }
            ProbeTextKind::AnrTrace => structured::parse_anr_trace(body),
            ProbeTextKind::Tombstone => structured::parse_tombstone(body),
        };
        if events.is_empty() {
            // Nothing parsed out of a named file: keep one event carrying the raw lines, so
            // the report can show what was found instead of claiming nothing was there.
            events = structured::parse_entry(CrashOrigin::Dropbox, body);
        }
        let (timestamp, tag) = match parse_file_name(name) {
            Some((timestamp, tag)) => (timestamp, Some(tag)),
            None => (None, None),
        };
        for event in &mut events {
            if event.timestamp.is_none() {
                event.timestamp = timestamp.clone();
            }
            // The name is evidence: it carries the Dropbox tag and time
            // (`system_app_crash@1696112345678.txt`), which is the difference between "some
            // crash" and "the system app crashed, then". It goes first so the report shows it
            // before the body.
            let head = match &tag {
                Some(tag) => format!("[{tag}] {name}"),
                None => format!("[{}] {name}", event.origin.id()),
            };
            event.raw.insert(0, head);
        }
        return events;
    }
    events_from_body(text)
}

/// Classifies and parses a probe read that carries no file header.
#[must_use]
fn events_from_body(text: &str) -> Vec<CrashEvent> {
    match classify_probe_text(text) {
        ProbeTextKind::Empty => Vec::new(),
        ProbeTextKind::AnrTrace => structured::parse_anr_trace(text),
        ProbeTextKind::Tombstone => structured::parse_tombstone(text),
        ProbeTextKind::Dropbox => structured::parse_entry(CrashOrigin::Dropbox, text),
    }
}

#[cfg(test)]
mod probe_text_tests {
    use super::*;

    #[test]
    fn shapes_are_classified() {
        assert_eq!(classify_probe_text(""), ProbeTextKind::Empty);
        assert_eq!(classify_probe_text("   \n \t"), ProbeTextKind::Empty);
        assert_eq!(
            classify_probe_text("----- pid 5678 at 2026-09-30 23:20:00 -----\nCmd line: com.example\n"),
            ProbeTextKind::AnrTrace
        );
        assert_eq!(
            classify_probe_text("*** *** *** *** ***\npid: 1, tid: 1, name: com.example\n"),
            ProbeTextKind::Tombstone
        );
        assert_eq!(
            classify_probe_text("pid: 1, tid: 1, name: com.example\nsignal 11 (SIGSEGV), code 1\n"),
            ProbeTextKind::Tombstone
        );
        // A dropbox entry usually has neither marker.
        assert_eq!(
            classify_probe_text("Process: com.example, PID: 7\njava.lang.RuntimeException: boom\n"),
            ProbeTextKind::Dropbox
        );
    }

    #[test]
    fn each_shape_becomes_events_with_its_origin() {
        let dropbox = "Process: com.android.settings, PID: 1234\njava.lang.NullPointerException: boom\n\tat com.android.settings.Main.onCreate(Main.java:42)\n";
        let events = events_from_probe_text(dropbox);
        assert_eq!(events.len(), 1);
        assert_eq!(events.first().map(|event| event.origin), Some(CrashOrigin::Dropbox));
        assert_eq!(events.first().and_then(|event| event.pid), Some(1234));
        assert!(events.first().is_some_and(CrashEvent::has_stack));

        let anr = "----- pid 5678 at 2026-09-30 23:20:00 -----\nCmd line: com.example.slow\n\"main\" prio=5 tid=1 Native\n  #00 pc 0000000000012345  /system/lib64/libc.so (syscall+28)\n";
        let events = events_from_probe_text(anr);
        assert_eq!(events.len(), 1);
        assert_eq!(events.first().map(|event| event.origin), Some(CrashOrigin::AnrTrace));

        let tombstone = "*** *** *** *** ***\npid: 4321, tid: 4321, name: com.example.native\nsignal 11 (SIGSEGV), fault addr 0x0\nbacktrace:\n      #00 pc 0000000000045678  /apex/libc.so (abort+164)\n";
        let events = events_from_probe_text(tombstone);
        assert_eq!(events.len(), 1);
        assert_eq!(events.first().map(|event| event.origin), Some(CrashOrigin::Tombstone));
        assert!(events.first().is_some_and(CrashEvent::has_stack));
    }

    #[test]
    fn empty_reads_produce_nothing_and_junk_still_produces_an_event() {
        assert!(events_from_probe_text("").is_empty());
        assert!(events_from_probe_text("\n\n   \n").is_empty());
        let junk = events_from_probe_text("com.example.vendor.opaque 0x1234\n");
        assert_eq!(junk.len(), 1, "an unreadable read still carries its raw lines");
        assert_eq!(junk.first().map(|event| event.raw.len()), Some(1));
    }

    /// The collector writes `== <file name>` before every file a directory probe reads, and
    /// for Dropbox that name carries the tag and the time. Losing them was the whole reason
    /// `cat *` was wrong: without the name, a crash has no author and no moment.
    #[test]
    fn a_file_header_supplies_the_dropbox_tag_and_time() {
        let read = "== system_app_crash@1696112345678.txt\nProcess: com.android.systemui, PID: 1234\njava.lang.RuntimeException: boom\n\tat com.android.systemui.Main.run(Main.java:1)\n";
        let events = events_from_probe_text(read);
        assert_eq!(events.len(), 1, "{events:#?}");
        let event = events.first().expect("event");
        assert_eq!(event.origin, CrashOrigin::Dropbox);
        assert_eq!(event.timestamp.as_deref(), Some("1696112345678"));
        assert_eq!(event.raw.first().map(String::as_str), Some("[system_app_crash] system_app_crash@1696112345678.txt"));
        // The body still parsed: the header must not shadow the crash itself.
        assert_eq!(event.pid, Some(1234));
        assert!(event.has_stack());
    }

    /// A tombstone in a directory listing has a name header too, and its body — not the
    /// header — decides which parser owns it. Treating it as a Dropbox entry would put the
    /// wrong origin on a native crash.
    #[test]
    fn a_named_tombstone_keeps_its_origin_and_still_records_its_name() {
        let read = "== tombstone_00\n*** *** *** *** ***\npid: 4321, tid: 4321, name: com.example.native\nsignal 11 (SIGSEGV), fault addr 0x0\nbacktrace:\n      #00 pc 0000000000045678  /apex/libc.so (abort+164)\n";
        let events = events_from_probe_text(read);
        assert_eq!(events.len(), 1, "{events:#?}");
        let event = events.first().expect("event");
        assert_eq!(event.origin, CrashOrigin::Tombstone);
        assert_eq!(event.raw.first().map(String::as_str), Some("[tombstone_00] tombstone_00"));
    }

    /// Malformed headers must fall back to plain classification rather than eating the text.
    #[test]
    fn a_broken_or_absent_header_is_harmless() {
        // No header at all.
        let plain = events_from_probe_text("Process: com.example, PID: 7\njava.lang.RuntimeException: x\n");
        assert_eq!(plain.len(), 1);
        assert_eq!(plain.first().map(|event| event.origin), Some(CrashOrigin::Dropbox));
        // A header that is not a header (`==` alone, then a name with spaces).
        let odd = events_from_probe_text("== \nProcess: com.example, PID: 7\njava.lang.RuntimeException: x\n");
        assert_eq!(odd.len(), 1);
        assert_eq!(odd.first().map(|event| event.origin), Some(CrashOrigin::Dropbox));
        let spaced = events_from_probe_text("== not a file name\nProcess: com.example, PID: 7\n");
        assert_eq!(spaced.len(), 1);
        // An empty body behind a valid header still yields nothing to report.
        assert!(events_from_probe_text("== system_app_crash@1.txt\n").is_empty());
    }
}
