//! One call that turns a capture's raw material into the forensics result.
//!
//! The modules before this one each answer a narrow question — what crashed
//! (`structured`), what the dropbox recorded (`dropbox`), who was killed with it
//! (`ams`), which process it all belongs to (`correlate`), what pressure preceded it
//! (`resources`), and how much can be trusted (`integrity`). Each is tested alone and
//! each is useless alone: a caller that has to know the order, the types and the
//! windows will eventually wire them up differently from the tests that cover them.
//!
//! So this module is the only place that knows the pipeline, and the frontend
//! consumes [`Forensics`] and nothing else. Putting the order in one function is what
//! makes the order changeable.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};

use crate::crash::ams::{self, AmsSignal, CausalLink, LinkOptions};
use crate::crash::correlate::{self, Correlation};
use crate::crash::integrity::{self, CheckStatus, IntegrityCheck, IntegrityInput, Limits};
use crate::crash::resources::{self, ResourceAnomaly, ResourceAttachment};
use crate::crash::structured::CrashEvent;

/// Everything the analysis reads.
///
/// Lines are borrowed so a capture keeps ownership of its buffers — it may hold
/// hundreds of thousands of them.
pub struct ForensicsInput<'a> {
    /// Structured crashes, from any source.
    pub events: &'a [CrashEvent],
    /// AMS / ActivityManager lines, raw.
    pub ams_lines: &'a [&'a str],
    /// Resource lines, raw: lmkd, allocator, fdsan, Binder.
    pub resource_lines: &'a [&'a str],
    /// Counters and configuration for the integrity checks.
    pub integrity: IntegrityInput,
}

/// The assembled result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Forensics {
    /// Every AMS signal found, in order.
    pub signals: Vec<AmsSignal>,
    /// `source → victim` links, with evidence line numbers.
    pub links: Vec<CausalLink>,
    /// Every resource anomaly found, in order.
    pub anomalies: Vec<ResourceAnomaly>,
    /// One story per process identity.
    pub correlation: Correlation,
    /// Anomalies grouped under the story they explain.
    pub anomalies_by_story: ResourceAttachment,
    /// Integrity findings, worst first.
    pub checks: Vec<IntegrityCheck>,
    /// Crashes that matched a known signature, with their cause instead of a stack.
    pub known: Vec<crate::crash::known::KnownNote>,
}

impl Forensics {
    /// Findings that suggest the capture may mislead — the report's badge.
    #[must_use]
    pub fn warnings(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Warning)
            .count()
    }

    /// Total crashes across every story.
    #[must_use]
    pub fn crash_count(&self) -> usize {
        self.correlation
            .stories
            .iter()
            .map(|story| story.crashes.len())
            .sum()
    }

    /// One-line summary for the report header.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} 个进程、{} 次崩溃、{} 条连带关系、{} 项资源异常、{} 项完整性告警",
            self.correlation.stories.len(),
            self.crash_count(),
            self.links.len(),
            self.anomalies.len(),
            self.warnings()
        )
    }
}

/// Runs the whole pipeline.
///
/// `limits` are the integrity thresholds and `windows` bound the causal links; both
/// are parameters so a test can pin a scenario down, and so the UI can tighten them
/// later without touching this order.
#[must_use]
pub fn analyse(input: &ForensicsInput<'_>, limits: &Limits, windows: &LinkOptions) -> Forensics {
    let signals = ams::parse_signals(input.ams_lines);
    let links = ams::link_deaths(&signals, windows);
    let anomalies = resources::parse_signals(input.resource_lines);
    // Correlation runs *after* the links exist: a story lists the victims its identity
    // caused, so the links have to be known first.
    let correlation = correlate::correlate(input.events, &signals, &links);
    let anomalies_by_story = resources::attach(&anomalies, &correlation.stories);
    let mut checks = integrity::assess(&input.integrity, limits);
    // Worst first: a warning about dropped records matters more than "10000 lines
    // received", and the report must not bury it.
    checks.sort_by_key(|check| Reverse(check.status));

    Forensics {
        signals,
        links,
        anomalies,
        correlation,
        anomalies_by_story,
        checks,
        known: crate::crash::known::notes_for(input.events),
    }
}

