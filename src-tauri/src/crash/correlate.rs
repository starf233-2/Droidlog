//! Correlation: one story per process, however many pids it went through.
//!
//! Phases 1–3 produce crash events (crash buffer, dropbox, ANR, tombstone) and AMS
//! signals with `source → victim` links. That is still a pile of records keyed by
//! *pid*, and a pid is the least stable thing about a crashing app: the process is
//! killed and restarted with a new pid, sometimes twice in a minute.
//!
//! So this module groups by **identity** — the package or process name — and keeps
//! the pids as *episodes* inside it. The result answers the question the user
//! actually has ("did Settings crash again, and who did it take with it?") instead
//! of "what happened to pid 1234?".
//!
//! Identity resolution is deliberately conservative:
//!
//! * an event that names its process is used as-is;
//! * an event that only has a pid is resolved through the pid→process mapping the
//!   AMS signals provide (they are the only source that states both);
//! * an event with neither is **counted as unlinked** rather than being attached to
//!   whatever story was nearby — a wrong attribution is worse than a gap, and the
//!   integrity report (phase 8) has a field for exactly this number.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::crash::ams::{AmsSignal, AmsSignalKind, CausalLink};
use crate::crash::structured::CrashEvent;

/// One run of a process: a pid and the line range it was seen in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidEpisode {
    /// The pid this episode had.
    pub pid: i32,
    /// First evidence line for the pid.
    pub first_line: usize,
    /// Last evidence line for the pid.
    pub last_line: usize,
    /// Whether a start signal was seen for this pid (an `am_proc_start` or a
    /// `ProcessRecord`), as opposed to only hearing about its death.
    pub started: bool,
}

/// Everything known about one process identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashStory {
    /// Package or process name — stable across restarts.
    pub identity: String,
    /// Application uid, when a signal stated one.
    pub uid: Option<i32>,
    /// Every pid this identity was seen with, in order.
    pub episodes: Vec<PidEpisode>,
    /// Crashes attributed to this identity, in order.
    pub crashes: Vec<CrashEvent>,
    /// Links where this identity is the **source** (its victims).
    pub victims: Vec<CausalLink>,
    /// First line this story has evidence in.
    pub first_line: usize,
    /// Last line this story has evidence in.
    pub last_line: usize,
}

impl CrashStory {
    /// Number of distinct pids, i.e. how many times the process started over.
    #[must_use]
    pub fn restarts(&self) -> usize {
        self.episodes.len().saturating_sub(1)
    }
}

/// The whole correlation result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Correlation {
    /// One story per identity, ordered by first evidence.
    pub stories: Vec<CrashStory>,
    /// Crash events that could not be attributed to an identity.
    pub unlinked_events: usize,
    /// Links whose source was not a known story.
    pub unlinked_links: usize,
}

/// Builds the pid → process and pid → uid maps from AMS signals.
fn identity_maps(signals: &[AmsSignal]) -> (HashMap<i32, String>, HashMap<i32, i32>) {
    let mut by_pid: HashMap<i32, String> = HashMap::new();
    let mut uid_by_pid: HashMap<i32, i32> = HashMap::new();
    for signal in signals {
        let (Some(pid), Some(process)) = (signal.pid, signal.process.as_ref()) else {
            continue;
        };
        by_pid.insert(pid, process.clone());
        if let Some(uid) = signal.uid {
            uid_by_pid.insert(pid, uid);
        }
    }
    (by_pid, uid_by_pid)
}

/// Resolves the identity of a crash event, and the pid it is evidence for.
fn identity_of(event: &CrashEvent, by_pid: &HashMap<i32, String>) -> (Option<String>, Option<i32>) {
    let pid = event.pid;
    let from_pid = pid.and_then(|pid| by_pid.get(&pid).cloned());
    let identity = event.process.clone().or(from_pid);
    (identity, pid)
}

