//! Resource anomalies: the pressure that shows up before a crash does.
//!
//! A process rarely dies for no reason. Before it does, the device usually says so
//! — the kernel's low-memory killer picks a victim, a file descriptor table runs
//! out, thread creation starts failing, or a Binder transaction is too large to
//! deliver. Those lines are scattered across buffers, and none of them is a crash,
//! which is why they are easy to read past.
//!
//! This module recognises them and, crucially, [`attach`]s them to the crash
//! stories from [`crate::crash::correlate`]: "com.example died" becomes "com.example
//! died, 2.4 s after the kernel reclaimed 640 MB and killed two of its siblings".
//!
//! Detection is a pure text match with a *tight* pattern per family. A loose match
//! ("the line contains 'memory'") would produce noise on every device, so each
//! family here is anchored on the exact wording the platform prints, and each has a
//! negative test.

use serde::{Deserialize, Serialize};

use crate::crash::correlate::CrashStory;

/// Which resource ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceKind {
    /// `lowmemorykiller` / `lmkd` reclaimed a process.
    LowMemoryKill,
    /// The allocator failed (`OutOfMemoryError`, `Failed to allocate`).
    OutOfMemory,
    /// The file descriptor table was exhausted (`EMFILE`, `fdsan`).
    FdExhaustion,
    /// Thread creation failed (`pthread_create`, `unable to create thread`).
    ThreadExhaustion,
    /// A Binder call could not be delivered (`TransactionTooLargeException`, …).
    BinderFailure,
}

impl ResourceKind {
    /// Stable id for the frontend.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::LowMemoryKill => "lowMemoryKill",
            Self::OutOfMemory => "outOfMemory",
            Self::FdExhaustion => "fdExhaustion",
            Self::ThreadExhaustion => "threadExhaustion",
            Self::BinderFailure => "binderFailure",
        }
    }

    /// Human label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LowMemoryKill => "低内存回收",
            Self::OutOfMemory => "内存分配失败",
            Self::FdExhaustion => "文件描述符耗尽",
            Self::ThreadExhaustion => "线程创建失败",
            Self::BinderFailure => "Binder 异常",
        }
    }

    /// Whether the anomaly on its own explains a death.
    ///
    /// A low-memory kill *is* the death; the others are conditions that make the
    /// next crash likely, so the UI presents them as context rather than as causes.
    #[must_use]
    pub fn kills_directly(self) -> bool {
        matches!(self, Self::LowMemoryKill)
    }
}

/// How much attention an anomaly deserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Worth knowing, might be normal on this device.
    Warning,
    /// The condition ends a process (or has already done so).
    Critical,
}

/// One recognised anomaly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAnomaly {
    /// What ran out.
    pub kind: ResourceKind,
    /// How bad it is.
    pub severity: Severity,
    /// Process the line names, when it names one.
    pub process: Option<String>,
    /// Pid the line names, when it names one.
    pub pid: Option<i32>,
    /// The informative fragment (numbers, signal, reason), trimmed for display.
    pub detail: String,
    /// Index of the source line.
    pub line_index: usize,
    /// The line verbatim.
    pub raw: String,
}

/// Anomalies grouped under the crash story they belong to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryAnomalies {
    /// Identity of the story, as used by [`crate::crash::correlate`].
    pub identity: String,
    /// Anomalies about that identity, in order.
    pub anomalies: Vec<ResourceAnomaly>,
}

/// Result of attaching anomalies to stories.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAttachment {
    /// Per-story anomalies, only for identities that have a story.
    pub by_story: Vec<StoryAnomalies>,
    /// Anomalies about processes that have no story: counted, never attached.
    pub unattached: usize,
}

