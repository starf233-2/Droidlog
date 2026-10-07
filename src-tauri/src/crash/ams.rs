//! Causal death analysis: which process actually broke, and who was killed with it.
//!
//! The reported pain is precise: when a system app like Settings crashes, other
//! processes are torn down around it, and logcat shows the *victims* being
//! Force finished — the real source and its stack can be invisible in the live
//! stream. ActivityManager knows the difference and says so, in two places:
//!
//! * the **events** buffer (`am_crash`, `am_anr`, `am_proc_died`, `am_kill`), which
//!   is structured and cheap to read;
//! * the **AMS log lines** in the main/system buffers (`Force finishing activity`,
//!   `Killing 1234:com.example/u0a12 (adj 0): crash`, `isCrashing=true`,
//!   `Process com.example (pid 1234) has died`).
//!
//! This module parses both into [`AmsSignal`]s and then links them: a process that
//! announced itself as crashing is a **source**; a *different* process that gets
//! Force finished/killed shortly after is a **victim**, and the link carries the
//! line numbers of both so the UI can show the evidence and jump to it.
//!
//! Two deliberate limits:
//!
//! * only crash-shaped reasons create links — a process killed for `excessive cpu`
//!   twenty lines later is not a casualty, and calling it one would be a lie;
//! * nothing is invented: with no source in range, no link is produced. The
//!   integrity report has a section for "events that could not be linked".

use serde::{Deserialize, Serialize};

/// What an ActivityManager signal says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AmsSignalKind {
    /// `am_crash` — a process reported a crash to AMS.
    AmCrash,
    /// `am_anr` — a process stopped responding.
    AmAnr,
    /// `am_proc_died` — a process disappeared.
    AmProcDied,
    /// `am_kill` — AMS or lmkd killed a process.
    AmKill,
    /// `am_proc_start` — a process was (re)started.
    AmProcStart,
    /// `Force finishing activity …`
    ForceFinish,
    /// `Force stopping …`
    ForceStop,
    /// `Killing <pid>:<process>/<uid> (adj …): <reason>`
    Killing,
    /// `Process <name> (pid <n>) has died`
    ProcessDied,
    /// `isCrashing=true`
    IsCrashing,
    /// `ANR in <process>`
    AnrIn,
    /// `Scheduling restart of crashed service`
    Restart,
    /// `ProcessRecord{hash pid:process/uid}` — AMS naming the process it is about.
    ///
    /// Not an event, but it is how the *next* lines say which process they mean:
    /// `isCrashing=true` carries no name of its own, and without this the crash
    /// source would have no identity.
    ProcessRecord,
}

impl AmsSignalKind {
    /// Stable id for the frontend.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::AmCrash => "amCrash",
            Self::AmAnr => "amAnr",
            Self::AmProcDied => "amProcDied",
            Self::AmKill => "amKill",
            Self::AmProcStart => "amProcStart",
            Self::ForceFinish => "forceFinish",
            Self::ForceStop => "forceStop",
            Self::Killing => "killing",
            Self::ProcessDied => "processDied",
            Self::IsCrashing => "isCrashing",
            Self::AnrIn => "anrIn",
            Self::Restart => "restart",
            Self::ProcessRecord => "processRecord",
        }
    }

    /// Human label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AmCrash => "进程崩溃",
            Self::AmAnr => "进程无响应",
            Self::AmProcDied => "进程结束",
            Self::AmKill => "进程被杀",
            Self::AmProcStart => "进程启动",
            Self::ForceFinish => "强制结束界面",
            Self::ForceStop => "强制停止应用",
            Self::Killing => "杀进程",
            Self::ProcessDied => "进程死亡",
            Self::IsCrashing => "正在崩溃",
            Self::AnrIn => "ANR",
            Self::Restart => "重启服务",
            Self::ProcessRecord => "进程记录",
        }
    }

    /// Whether this signal can open a *source*.
    #[must_use]
    fn is_source(self) -> bool {
        matches!(self, Self::AmCrash | Self::AmAnr | Self::IsCrashing | Self::AnrIn)
    }

    /// Whether this signal can mark a *victim*.
    #[must_use]
    fn is_victim(self) -> bool {
        matches!(
            self,
            Self::ForceFinish | Self::ForceStop | Self::Killing | Self::ProcessDied | Self::AmProcDied
        )
    }
}