/// Groups crashes and links into one story per process identity.
///
/// `events` are in capture order; the line numbers used for ordering come from the
/// AMS signals (crash events do not carry line numbers of their own), so an event
/// is placed at the line of the first signal that matches its pid or identity.
#[must_use]
pub fn correlate(events: &[CrashEvent], signals: &[AmsSignal], links: &[CausalLink]) -> Correlation {
    let (by_pid, uid_by_pid) = identity_maps(signals);

    // Lines where each identity/pid was mentioned, so events can be positioned.
    let mut line_by_pid: HashMap<i32, (usize, usize)> = HashMap::new();
    let mut line_by_identity: HashMap<String, (usize, usize)> = HashMap::new();
    let mut started_pids: HashMap<i32, bool> = HashMap::new();
    let mut uid_by_identity: HashMap<String, i32> = HashMap::new();

    for signal in signals {
        let Some(process) = signal.process.as_ref() else {
            continue;
        };
        let entry = line_by_identity
            .entry(process.clone())
            .or_insert((signal.line_index, signal.line_index));
        entry.0 = entry.0.min(signal.line_index);
        entry.1 = entry.1.max(signal.line_index);
        if let Some(uid) = signal.uid {
            uid_by_identity.insert(process.clone(), uid);
        }
        if let Some(pid) = signal.pid {
            let pid_entry = line_by_pid
                .entry(pid)
                .or_insert((signal.line_index, signal.line_index));
            pid_entry.0 = pid_entry.0.min(signal.line_index);
            pid_entry.1 = pid_entry.1.max(signal.line_index);
            if matches!(
                signal.kind,
                AmsSignalKind::AmProcStart | AmsSignalKind::ProcessRecord
            ) {
                started_pids.insert(pid, true);
            }
        }
    }

    let mut stories: Vec<CrashStory> = Vec::new();
    let mut unlinked_events = 0_usize;
    let mut index_by_identity: HashMap<String, usize> = HashMap::new();

    for event in events {
        let (identity, pid) = identity_of(event, &by_pid);
        let Some(identity) = identity else {
            unlinked_events += 1;
            continue;
        };
        let position = pid
            .and_then(|pid| line_by_pid.get(&pid).map(|(first, _)| *first))
            .or_else(|| line_by_identity.get(&identity).map(|(first, _)| *first))
            .unwrap_or(usize::MAX);

        let index = match index_by_identity.get(&identity) {
            Some(index) => *index,
            None => {
                let index = stories.len();
                index_by_identity.insert(identity.clone(), index);
                stories.push(CrashStory {
                    uid: uid_by_identity
                        .get(&identity)
                        .copied()
                        .or_else(|| pid.and_then(|pid| uid_by_pid.get(&pid).copied())),
                    identity: identity.clone(),
                    episodes: Vec::new(),
                    crashes: Vec::new(),
                    victims: Vec::new(),
                    first_line: position,
                    last_line: position,
                });
                index
            }
        };
        let Some(story) = stories.get_mut(index) else {
            continue;
        };
        if story.uid.is_none() {
            story.uid = pid.and_then(|pid| uid_by_pid.get(&pid).copied());
        }
        story.crashes.push(event.clone());
        story.first_line = story.first_line.min(position);
        story.last_line = story.last_line.max(position);
    }

    // Pids become episodes, whether or not a crash was attributed to them: a
    // restart that has not crashed yet is still part of the story.
    for (pid, process) in &by_pid {
        let Some(index) = index_by_identity.get(process).copied() else {
            continue;
        };
        let Some(story) = stories.get_mut(index) else {
            continue;
        };
        if story.episodes.iter().any(|episode| episode.pid == *pid) {
            continue;
        }
        let (first, last) = line_by_pid.get(pid).copied().unwrap_or((usize::MAX, usize::MAX));
        story.episodes.push(PidEpisode {
            pid: *pid,
            first_line: first,
            last_line: last,
            started: started_pids.get(pid).copied().unwrap_or(false),
        });
        if first != usize::MAX {
            story.first_line = story.first_line.min(first);
            story.last_line = story.last_line.max(last);
        }
    }
    for story in stories.iter_mut() {
        story.episodes.sort_by_key(|episode| episode.first_line);
    }

    // Links belong to the story of their source.
    let mut unlinked_links = 0_usize;
    for link in links {
        match index_by_identity.get(&link.source).copied() {
            Some(index) => {
                if let Some(story) = stories.get_mut(index) {
                    if !story.victims.iter().any(|existing| existing.victim == link.victim) {
                        story.victims.push(link.clone());
                    }
                    if let Some(line) = link.evidence.first() {
                        story.first_line = story.first_line.min(*line);
                    }
                    if let Some(line) = link.evidence.last() {
                        story.last_line = story.last_line.max(*line);
                    }
                }
            }
            // A link whose source never produced a story: counted, not fabricated
            // into one.
            None => unlinked_links += 1,
        }
    }

    stories.sort_by_key(|story| story.first_line);
    Correlation {
        stories,
        unlinked_events,
        unlinked_links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::ams::LinkConfidence;
    use crate::crash::structured::{self, CrashOrigin};

    fn crash(pid: i32, process: &str, exception: &str) -> CrashEvent {
        let text = format!(
            "Process: {process}, PID: {pid}\njava.lang.RuntimeException: {exception}\n\tat {process}.Main.onCreate(Main.java:1)\n"
        );
        structured::parse_entry(CrashOrigin::Dropbox, &text)
            .into_iter()
            .next()
            .expect("event")
    }

    const SIGNALS: &[&str] = &[
        // Settings, first run.
        "09-30 23:12:01.000  1  1 I ActivityManager: ProcessRecord{aaa 1234:com.android.settings/u0a12}",
        "09-30 23:12:01.100  1  1 I ActivityManager: isCrashing=true",
        "09-30 23:12:01.150  1  1 I ActivityManager: Force finishing activity com.example.reader/.MainActivity",
        // Settings comes back with a new pid, and crashes again.
        "09-30 23:13:10.000  1  1 I am_proc_start: [0,4321,com.android.settings,10012,activity,com.android.settings/.Settings]",
        "09-30 23:13:20.000  1  1 I ActivityManager: ProcessRecord{bbb 4321:com.android.settings/u0a12}",
        "09-30 23:13:20.100  1  1 I ActivityManager: isCrashing=true",
    ];

    #[test]
    fn one_story_per_identity_across_pid_changes() {
        let signals = crate::crash::ams::parse_signals(SIGNALS);
        let events = vec![
            crash(1234, "com.android.settings", "first"),
            crash(4321, "com.android.settings", "second"),
            crash(9999, "com.example.unrelated", "other"),
        ];
        let correlation = correlate(&events, &signals, &[]);
        assert_eq!(correlation.stories.len(), 2, "{:#?}", correlation.stories);

        let settings = correlation
            .stories
            .iter()
            .find(|story| story.identity == "com.android.settings")
            .expect("settings story");
        assert_eq!(settings.crashes.len(), 2, "both crashes belong to one identity");
        assert_eq!(settings.episodes.len(), 2, "two pids are two episodes");
        assert_eq!(settings.restarts(), 1);
        assert_eq!(settings.episodes.first().map(|e| e.pid), Some(1234));
        assert_eq!(settings.episodes.get(1).map(|e| e.pid), Some(4321));
        assert!(
            settings.episodes.get(1).is_some_and(|episode| episode.started),
            "am_proc_start marks the restart"
        );
        assert_eq!(settings.crashes.first().and_then(|c| c.message.as_deref()), Some("first"));
        assert_eq!(settings.crashes.get(1).and_then(|c| c.message.as_deref()), Some("second"));

        let unrelated = correlation
            .stories
            .iter()
            .find(|story| story.identity == "com.example.unrelated")
            .expect("unrelated story");
        assert!(unrelated.episodes.is_empty(), "no evidence for that pid");
        assert_eq!(correlation.unlinked_events, 0);
    }

    #[test]
    fn identity_is_recovered_from_a_pid_when_the_event_has_no_process() {
        let signals = crate::crash::ams::parse_signals(SIGNALS);
        // A dropbox tombstone-style event: pid only.
        let mut event = crash(1234, "placeholder", "boom");
        event.process = None;
        let correlation = correlate(&[event], &signals, &[]);
        assert_eq!(correlation.stories.len(), 1);
        assert_eq!(
            correlation.stories.first().map(|story| story.identity.as_str()),
            Some("com.android.settings"),
            "the pid map provides the identity"
        );
        assert_eq!(correlation.unlinked_events, 0);
    }

    #[test]
    fn an_unknown_pid_is_counted_not_guessed() {
        let signals = crate::crash::ams::parse_signals(SIGNALS);
        let mut event = crash(7777, "placeholder", "boom");
        event.process = None;
        let correlation = correlate(&[event], &signals, &[]);
        assert!(correlation.stories.is_empty(), "no story may be invented");
        assert_eq!(correlation.unlinked_events, 1);
    }

    #[test]
    fn victims_are_attached_to_the_source_story() {
        let signals = crate::crash::ams::parse_signals(SIGNALS);
        let links = crate::crash::ams::link_deaths(&signals, &Default::default());
        assert!(!links.is_empty());
        let events = vec![crash(1234, "com.android.settings", "first")];
        let correlation = correlate(&events, &signals, &links);
        let story = correlation
            .stories
            .iter()
            .find(|story| story.identity == "com.android.settings")
            .expect("settings story");
        assert_eq!(story.victims.len(), links.len());
        assert!(story.victims.iter().any(|link| link.victim == "com.example.reader"));
        assert_eq!(correlation.unlinked_links, 0);
    }

    #[test]
    fn a_link_without_a_story_is_counted() {
        let signals = crate::crash::ams::parse_signals(SIGNALS);
        let orphan = CausalLink {
            source: "com.example.never-seen".to_owned(),
            source_pid: None,
            victim: "com.example.victim".to_owned(),
            victim_pid: None,
            reason: "test".to_owned(),
            evidence: vec![1, 2],
            confidence: LinkConfidence::Likely,
        };
        let correlation = correlate(&[], &signals, &[orphan]);
        assert_eq!(correlation.unlinked_links, 1);
        assert!(correlation.stories.is_empty());
    }

    #[test]
    fn uid_is_carried_onto_the_story() {
        let signals = crate::crash::ams::parse_signals(&[
            "09-30 23:12:02.000  1  1 I am_crash: [0,5555,com.example.victim,10123,java.lang.RuntimeException,boom,Main.java,1]",
        ]);
        let events = vec![crash(5555, "com.example.victim", "boom")];
        let correlation = correlate(&events, &signals, &[]);
        let story = correlation.stories.first().expect("story");
        assert_eq!(story.uid, Some(10123), "the events buffer states the uid");
    }

    #[test]
    fn malformed_input_produces_nothing_rather_than_nonsense() {
        assert_eq!(
            correlate(&[], &[], &[]),
            Correlation {
                stories: Vec::new(),
                unlinked_events: 0,
                unlinked_links: 0
            }
        );
        // Signals with a pid but no process cannot create an identity.
        let signals = crate::crash::ams::parse_signals(&["am_kill: [0,6666,,10123,excessive cpu]"]);
        let correlation = correlate(&[], &signals, &[]);
        assert!(correlation.stories.is_empty());
    }

    #[test]
    fn stories_are_ordered_by_first_evidence() {
        let signals = crate::crash::ams::parse_signals(SIGNALS);
        let events = vec![
            crash(4321, "com.android.settings", "second"),
            crash(1234, "com.android.settings", "first"),
            crash(9999, "com.example.unrelated", "other"),
        ];
        let correlation = correlate(&events, &signals, &[]);
        let first_lines: Vec<usize> = correlation.stories.iter().map(|story| story.first_line).collect();
        let mut sorted = first_lines.clone();
        sorted.sort_unstable();
        assert_eq!(first_lines, sorted);
        assert_eq!(correlation.stories.first().map(|story| story.identity.as_str()), Some("com.android.settings"));
    }
}