/// Extracts a quoted process name (`'com.example'`).
fn quoted(line: &str) -> Option<String> {
    let start = line.find('\'')? + 1;
    let rest = line.get(start..)?;
    let end = rest.find('\'')?;
    let name = rest.get(..end)?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// First `<pid>` inside parentheses: `(1234)`.
fn paren_pid(line: &str) -> Option<i32> {
    let start = line.find('(')? + 1;
    let rest = line.get(start..)?;
    let end = rest.find(')').unwrap_or(rest.len());
    let digits: String = rest
        .get(..end)?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// A package-looking token, used when a line names no quoted process.
fn package_token(line: &str) -> Option<String> {
    line.split(|c: char| {
        c.is_whitespace() || c == ',' || c == '{' || c == '}' || c == '/' || c == ':' || c == '\''
    })
    .find(|token| {
        token.split('.').count() >= 2
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && token.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    })
    .map(str::to_owned)
}

/// Trims a matched fragment for display.
fn fragment(line: &str, needle: &str) -> String {
    let Some(start) = line.find(needle) else {
        return needle.to_owned();
    };
    let rest = line.get(start..).unwrap_or(needle);
    let cut = rest
        .char_indices()
        .take_while(|(index, _)| *index < 160)
        .last()
        .map_or(rest.len(), |(index, c)| index + c.len_utf8());
    rest.get(..cut).unwrap_or(rest).trim().to_owned()
}

/// Parses one line into an anomaly, if it is one.
#[must_use]
pub fn parse_line(line: &str, line_index: usize) -> Option<ResourceAnomaly> {
    let mut kind = None;
    let mut severity = Severity::Warning;
    let mut needle = "";

    // ---- low memory -------------------------------------------------------
    // `lowmemorykiller: Killing 'com.example' (1234), uid 10123, oom_score_adj=900`
    // `lmkd     : Kill 'com.example' (1234), uid=10123, oom_score_adj=900`
    if (line.contains("lowmemorykiller") || line.contains("lmkd"))
        && (line.contains("Killing") || line.contains("Kill "))
    {
        kind = Some(ResourceKind::LowMemoryKill);
        severity = Severity::Critical;
        needle = "Killing";
        if !line.contains(needle) {
            needle = "Kill ";
        }
    } else if line.contains("OutOfMemoryError") || line.contains("Failed to allocate") {
        kind = Some(ResourceKind::OutOfMemory);
        severity = Severity::Critical;
        needle = if line.contains("OutOfMemoryError") {
            "OutOfMemoryError"
        } else {
            "Failed to allocate"
        };
    } else if line.contains("EMFILE")
        || line.contains("Too many open files")
        || line.contains("fdsan")
        || line.contains("FileDescriptorLimit")
    {
        kind = Some(ResourceKind::FdExhaustion);
        severity = Severity::Critical;
        needle = if line.contains("EMFILE") {
            "EMFILE"
        } else if line.contains("Too many open files") {
            "Too many open files"
        } else if line.contains("fdsan") {
            "fdsan"
        } else {
            "FileDescriptorLimit"
        };
    } else if line.contains("pthread_create")
        || line.contains("unable to create thread")
        || line.contains("Thread creation failed")
        || line.contains("OutOfResources: thread")
    {
        kind = Some(ResourceKind::ThreadExhaustion);
        severity = Severity::Critical;
        needle = if line.contains("pthread_create") {
            "pthread_create"
        } else if line.contains("unable to create thread") {
            "unable to create thread"
        } else if line.contains("Thread creation failed") {
            "Thread creation failed"
        } else {
            "OutOfResources: thread"
        };
    } else if line.contains("TransactionTooLargeException")
        || line.contains("Binder transaction failed")
        || line.contains("binder transaction failed")
        || line.contains("DeadSystemException")
        || line.contains("Failed to find prepared transaction")
    {
        kind = Some(ResourceKind::BinderFailure);
        severity = Severity::Warning;
        needle = if line.contains("TransactionTooLargeException") {
            "TransactionTooLargeException"
        } else if line.contains("DeadSystemException") {
            "DeadSystemException"
        } else if line.contains("Failed to find prepared transaction") {
            "Failed to find prepared transaction"
        } else {
            "transaction failed"
        };
    }

    let kind = kind?;
    let process = quoted(line).or_else(|| package_token(line));
    let pid = paren_pid(line).or_else(|| {
        // `Killing 1234:com.example` writes the pid before the colon.
        line.split("Killing ")
            .nth(1)
            .and_then(|rest| rest.split(':').next())
            .and_then(|digits| digits.trim().parse().ok())
    });
    Some(ResourceAnomaly {
        kind,
        severity,
        process,
        pid,
        detail: fragment(line, needle),
        line_index,
        raw: line.to_owned(),
    })
}

/// Parses every anomaly in a batch of lines.
#[must_use]
pub fn parse_signals(lines: &[&str]) -> Vec<ResourceAnomaly> {
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(anomaly) = parse_line(line, index) {
            out.push(anomaly);
        }
    }
    out
}

/// Attaches anomalies to the crash stories they explain.
///
/// An anomaly is attached by identity (or by pid, resolved through the story's own
/// episodes). Anomalies about processes with no story are counted in `unattached`:
/// the timeline shows nothing for them, and the integrity report says how many were
/// left over instead of the UI silently dropping them.
#[must_use]
pub fn attach(anomalies: &[ResourceAnomaly], stories: &[CrashStory]) -> ResourceAttachment {
    let mut by_story: Vec<StoryAnomalies> = Vec::new();
    let mut unattached = 0_usize;

    for story in stories {
        let pids: Vec<i32> = story.episodes.iter().map(|episode| episode.pid).collect();
        let mut matched: Vec<ResourceAnomaly> = Vec::new();
        for anomaly in anomalies {
            let by_identity = anomaly
                .process
                .as_ref()
                .is_some_and(|process| *process == story.identity);
            let by_pid = anomaly
                .pid
                .is_some_and(|pid| pids.contains(&pid));
            if by_identity || by_pid {
                matched.push(anomaly.clone());
            }
        }
        if !matched.is_empty() {
            by_story.push(StoryAnomalies {
                identity: story.identity.clone(),
                anomalies: matched,
            });
        }
    }

    for anomaly in anomalies {
        let attached = by_story
            .iter()
            .any(|entry| entry.anomalies.iter().any(|item| item.line_index == anomaly.line_index));
        if !attached {
            unattached += 1;
        }
    }

    ResourceAttachment {
        by_story,
        unattached,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::correlate::{CrashStory, PidEpisode};

    #[test]
    fn low_memory_kills_are_recognised() {
        let line = "09-30 23:12:00.100  1  1 I lowmemorykiller: Killing 'com.example.bg' (1234), uid 10123, oom_score_adj=900 to free 65536kB";
        let anomaly = parse_line(line, 7).expect("anomaly");
        assert_eq!(anomaly.kind, ResourceKind::LowMemoryKill);
        assert_eq!(anomaly.severity, Severity::Critical);
        assert_eq!(anomaly.process.as_deref(), Some("com.example.bg"));
        assert_eq!(anomaly.pid, Some(1234));
        assert_eq!(anomaly.line_index, 7);
        assert!(anomaly.kind.kills_directly());

        // lmkd writes it differently.
        let lmkd = parse_line("09-30 23:12:00.200  1  1 I lmkd: Kill 'com.example.bg' (1234), uid=10123, oom_score_adj=900", 8)
            .expect("lmkd anomaly");
        assert_eq!(lmkd.kind, ResourceKind::LowMemoryKill);
        assert_eq!(lmkd.pid, Some(1234));
    }

    #[test]
    fn allocation_failures_are_recognised() {
        let oom = parse_line(
            "09-30 23:12:01.000  1234  1234 E AndroidRuntime: java.lang.OutOfMemoryError: Failed to allocate a 1048576 byte allocation",
            1,
        )
        .expect("oom");
        assert_eq!(oom.kind, ResourceKind::OutOfMemory);
        assert!(oom.detail.contains("OutOfMemoryError"), "{}", oom.detail);

        let native = parse_line("09-30 23:12:01.100  1234  1234 F libc: Failed to allocate 4096 bytes", 2)
            .expect("native oom");
        assert_eq!(native.kind, ResourceKind::OutOfMemory);
    }

    #[test]
    fn fd_and_thread_exhaustion_are_recognised() {
        let fd = parse_line(
            "09-30 23:12:02.000  1234  1234 E System.err: open failed: EMFILE (Too many open files)",
            3,
        )
        .expect("fd");
        assert_eq!(fd.kind, ResourceKind::FdExhaustion);
        assert_eq!(fd.severity, Severity::Critical);

        let fdsan = parse_line("09-30 23:12:02.100  1234  1234 E fdsan: attempted to close file descriptor 42", 4)
            .expect("fdsan");
        assert_eq!(fdsan.kind, ResourceKind::FdExhaustion);

        let thread = parse_line("09-30 23:12:03.000  1234  1234 E libc: pthread_create failed: EAGAIN", 5)
            .expect("thread");
        assert_eq!(thread.kind, ResourceKind::ThreadExhaustion);

        let thread2 = parse_line("09-30 23:12:03.100  1234  1234 E art: unable to create thread", 6)
            .expect("thread2");
        assert_eq!(thread2.kind, ResourceKind::ThreadExhaustion);
    }

    #[test]
    fn binder_failures_are_recognised_as_warnings() {
        let too_large = parse_line(
            "09-30 23:12:04.000  1234  1234 E JavaBinder: TransactionTooLargeException: data parcel size 1234567 bytes",
            9,
        )
        .expect("binder");
        assert_eq!(too_large.kind, ResourceKind::BinderFailure);
        assert_eq!(too_large.severity, Severity::Warning);
        assert!(!too_large.kind.kills_directly(), "a large transaction is context, not a cause");

        let dead = parse_line("09-30 23:12:04.100  1234  1234 W System.err: android.os.DeadSystemException", 10)
            .expect("dead");
        assert_eq!(dead.kind, ResourceKind::BinderFailure);
    }

    #[test]
    fn ordinary_chatter_is_not_an_anomaly() {
        let lines = &[
            "09-30 23:12:00.000  1234  1234 I ActivityManager: Start proc com.example for activity",
            "09-30 23:12:00.100  1234  1234 D dalvikvm: GC freed 1234 objects / 56789 bytes in 12ms",
            "09-30 23:12:00.200  1234  1234 I memory  : total 5832 MB, free 2048 MB",
            "09-30 23:12:00.300  1234  1234 V Binder  : transaction 1234 complete",
            "09-30 23:12:00.400  1234  1234 I chat    : Killing time on the couch",
        ];
        for (index, line) in lines.iter().enumerate() {
            assert!(parse_line(line, index).is_none(), "false positive on: {line}");
        }
    }

    #[test]
    fn malformed_lines_do_not_panic() {
        let lines = &[
            "",
            "   ",
            "lowmemorykiller:",
            "lowmemorykiller: Killing '",
            "lmkd: Kill 'x' (",
            "OutOfMemoryError",
            "EMFILE (",
            "pthread_create",
            "TransactionTooLargeException",
            "lowmemorykiller: Killing 'com.a.b' (99999999999999999999), uid 1",
        ];
        for (index, line) in lines.iter().enumerate() {
            let _ = parse_line(line, index);
        }
        // A huge pid cannot be parsed, but the anomaly is still reported.
        let huge = parse_line(
            "lowmemorykiller: Killing 'com.a.b' (99999999999999999999), uid 1",
            0,
        )
        .expect("anomaly");
        assert_eq!(huge.pid, None);
        assert_eq!(huge.process.as_deref(), Some("com.a.b"));
    }

    fn story(identity: &str, pid: i32) -> CrashStory {
        CrashStory {
            identity: identity.to_owned(),
            uid: None,
            episodes: vec![PidEpisode {
                pid,
                first_line: 0,
                last_line: 10,
                started: true,
            }],
            crashes: Vec::new(),
            victims: Vec::new(),
            first_line: 0,
            last_line: 10,
        }
    }

    #[test]
    fn anomalies_attach_by_identity_or_pid() {
        let anomalies = parse_signals(&[
            "lowmemorykiller: Killing 'com.example.bg' (1234), uid 10123, oom_score_adj=900",
            "pthread_create failed: EAGAIN",
            "lowmemorykiller: Killing 'com.other' (7777), uid 10, oom_score_adj=900",
        ]);
        let stories = vec![story("com.example.bg", 1234), story("com.example.bg2", 4321)];
        let attachment = attach(&anomalies, &stories);

        let first = attachment
            .by_story
            .iter()
            .find(|entry| entry.identity == "com.example.bg")
            .expect("first story");
        assert_eq!(first.anomalies.len(), 1);
        assert_eq!(first.anomalies.first().map(|a| a.kind), Some(ResourceKind::LowMemoryKill));
        // The story with no matching anomaly gets no entry at all.
        assert!(attachment.by_story.iter().all(|entry| entry.identity != "com.example.bg2"));
        // `com.other` and the anonymous thread failure are counted, not attached.
        assert_eq!(attachment.unattached, 2);
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(ResourceKind::LowMemoryKill.id(), "lowMemoryKill");
        assert_eq!(ResourceKind::FdExhaustion.label(), "文件描述符耗尽");
        assert!(Severity::Critical > Severity::Warning);
    }
}