/// One parsed ActivityManager signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmsSignal {
    /// What it says.
    pub kind: AmsSignalKind,
    /// Process name (package or process name), when the line names one.
    pub process: Option<String>,
    /// Process id.
    pub pid: Option<i32>,
    /// Application uid.
    pub uid: Option<i32>,
    /// Reason text, when the line carries one (`crash`, `excessive cpu`, …).
    pub reason: Option<String>,
    /// `isCrashing=true/false`, or the flag from an `am_crash` record.
    pub crashing: Option<bool>,
    /// Exception/message from an `am_crash` record.
    pub detail: Option<String>,
    /// Index of the source line, so the UI can jump back to it.
    pub line_index: usize,
    /// The line itself, verbatim.
    pub raw: String,
}

/// How sure the link is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkConfidence {
    /// The victim's own line says it was a crash/ANR, and the source said
    /// `isCrashing=true` within a couple of seconds.
    Confirmed,
    /// Ordering and proximity agree, but no line states the cause.
    Likely,
}

/// A `source → victim` relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalLink {
    /// Process that broke first.
    pub source: String,
    /// Its pid, when known.
    pub source_pid: Option<i32>,
    /// Process that was taken down with it.
    pub victim: String,
    /// Its pid, when known.
    pub victim_pid: Option<i32>,
    /// Why this link is claimed, in the user's language.
    pub reason: String,
    /// Line indices of the evidence, in order.
    pub evidence: Vec<usize>,
    /// How sure we are.
    pub confidence: LinkConfidence,
}

/// Bounds on how far apart two signals may be and still be linked.
#[derive(Debug, Clone, Copy)]
pub struct LinkOptions {
    /// Maximum number of signal lines between source and victim.
    pub max_lines: usize,
    /// Maximum gap in milliseconds, when both lines carry a timestamp.
    pub max_gap_ms: i64,
}

impl Default for LinkOptions {
    fn default() -> Self {
        // 60 lines is generous for a burst of AMS chatter, and five seconds is the
        // window in which "torn down with it" is still the honest description.
        Self {
            max_lines: 60,
            max_gap_ms: 5_000,
        }
    }
}