/// The integrity checks for a finished session, ready for the report.
///
/// This is the report's side of item 8: the counters the reader accumulated
/// ([`crate::crash::session`]) plus what the capture knows about its buffers and its
/// crashes, turned into the findings the report shows. A session that never counted
/// still gets the six checks — "no logs received" is a finding, not a gap.
#[must_use]
pub fn session_checks(
    session_id: &str,
    requested_buffers: Vec<String>,
    available_buffers: Vec<String>,
    crashes: Vec<crate::crash::structured::CrashEvent>,
) -> Vec<IntegrityCheck> {
    integrity::checks_for_session(
        crate::crash::session::counters(session_id),
        requested_buffers,
        available_buffers,
        crashes,
        &Limits::default(),
    )
}

/// [`session_checks`] with the counters supplied by the caller.
///
/// Used where the caller already holds the counters — the command layer merges the
/// ring's own drop counter into them before asking, and taking a fresh copy here would
/// throw that away.
#[must_use]
pub fn session_checks_with(
    counters: Option<crate::crash::integrity::CaptureCounters>,
    requested_buffers: Vec<String>,
    available_buffers: Vec<String>,
    crashes: Vec<crate::crash::structured::CrashEvent>,
) -> Vec<IntegrityCheck> {
    integrity::checks_for_session(
        counters,
        requested_buffers,
        available_buffers,
        crashes,
        &Limits::default(),
    )
}

#[cfg(test)]
mod probe_contract_tests {
    use super::*;
    use crate::crash::integrity::IntegrityInput;
    use crate::crash::structured::{self, CrashOrigin};

    /// A dropbox entry exactly as the probe reads it from disk.
    const DROPBOX: &str = "Process: com.android.settings, PID: 1234\njava.lang.NullPointerException: boom\n\tat com.android.settings.Main.onCreate(Main.java:42)\n\tat android.app.Activity.performCreate(Activity.java:8000)\n";
    /// `/data/anr/traces.txt`, one section.
    const ANR: &str = "----- pid 5678 at 2026-09-30 23:20:00 -----\nCmd line: com.example.slow\n\"main\" prio=5 tid=1 Native\n  #00 pc 0000000000012345  /system/lib64/libc.so (syscall+28)\n";
    /// A tombstone file.
    const TOMBSTONE: &str = "*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***\npid: 4321, tid: 4321, name: com.example.native  >>> com.example.native <<<\nsignal 11 (SIGSEGV), code 1 (SEGV_MAPERR), fault addr 0x0\nAbort message: 'FORTIFY: fread: null buffer'\nbacktrace:\n      #00 pc 0000000000045678  /apex/com.android.runtime/lib64/bionic/libc.so (abort+164)\n";

    /// The three probe outputs must become **one** stream of the same shape — that is
    /// what lets the timeline, the correlation and the report treat a dropbox record, an
    /// ANR trace and a tombstone as the same kind of thing.
    #[test]
    fn probe_outputs_become_one_event_stream() {
        let mut events = Vec::new();
        events.extend(structured::parse_entry(CrashOrigin::Dropbox, DROPBOX));
        events.extend(structured::parse_anr_trace(ANR));
        events.extend(structured::parse_tombstone(TOMBSTONE));
        assert_eq!(events.len(), 3, "{events:#?}");
        assert!(events.iter().all(|event| event.has_stack()), "every source carries a stack");
        // The origin survives, so the report can say where a crash came from.
        let origins: Vec<CrashOrigin> = events.iter().map(|event| event.origin).collect();
        assert!(origins.contains(&CrashOrigin::Dropbox));
        assert!(origins.contains(&CrashOrigin::AnrTrace));
        assert!(origins.contains(&CrashOrigin::Tombstone));

        // And the whole pipeline accepts them without a special case.
        let result = analyse(
            &ForensicsInput {
                events: &events,
                ams_lines: &[],
                resource_lines: &[],
                integrity: IntegrityInput {
                    crashes: events.clone(),
                    ..IntegrityInput::default()
                },
            },
            &Limits::default(),
            &LinkOptions::default(),
        );
        assert_eq!(result.crash_count(), 3, "one story per process: {:#?}", result.correlation.stories);
        assert_eq!(result.correlation.unlinked_events, 0, "each event names its process");
        assert_eq!(result.warnings(), 0, "all three carry stacks: {:#?}", result.checks);
        let stacks = result
            .checks
            .iter()
            .find(|check| check.id == "stacks")
            .expect("stacks check");
        assert!(stacks.detail.contains('3'), "{}", stacks.detail);
    }