/// Reads the value after `key` up to the end of a delimiter.
fn after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let value = line.get(start..)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Reads an `i32` after `key`.
fn number_after(line: &str, key: &str) -> Option<i32> {
    let value = after(line, key)?;
    let digits: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// The first package-looking token on a line (`com.example.app`).
///
/// `:` counts as a separator because AMS writes the pid in front of the name
/// (`1234:com.example/u0a12`); without that the token fails the character check and
/// the process is invisible.
fn package_token(line: &str) -> Option<String> {
    line.split(|c: char| {
        c.is_whitespace() || c == ',' || c == '{' || c == '}' || c == '/' || c == ':'
    })
    .find(|token| {
        let parts = token.split('.').count();
        parts >= 2
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && token.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    })
    .map(str::to_owned)
}

/// Splits the `[a,b,c]` payload of an events-buffer line.
fn events_fields(line: &str) -> Option<Vec<String>> {
    let start = line.find('[')? + 1;
    let rest = line.get(start..)?;
    let end = rest.find(']').unwrap_or(rest.len());
    let inside = rest.get(..end)?;
    let fields: Vec<String> = inside
        .split(',')
        .map(|field| field.trim().to_owned())
        .collect();
    if fields.iter().all(String::is_empty) {
        None
    } else {
        Some(fields)
    }
}

/// Milliseconds-of-day for a `MM-DD HH:MM:SS.mmm` prefix, when present.
///
/// Only used to compare two lines inside one capture, so the date is ignored and a
/// midnight rollover is handled by the caller.
fn stamp_ms(line: &str) -> Option<i64> {
    let trimmed = line.trim_start();
    let stamp = trimmed.get(..18)?;
    let bytes = stamp.as_bytes();
    if bytes.get(2) != Some(&b'-') || bytes.get(5) != Some(&b' ') || bytes.get(8) != Some(&b':') {
        return None;
    }
    let hours: i64 = stamp.get(6..8)?.parse().ok()?;
    let minutes: i64 = stamp.get(9..11)?.parse().ok()?;
    let seconds: i64 = stamp.get(12..14)?.parse().ok()?;
    let millis: i64 = stamp.get(15..18)?.parse().ok()?;
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

/// True for the `Killing <pid>:<process>…` shape AMS writes.
fn killing_shape(line: &str) -> bool {
    let Some(rest) = after(line, "Killing ") else {
        return false;
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    !digits.is_empty() && rest.get(digits.len()..).is_some_and(|tail| tail.starts_with(':'))
}

/// Extracts the process name from `Killing 1234:com.example/u0a12 (adj 0): crash`.
fn killing_process(body: &str) -> Option<String> {
    let rest = after(body, "Killing ")?;
    let after_pid = rest.split_once(':').map_or(rest, |(_, tail)| tail);
    let name = after_pid.split('/').next().unwrap_or(after_pid).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Parses one line into a signal, if it is an ActivityManager signal at all.
#[must_use]
pub fn parse_line(line: &str, line_index: usize) -> Option<AmsSignal> {
    let mut signal = AmsSignal {
        kind: AmsSignalKind::AmCrash,
        process: None,
        pid: None,
        uid: None,
        reason: None,
        crashing: None,
        detail: None,
        line_index,
        raw: line.to_owned(),
    };

    // ---- events buffer: `am_crash: [user,pid,process,flags,exception,message,…]`
    if let Some(fields) = events_fields(line) {
        let kind = if line.contains("am_crash:") {
            Some(AmsSignalKind::AmCrash)
        } else if line.contains("am_anr:") {
            Some(AmsSignalKind::AmAnr)
        } else if line.contains("am_proc_died:") {
            Some(AmsSignalKind::AmProcDied)
        } else if line.contains("am_kill:") {
            Some(AmsSignalKind::AmKill)
        } else if line.contains("am_proc_start:") {
            Some(AmsSignalKind::AmProcStart)
        } else if line.contains("am_low_memory:") {
            Some(AmsSignalKind::AmKill)
        } else {
            None
        };
        if let Some(kind) = kind {
            signal.kind = kind;
            signal.pid = fields.get(1).and_then(|field| field.parse().ok());
            signal.process = fields
                .get(2)
                .filter(|field| !field.is_empty())
                .cloned()
                .or_else(|| package_token(line));
            signal.uid = fields
                .get(3)
                .and_then(|field| field.parse().ok())
                .filter(|uid: &i32| *uid > 0);
            // `am_crash` carries the exception and message; `am_kill` a reason.
            if kind == AmsSignalKind::AmCrash {
                let exception = fields.get(4).filter(|field| !field.is_empty()).cloned();
                let message = fields.get(5).filter(|field| !field.is_empty()).cloned();
                signal.detail = match (exception, message) {
                    (Some(exception), Some(message)) => Some(format!("{exception}: {message}")),
                    (Some(exception), None) => Some(exception),
                    (None, message) => message,
                };
                signal.crashing = Some(true);
            }
            if matches!(kind, AmsSignalKind::AmKill | AmsSignalKind::AmAnr) {
                signal.reason = fields
                    .iter()
                    .rev()
                    .find(|field| {
                        !field.is_empty() && field.parse::<i64>().is_err() && field.contains(' ')
                    })
                    .cloned()
                    .or_else(|| fields.last().filter(|f| !f.is_empty()).cloned());
            }
            return Some(signal);
        }
    }

    // ---- ActivityManager text lines
    // Checked before `ProcessRecord{`: AMS prints the record *and* the flag on one
    // line (`ProcessRecord{…} isCrashing=true`), and the flag is the crash marker —
    // treating that line as a mere record would lose the event that matters.
    if line.contains("isCrashing=") {
        signal.kind = AmsSignalKind::IsCrashing;
        signal.crashing = after(line, "isCrashing=").map(|value| value.starts_with("true"));
        signal.process = package_token(line);
        signal.pid = after(line, "ProcessRecord{")
            .and_then(|rest| rest.split_whitespace().nth(1))
            .and_then(|token| token.split(':').next())
            .and_then(|digits| digits.trim().parse().ok())
            .or_else(|| number_after(line, "pid "));
        return Some(signal);
    }
    if line.contains("ProcessRecord{") {
        signal.kind = AmsSignalKind::ProcessRecord;
        signal.process = package_token(line);
        // `ProcessRecord{9cff32b 1234:com.example/u0a12}` — the pid sits between the
        // hash and the colon that precedes the process name.
        signal.pid = after(line, "ProcessRecord{")
            .and_then(|rest| rest.split_whitespace().nth(1))
            .and_then(|token| token.split(':').next())
            .and_then(|digits| digits.trim().parse().ok());
        // The uid is printed as `u0a12` (10000 + 12 for a normal app); deriving it
        // would be a guess, so it is left unset rather than reported wrongly.
        signal.process.as_ref()?;
        return Some(signal);
    }
    if line.contains("Force finishing activity") {
        signal.kind = AmsSignalKind::ForceFinish;
        signal.process = after(line, "Force finishing activity ")
            .map(|value| value.split('/').next().unwrap_or(value).trim().to_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| package_token(line));
        return Some(signal);
    }
    if line.contains("Force stopping ") {
        signal.kind = AmsSignalKind::ForceStop;
        signal.process = after(line, "Force stopping ")
            .map(|value| value.split_whitespace().next().unwrap_or(value).to_owned())
            .filter(|value| !value.is_empty());
        signal.pid = number_after(line, "appid=");
        signal.reason = after(line, "reason=").map(str::to_owned);
        return Some(signal);
    }
    // `Killing 1234:com.example/u0a12 (adj 0): crash` — the pid-then-colon shape is
    // required. Without it the low-memory killer's `Killing 'com.example' (1234)`
    // would be parsed as an AMS kill with a process name made of the whole remainder,
    // and the resource analyser already reports that line properly.
    if line.contains("Killing ") && killing_shape(line) {
        signal.kind = AmsSignalKind::Killing;
        signal.process = killing_process(line);
        signal.pid = line
            .split("Killing ")
            .nth(1)
            .and_then(|rest| rest.split(':').next())
            .and_then(|digits| digits.trim().parse().ok());
        signal.reason = line
            .rsplit("): ")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != line)
            .map(str::to_owned);
        return Some(signal);
    }
    if line.contains("has died") {
        signal.kind = AmsSignalKind::ProcessDied;
        signal.process = after(line, "Process ")
            .map(|value| value.split_whitespace().next().unwrap_or(value).to_owned())
            .filter(|value| !value.is_empty());
        signal.pid = number_after(line, "(pid ");
        return Some(signal);
    }
    if line.contains("ANR in ") {
        signal.kind = AmsSignalKind::AnrIn;
        signal.process = after(line, "ANR in ")
            .map(|value| value.split_whitespace().next().unwrap_or(value).to_owned())
            .filter(|value| !value.is_empty());
        signal.reason = after(line, "Reason:").map(str::to_owned);
        return Some(signal);
    }
    if line.contains("Scheduling restart of crashed service") {
        signal.kind = AmsSignalKind::Restart;
        signal.process = package_token(line);
        return Some(signal);
    }

    None
}

/// Parses every ActivityManager signal in a batch of lines.
#[must_use]
pub fn parse_signals(lines: &[&str]) -> Vec<AmsSignal> {
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(signal) = parse_line(line, index) {
            out.push(signal);
        }
    }
    out
}

/// Whether a kill reason describes a crash rather than housekeeping.
fn is_crash_reason(reason: Option<&str>) -> bool {
    let Some(reason) = reason else {
        return false;
    };
    let lower = reason.to_ascii_lowercase();
    lower.contains("crash")
        || lower.contains("anr")
        || lower.contains("error")
        || lower.contains("watchdog")
        || lower.contains("force stop")
        || lower.contains("force finish")
}

/// Active crash source while the linker walks the signal list.
struct Source {
    index: usize,
    process: String,
    pid: Option<i32>,
    detail: Option<String>,
    ms: Option<i64>,
}

/// Links crash sources to the processes torn down around them.
///
/// Returns one link per distinct `source → victim` pair, keeping the strongest
/// confidence found. Processes that crashed themselves are never reported as
/// victims, and a victim whose own line says it crashed is treated as a source too.
#[must_use]
pub fn link_deaths(signals: &[AmsSignal], options: &LinkOptions) -> Vec<CausalLink> {
    let mut links: Vec<CausalLink> = Vec::new();
    let mut sources: Vec<Source> = Vec::new();
    let mut crashed: Vec<String> = Vec::new();
    let mut previous_ms: Option<i64> = None;
    let mut day_offset = 0_i64;
    // Last process AMS named, used when a signal does not name one itself
    // (`isCrashing=true` is the important case: it is the crash marker, and it
    // carries no identity of its own).
    let mut current: Option<(String, Option<i32>)> = None;

    for (position, signal) in signals.iter().enumerate() {
        // Track wall clock forward, tolerating midnight inside one capture.
        let mut ms = stamp_ms(&signal.raw);
        if let (Some(current), Some(previous)) = (ms, previous_ms) {
            if current + 60_000 < previous {
                day_offset += 86_400_000;
            }
            ms = Some(current + day_offset);
            previous_ms = Some(current + day_offset);
        } else if ms.is_some() {
            ms = ms.map(|value| value + day_offset);
            previous_ms = ms;
        }

        if signal.kind.is_source() && signal.crashing != Some(false) {
            let named = signal
                .process
                .clone()
                .or_else(|| current.as_ref().map(|(process, _)| process.clone()));
            let pid = signal.pid.or_else(|| current.as_ref().and_then(|(_, pid)| *pid));
            if let Some(process) = named {
                if !crashed.contains(&process) {
                    crashed.push(process.clone());
                }
                sources.push(Source {
                    index: signal.line_index,
                    process,
                    pid,
                    detail: signal.detail.clone(),
                    ms,
                });
            }
            continue;
        }

        // Remember who AMS is talking about, for the signals that do not say.
        if let Some(process) = signal.process.clone() {
            current = Some((process, signal.pid));
        }

        if !signal.kind.is_victim() {
            continue;
        }
        // A kill that is plainly housekeeping links to nothing.
        if signal.kind == AmsSignalKind::AmKill && !is_crash_reason(signal.reason.as_deref()) {
            continue;
        }
        let Some(victim) = signal.process.clone() else {
            continue;
        };
        // Expire sources that are too far away, in lines and in time.
        sources.retain(|source| {
            position.saturating_sub(source.index) <= options.max_lines
                && match (ms, source.ms) {
                    (Some(victim_ms), Some(source_ms)) => {
                        (victim_ms - source_ms).abs() <= options.max_gap_ms
                    }
                    // Without timestamps only the line window applies.
                    _ => true,
                }
        });
        let Some(source) = sources
            .iter()
            .rfind(|source| source.process != victim && !crashed.contains(&victim))
        else {
            continue;
        };

        let gap = match (ms, source.ms) {
            (Some(victim_ms), Some(source_ms)) => Some(victim_ms - source_ms),
            _ => None,
        };
        let crash_reason = is_crash_reason(signal.reason.as_deref());
        let confidence = if crash_reason && gap.is_some_and(|gap| gap.abs() <= 2_000) {
            LinkConfidence::Confirmed
        } else {
            LinkConfidence::Likely
        };
        let detail = source
            .detail
            .clone()
            .map(|detail| format!("（{detail}）"))
            .unwrap_or_default();
        let reason = match gap {
            Some(gap) if gap >= 0 => format!(
                "{} 崩溃{}后 {} ms，{} 被{}（连带受害者）",
                source.process,
                detail,
                gap,
                victim,
                signal.kind.label()
            ),
            Some(gap) => format!(
                "{} 被{}比 {} 的崩溃早 {} ms（连带受害者）",
                victim,
                signal.kind.label(),
                source.process,
                -gap
            ),
            None => format!(
                "{} 崩溃{}后，{} 被{}（连带受害者）",
                source.process,
                detail,
                victim,
                signal.kind.label()
            ),
        };
        let evidence = vec![source.index, signal.line_index];

        if let Some(existing) = links
            .iter_mut()
            .find(|link| link.source == source.process && link.victim == victim)
        {
            if confidence == LinkConfidence::Confirmed {
                existing.confidence = LinkConfidence::Confirmed;
                existing.reason = reason;
                existing.evidence = evidence;
            }
            continue;
        }
        links.push(CausalLink {
            source: source.process.clone(),
            source_pid: source.pid,
            victim,
            victim_pid: signal.pid,
            reason,
            evidence,
            confidence,
        });
    }

    links
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: &[&str] = &[
        "09-30 23:12:01.100  1234  1234 I ActivityManager: ProcessRecord{abc 1234:com.android.settings/u0a12}",
        "09-30 23:12:01.110  1234  1234 I ActivityManager: isCrashing=true",
        "09-30 23:12:01.120  1234  1234 I ActivityManager: Force finishing activity com.android.settings/.Settings",
        "09-30 23:12:01.150  1234  1234 I ActivityManager: Killing 1234:com.android.settings/u0a12 (adj 0): crash",
        "09-30 23:12:01.220  9999  9999 I ActivityManager: Force finishing activity com.example.reader/.MainActivity",
        "09-30 23:12:01.300  9999  9999 I ActivityManager: Process com.example.reader (pid 4321) has died",
        "09-30 23:12:02.000  1  1 I ActivityManager: am_crash: [0,5555,com.example.victim,0,java.lang.RuntimeException,boom,Main.java,1]",
    ];

    #[test]
    fn ams_lines_are_parsed_into_signals() {
        let signals = parse_signals(LINES);
        assert!(signals.len() >= 5, "{signals:#?}");
        let kinds: Vec<AmsSignalKind> = signals.iter().map(|signal| signal.kind).collect();
        assert!(kinds.contains(&AmsSignalKind::IsCrashing));
        assert!(kinds.contains(&AmsSignalKind::ForceFinish));
        assert!(kinds.contains(&AmsSignalKind::Killing));
        assert!(kinds.contains(&AmsSignalKind::ProcessDied));
        assert!(kinds.contains(&AmsSignalKind::AmCrash));

        let crashing = signals
            .iter()
            .find(|signal| signal.kind == AmsSignalKind::IsCrashing)
            .expect("isCrashing");
        assert_eq!(crashing.crashing, Some(true));

        let killing = signals
            .iter()
            .find(|signal| signal.kind == AmsSignalKind::Killing)
            .expect("killing");
        assert_eq!(killing.pid, Some(1234));
        assert_eq!(killing.process.as_deref(), Some("com.android.settings"));
        assert_eq!(killing.reason.as_deref(), Some("crash"));

        let died = signals
            .iter()
            .find(|signal| signal.kind == AmsSignalKind::ProcessDied)
            .expect("died");
        assert_eq!(died.process.as_deref(), Some("com.example.reader"));
        assert_eq!(died.pid, Some(4321));

        let crash = signals
            .iter()
            .find(|signal| signal.kind == AmsSignalKind::AmCrash)
            .expect("am_crash");
        assert_eq!(crash.pid, Some(5555));
        assert_eq!(crash.process.as_deref(), Some("com.example.victim"));
        assert_eq!(crash.detail.as_deref(), Some("java.lang.RuntimeException: boom"));
    }

    #[test]
    fn events_lines_are_parsed_without_activity_manager_prefix() {
        let signals = parse_signals(&[
            "09-30 23:12:02.000  1  1 I am_crash: [0,5555,com.example,0,java.lang.IllegalStateException,oops,Main.java,7]",
            "09-30 23:12:03.000  1  1 I am_kill: [0,6666,com.example.bg,10123,excessive cpu]",
            "09-30 23:12:04.000  1  1 I am_proc_died: [0,7777,com.example.dead,10123,0]",
            "09-30 23:12:05.000  1  1 I am_anr: [0,8888,com.example.slow,0,Input dispatching timed out]",
            "09-30 23:12:06.000  1  1 I am_proc_start: [0,9999,com.example.new,10123,activity,com.example.new/.Main]",
        ]);
        assert_eq!(signals.len(), 5);
        assert_eq!(signals.first().map(|s| s.kind), Some(AmsSignalKind::AmCrash));
        assert_eq!(signals.first().and_then(|s| s.detail.as_deref()), Some("java.lang.IllegalStateException: oops"));
        assert_eq!(signals.get(1).map(|s| s.kind), Some(AmsSignalKind::AmKill));
        assert_eq!(signals.get(1).and_then(|s| s.reason.as_deref()), Some("excessive cpu"));
        assert_eq!(signals.get(2).map(|s| s.kind), Some(AmsSignalKind::AmProcDied));
        assert_eq!(signals.get(3).map(|s| s.kind), Some(AmsSignalKind::AmAnr));
        assert_eq!(signals.get(4).map(|s| s.kind), Some(AmsSignalKind::AmProcStart));
    }

    #[test]
    fn a_crash_links_to_the_process_torn_down_with_it() {
        let signals = parse_signals(LINES);
        let links = link_deaths(&signals, &LinkOptions::default());
        assert!(!links.is_empty(), "expected a link: {signals:#?}");
        let link = links
            .iter()
            .find(|link| link.victim == "com.example.reader")
            .expect("reader link");
        assert_eq!(link.source, "com.android.settings");
        assert_eq!(link.evidence.len(), 2);
        assert!(link.evidence.first().is_some_and(|index| *index < link.evidence[1]));
        assert!(
            link.reason.contains("com.android.settings") && link.reason.contains("连带受害者"),
            "{}",
            link.reason
        );
        // The crashing process itself is not a victim.
        assert!(links.iter().all(|link| link.victim != "com.android.settings"));
    }

    #[test]
    fn housekeeping_kills_do_not_create_links() {
        let lines = &[
            "09-30 23:12:01.100  1  1 I ActivityManager: isCrashing=true",
            "09-30 23:12:01.200  1  1 I am_kill: [0,6666,com.example.bg,10123,excessive cpu]",
            "09-30 23:12:01.300  1  1 I am_kill: [0,6667,com.example.bg2,10123,empty for 1800s]",
        ];
        let signals = parse_signals(lines);
        let links = link_deaths(&signals, &LinkOptions::default());
        assert!(links.is_empty(), "housekeeping must not be reported as a casualty: {links:#?}");
    }

    #[test]
    fn order_matters_and_the_window_is_respected() {
        // The victim is killed *before* anything crashes: no link.
        let before = &[
            "09-30 23:12:01.100  1  1 I ActivityManager: Force finishing activity com.example.reader/.MainActivity",
            "09-30 23:12:05.000  1  1 I ActivityManager: isCrashing=true",
        ];
        let signals = parse_signals(before);
        assert!(link_deaths(&signals, &LinkOptions::default()).is_empty());

        // Far outside the line window: no link either.
        let mut far: Vec<String> = vec!["09-30 23:12:01.100  1  1 I ActivityManager: isCrashing=true".to_owned()];
        for index in 0..100 {
            far.push(format!("09-30 23:12:01.{}  1  1 I ActivityManager: unrelated chatter {index}", 200 + index));
        }
        far.push("09-30 23:12:30.000  1  1 I ActivityManager: Force finishing activity com.example.late/.Main".to_owned());
        let refs: Vec<&str> = far.iter().map(String::as_str).collect();
        let signals = parse_signals(&refs);
        assert!(link_deaths(&signals, &LinkOptions::default()).is_empty());
    }

    #[test]
    fn malformed_lines_never_panic_and_never_invent_links() {
        let lines = &[
            "",
            "   ",
            "am_crash: [",
            "am_crash: []",
            "am_kill: [,,,,]",
            "Killing :",
            "Force finishing activity ",
            "Process  (pid ) has died",
            "isCrashing=",
            "09-30 23:1",
            "ANR in ",
            "Scheduling restart of crashed service",
        ];
        let signals = parse_signals(lines);
        for signal in &signals {
            assert!(!signal.raw.is_empty());
        }
        let links = link_deaths(&signals, &LinkOptions::default());
        assert!(links.is_empty(), "garbage must not produce relationships: {links:#?}");
    }

    #[test]
    fn midnight_rollover_does_not_break_the_time_window() {
        let lines = &[
            "09-30 23:59:59.900  1  1 I ActivityManager: ProcessRecord{abc 1234:com.android.settings/u0a12} isCrashing=true",
            "10-01 00:00:00.400  1  1 I ActivityManager: Force finishing activity com.example.late/.Main",
        ];
        let signals = parse_signals(lines);
        let links = link_deaths(&signals, &LinkOptions::default());
        assert_eq!(links.len(), 1, "{links:#?}");
        assert_eq!(links.first().map(|link| link.victim.as_str()), Some("com.example.late"));
        assert!(links.first().is_some_and(|link| link.reason.contains("500 ms")), "{:#?}", links.first());
    }

    #[test]
    fn labels_and_ids_are_stable() {
        assert_eq!(AmsSignalKind::ForceFinish.id(), "forceFinish");
        assert_eq!(AmsSignalKind::IsCrashing.label(), "正在崩溃");
        assert_eq!(AmsSignalKind::AmCrash.label(), "进程崩溃");
    }
}