    /// A probe read that was cut short must not be reported as a clean capture: the
    /// event still exists, and the report says the stack is missing.
    #[test]
    fn a_truncated_probe_read_is_reported_as_a_missing_stack() {
        let truncated = structured::parse_entry(CrashOrigin::Dropbox, "Process: com.example, PID: 7\n");
        assert_eq!(truncated.len(), 1);
        assert!(!truncated.first().is_some_and(|event| event.has_stack()));
        let result = analyse(
            &ForensicsInput {
                events: &truncated,
                ams_lines: &[],
                resource_lines: &[],
                integrity: IntegrityInput {
                    crashes: truncated.clone(),
                    ..IntegrityInput::default()
                },
            },
            &Limits::default(),
            &LinkOptions::default(),
        );
        assert_eq!(
            result.checks.first().map(|check| check.status),
            Some(crate::crash::integrity::CheckStatus::Warning),
            "a stackless crash leads the report: {:#?}",
            result.checks
        );
    }
}

/// A forensics result together with the map back to capture sequence numbers.
///
/// [`analyse`] numbers the lines it is given from zero, because it only ever sees text.
/// The view has to jump to the *capture's* rows, so the map travels with the result:
/// `seq_at[i]` is the sequence number of the i-th line handed to the analyser. Without
/// it every evidence line number in `links` and `anomalies` would point at the wrong row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForensicsView {
    /// The analysis itself: signals, links, anomalies, stories and checks.
    pub analysis: Forensics,
    /// Capture sequence number for each analysed line, in the order they were passed.
    pub seq_at: Vec<u64>,
    /// A capture-level remark the analysis cannot make on its own — today, whether the
    /// rows carry more than one date, which means older logs came along with this capture.
    pub notice: Option<String>,
}

impl ForensicsView {
    /// Translates an analyser line index into a capture sequence number.
    #[must_use]
    pub fn seq_of(&self, line_index: usize) -> Option<u64> {
        self.seq_at.get(line_index).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::structured::{self, CrashOrigin};

    fn crash(pid: i32, process: &str) -> CrashEvent {
        let text = format!(
            "FATAL EXCEPTION: main\nProcess: {process}, PID: {pid}\njava.lang.RuntimeException: boom\n\tat {process}.Main.onCreate(Main.java:1)\n"
        );
        structured::parse_entry(CrashOrigin::CrashBuffer, &text)
            .into_iter()
            .next()
            .expect("event")
    }

    /// The same lines the `ams` and `correlate` tests use, so the expectations here
    /// rest on behaviour those modules already prove.
    const AMS_LINES: &[&str] = &[
        "09-30 23:12:01.000  1  1 I ActivityManager: ProcessRecord{aaa 1234:com.android.settings/u0a12}",
        "09-30 23:12:01.100  1  1 I ActivityManager: isCrashing=true",
        "09-30 23:12:01.220  1  1 I ActivityManager: Force finishing activity com.example.reader/.MainActivity",
        "09-30 23:13:10.000  1  1 I am_proc_start: [0,4321,com.android.settings,10012,activity,com.android.settings/.Settings]",
    ];

    const RESOURCE_LINES: &[&str] = &[
        "09-30 23:11:59.000  1  1 I lowmemorykiller: Killing 'com.example.bg' (7777), uid 10123, oom_score_adj=900 to free 65536kB",
        "09-30 23:12:00.500  1234  1234 E libc: pthread_create failed: EAGAIN",
        // Names a process, so it can be attached to that process's story by identity.
        // The two lines above cannot: one is about a process with no story, the other
        // carries no process name at all (a logcat prefix is not parsed for pids), so
        // they are counted as unattached rather than guessed onto a story.
        "09-30 23:12:00.900  1  1 I lmkd: Kill 'com.android.settings' (1234), uid=10012, oom_score_adj=900",
    ];

    fn healthy_integrity() -> IntegrityInput {
        IntegrityInput {
            records: 20_000,
            dropped: 0,
            unparsed: 20,
            requested_buffers: vec!["main".to_owned(), "system".to_owned(), "crash".to_owned()],
            available_buffers: vec!["main".to_owned(), "system".to_owned(), "crash".to_owned()],
            gaps_ms: vec![100, 200],
            crashes: vec![crash(1234, "com.android.settings")],
        }
    }

    fn run(events: &[CrashEvent], integrity: IntegrityInput) -> Forensics {
        analyse(
            &ForensicsInput {
                events,
                ams_lines: AMS_LINES,
                resource_lines: RESOURCE_LINES,
                integrity,
            },
            &Limits::default(),
            &LinkOptions::default(),
        )
    }

    #[test]
    fn the_pipeline_connects_every_phase() {
        let events = vec![
            crash(1234, "com.android.settings"),
            crash(4321, "com.android.settings"),
        ];
        let result = run(&events, healthy_integrity());

        // ams: a crash source, and the process torn down with it.
        assert!(!result.links.is_empty(), "{:#?}", result.links);
        assert!(result.links.iter().any(|link| link.victim == "com.example.reader"));

        // correlate: one identity, two pids, two crashes.
        assert_eq!(result.correlation.stories.len(), 1, "{:#?}", result.correlation.stories);
        let story = result.correlation.stories.first().expect("story");
        assert_eq!(story.identity, "com.android.settings");
        assert_eq!(story.crashes.len(), 2);
        assert_eq!(story.episodes.len(), 2);
        assert_eq!(result.crash_count(), 2);
        assert_eq!(story.victims.len(), result.links.len());

        // resources: three lines recognised; the one that names a story's process is
        // attached to it, the other two are counted rather than guessed.
        assert_eq!(result.anomalies.len(), 3, "{:#?}", result.anomalies);
        let attached = result
            .anomalies_by_story
            .by_story
            .iter()
            .find(|entry| entry.identity == "com.android.settings")
            .expect("anomaly attached to the story it belongs to");
        assert_eq!(attached.anomalies.len(), 1);
        assert_eq!(
            attached.anomalies.first().map(|anomaly| anomaly.kind),
            Some(crate::crash::resources::ResourceKind::LowMemoryKill)
        );
        assert_eq!(result.anomalies_by_story.unattached, 2);

        // integrity: a clean capture, so nothing is flagged.
        assert_eq!(result.checks.len(), 6);
        assert_eq!(result.warnings(), 0, "{:#?}", result.checks);
        assert!(result.summary().contains("1 个进程"), "{}", result.summary());
        assert!(result.summary().contains("2 次崩溃"), "{}", result.summary());
    }

    #[test]
    fn warnings_sort_to_the_top() {
        let events = vec![crash(1234, "com.android.settings")];
        let integrity = IntegrityInput {
            dropped: 4_000,
            requested_buffers: vec!["main".to_owned(), "events".to_owned()],
            available_buffers: vec!["main".to_owned()],
            ..healthy_integrity()
        };
        let result = run(&events, integrity);
        // Dropped records and the missing `events` buffer are both warnings.
        assert!(result.warnings() >= 2, "{:#?}", result.checks);
        assert_eq!(
            result.checks.first().map(|check| check.status),
            Some(CheckStatus::Warning),
            "warnings lead: {:#?}",
            result.checks
        );
        assert!(result.summary().contains("完整性告警"), "{}", result.summary());
    }

    #[test]
    fn an_empty_capture_is_described_not_treated_as_clean() {
        let result = analyse(
            &ForensicsInput {
                events: &[],
                ams_lines: &[],
                resource_lines: &[],
                integrity: IntegrityInput::default(),
            },
            &Limits::default(),
            &LinkOptions::default(),
        );
        assert!(result.signals.is_empty());
        assert!(result.links.is_empty());
        assert!(result.anomalies.is_empty());
        assert!(result.correlation.stories.is_empty());
        assert_eq!(result.correlation.unlinked_events, 0);
        assert!(result.anomalies_by_story.by_story.is_empty());
        assert_eq!(result.checks.len(), 6, "the checks still report the emptiness");
        assert_eq!(result.crash_count(), 0);
        assert!(result.summary().contains("0 个进程"));
    }

    #[test]
    fn an_event_without_a_process_is_never_attached_to_a_story() {
        let mut orphan = crash(9999, "placeholder");
        orphan.process = None;
        // `IntegrityInput.crashes` only feeds the integrity check; correlation reads
        // `events`, so an event that names no process and whose pid is unknown cannot
        // become a story — it is counted instead.
        let result = run(&[orphan], healthy_integrity());
        assert_eq!(result.correlation.unlinked_events, 1);
        assert!(result.correlation.stories.is_empty(), "{:#?}", result.correlation.stories);
    }

    /// A capture shaped like the real thing: a system app crashes, AMS marks it as crashing,
    /// the activity in front is Force finished, and the process is reaped. This is the case
    /// the whole feature exists for — the log table shows the *victim*, and only the link says
    /// who actually died.
    ///
    /// The lines are written the way MIUI/Android 17 prints them (threadtime prefix, an
    /// events-buffer record without the ActivityManager tag), not as a tidy ideal, because the
    /// value of this test is exactly that the parsers meet real wording.
    const LINKED_DEATH_CAPTURE: &[&str] = &[
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: FATAL EXCEPTION: main",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: Process: com.android.settings, PID: 1234",
        "09-30 23:12:01.100  1234  1234 E AndroidRuntime: java.lang.RuntimeException: boom",
        "09-30 23:12:01.120  1000  1000 I ActivityManager: ProcessRecord{1a2b3c4 1234:com.android.settings/u0a12}",
        "09-30 23:12:01.140  1000  1000 I ActivityManager: isCrashing=true",
        "09-30 23:12:01.160  1000  1000 I ActivityManager: Force finishing activity com.example.reader/.MainActivity",
        "09-30 23:12:01.200  1000  1000 I am_proc_died: [0,1234,com.android.settings,1000,13]",
    ];

    #[test]
    fn a_real_capture_links_the_crashing_system_app_to_the_process_it_took_down() {
        let events = vec![crash(1234, "com.android.settings")];
        let integrity = IntegrityInput {
            records: 7,
            requested_buffers: vec!["main".to_owned(), "crash".to_owned()],
            available_buffers: vec!["main".to_owned(), "crash".to_owned()],
            crashes: events.clone(),
            ..IntegrityInput::default()
        };
        let result = analyse(
            &ForensicsInput {
                events: &events,
                ams_lines: LINKED_DEATH_CAPTURE,
                resource_lines: LINKED_DEATH_CAPTURE,
                integrity,
            },
            &Limits::default(),
            &LinkOptions::default(),
        );

        // The signal that makes this case possible at all: AMS said the process is crashing.
        assert!(
            result.signals.iter().any(|signal| signal.crashing == Some(true)),
            "isCrashing must be recognised: {:#?}",
            result.signals
        );
        // The link: the crashing settings process took the reader activity with it.
        assert!(!result.links.is_empty(), "{:#?}", result.links);
        let link = result.links.first().expect("link");
        assert_eq!(link.source, "com.android.settings");
        assert_eq!(link.victim, "com.example.reader");
        // `evidence` must point at real lines of the text we passed, or the UI would jump to
        // an unrelated row after the `seqAt` translation.
        assert!(
            link.evidence.iter().all(|index| *index < LINKED_DEATH_CAPTURE.len()),
            "{:?}",
            link.evidence
        );
        // And the story: one identity, its crash, and the victim hanging off it. `victims`
        // holds the *links*, not bare names — that is what lets the UI show the reason and the
        // evidence for each one.
        let story = result
            .correlation
            .stories
            .iter()
            .find(|story| story.identity == "com.android.settings")
            .expect("settings story");
        assert!(
            story
                .victims
                .iter()
                .any(|victim| victim.victim == "com.example.reader"),
            "{:#?}",
            story.victims
        );
        assert!(!story.crashes.is_empty());
    }

    /// A ROM that words things differently must degrade to "no link", never to a wrong one.
    ///
    /// `Force stopping` is real AMS wording and is *not* the same event as `Force finishing`:
    /// it is a user or package-manager stop, so treating it as a causal kill would invent a
    /// link. The signals are still counted, and the report can say the capture contains AMS
    /// activity it could not connect.
    #[test]
    fn unfamiliar_ams_wording_yields_no_link_and_no_invention() {
        let unfamiliar: &[&str] = &[
            "09-30 23:12:01.100  1234  1234 E AndroidRuntime: Process: com.android.settings, PID: 1234",
            "09-30 23:12:01.140  1000  1000 I ActivityManager: Force stopping com.example.reader appid=10012 user=0",
            "09-30 23:12:01.160  1000  1000 I ActivityManager: Something this parser has never seen",
        ];
        let events = vec![crash(1234, "com.android.settings")];
        let result = analyse(
            &ForensicsInput {
                events: &events,
                ams_lines: unfamiliar,
                resource_lines: &[],
                integrity: IntegrityInput {
                    crashes: events.clone(),
                    ..IntegrityInput::default()
                },
            },
            &Limits::default(),
            &LinkOptions::default(),
        );
        assert!(
            result.links.is_empty(),
            "an unfamiliar stop must not become a causal link: {:#?}",
            result.links
        );
        // The crash itself is still reported — degrading the *link* must not lose the event.
        assert_eq!(result.crash_count(), 1);
    }
}
