//! Device-side probes: one command, one status, one diagnosis.
//!
//! A probe is the smallest unit of collection: a read-only command whose output
//! either contains log records or explains why it does not. Keeping the
//! diagnosis in the probe — rather than in the caller — is what lets the UI say
//! *why* a device produced nothing instead of showing an empty table.
//!
//! Every probe runs through
//! [`ExecTarget::run_tolerant`](crate::executor::ExecTarget::run_tolerant), so
//! a missing path or a denied one is reported, never fatal.

use crate::executor::ExecOutput;

/// How many files a directory probe reads back at most.
///
/// Tombstone and dropbox directories can hold dozens of entries, each of them a
/// full crash dump; the newest few carry the crash the user just reproduced.
pub const MAX_FOLLOW_UP_FILES: usize = 3;

/// Outcome of a single probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeStatus {
    /// The command ran and produced output.
    Found,
    /// The path or command exists, and there is simply nothing in it.
    Empty,
    /// The path does not exist on this device.
    Missing,
    /// Access was refused; reading needs privileges.
    Denied,
    /// The kernel ring is restricted (`dmesg_restrict`): distinct from `Denied`
    /// because it is fixable at runtime, not only by switching modes.
    Restricted,
    /// The command could not be run at all.
    Failed,
}

impl ProbeStatus {
    /// True when the probe is not a fault: data arrived, or none was expected.
    #[must_use]
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Found | Self::Empty)
    }

    /// Short Chinese label for the report UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Found => "已找到",
            Self::Empty => "为空",
            Self::Missing => "不存在",
            Self::Denied => "无权限",
            Self::Restricted => "内核受限",
            Self::Failed => "失败",
        }
    }

    /// What the user can do about a non-`Found` result.
    ///
    /// This is the whole point of the report: a silent empty table is
    /// indistinguishable from a healthy device, so each fault carries its own
    /// remedy.
    #[must_use]
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::Found => None,
            Self::Empty => Some("该位置存在但没有内容：设备可能尚未产生这类日志"),
            Self::Missing => Some("设备上不存在该路径：该机型可能不使用这个日志位置"),
            Self::Denied => Some("读取被拒绝：请在工具栏切换到 Root（su -c）模式后重试"),
            Self::Restricted => Some(
                "内核日志被限制（dmesg_restrict=1）：请使用 Root 模式，或执行 echo 0 > /proc/sys/kernel/dmesg_restrict",
            ),
            Self::Failed => Some("命令执行失败：请确认设备仍在线且 adb 版本与设备匹配"),
        }
    }
}

/// One read-only command, and how to treat its output.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    /// Stable id, used in the report and to pair a listing with its directory.
    pub id: &'static str,
    /// Human label for the report.
    pub label: &'static str,
    /// Exact device-side command that was run.
    pub command: &'static str,
    /// `false` for discovery probes: they report what exists but contribute no
    /// records, because their output is a file listing rather than log text.
    pub ingest: bool,
    /// For a listing probe, the directory the listed names live in. Each listed
    /// file is then read back with [`follow_up`].
    pub dir: Option<&'static str>,
}

/// Builds the read-back command for one file of a directory probe.
#[must_use]
pub fn follow_up(dir: &str, file: &str) -> String {
    format!("cat {}/{}", dir.trim_end_matches('/'), file)
}

/// Result of one probe, as shown in the collection report.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeOutcome {
    /// Probe id.
    pub id: String,
    /// Probe label.
    pub label: String,
    /// Command that was run.
    pub command: String,
    /// Diagnosis.
    pub status: ProbeStatus,
    /// Records this probe contributed.
    pub records: usize,
    /// Extra files read back after the probe (tombstones, pstore entries).
    pub files: Vec<String>,
    /// Device message (stderr tail), when there is one.
    pub detail: Option<String>,
    /// Remedy for a non-`Found` result.
    pub hint: Option<String>,
}

/// Crash-log probes, in the order they are worth reading.
///
/// The `logcat` buffers come first because they are the only probes that need
/// no privileges at all; the directories follow as discovery, since their
/// contents are read separately per file.
pub const CRASH_PROBES: &[Probe] = &[
    Probe {
        id: "crash-buffer",
        label: "崩溃缓冲区 logcat -b crash",
        command: "logcat -d -v threadtime -b crash",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "crash-events",
        label: "事件缓冲区（am_crash / am_anr / am_proc_died）",
        command: "logcat -d -v threadtime -b events -t 500",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "crash-kernel",
        label: "内核日志缓冲区 logcat -b kernel",
        // Modern Android exposes the kernel ring through logcat, which needs no
        // root — unlike `dmesg`, which `dmesg_restrict` usually refuses.
        command: "logcat -d -v threadtime -b kernel -t 800",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "crash-context",
        label: "崩溃前上下文（main + system）",
        command: "logcat -d -v threadtime -b main,system -t 800",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "tombstone-dir",
        label: "墓碑目录 /data/tombstones",
        command: "ls -1 /data/tombstones",
        ingest: false,
        dir: Some("/data/tombstones"),
    },
    Probe {
        id: "dropbox-dir",
        label: "系统崩溃归档 /data/system/dropbox",
        command: "ls -1 /data/system/dropbox",
        ingest: false,
        dir: Some("/data/system/dropbox"),
    },
    Probe {
        id: "anr-dir",
        label: "ANR 轨迹目录 /data/anr",
        command: "ls -1 /data/anr",
        ingest: false,
        dir: Some("/data/anr"),
    },
];

/// Recovery-mode probes.
///
/// Recovery has no `logcat` at all: the interesting history lives in the
/// recovery log, the previous recovery session and the kernel ring.
pub const RECOVERY_PROBES: &[Probe] = &[
    Probe {
        id: "recovery-log",
        label: "Recovery 主日志 /tmp/recovery.log",
        command: "cat /tmp/recovery.log",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "recovery-last-log",
        label: "上次 Recovery 日志 /cache/recovery/last_log",
        command: "cat /cache/recovery/last_log",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "recovery-kernel",
        label: "内核环形缓冲 dmesg",
        command: "dmesg",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "recovery-pstore",
        label: "pstore 目录 /sys/fs/pstore",
        command: "ls -1 /sys/fs/pstore",
        ingest: false,
        dir: Some("/sys/fs/pstore"),
    },
];

/// Boot-log probes: the first snapshot taken before polling starts.
///
/// The kernel source is picked by device capability rather than fixed here: a
/// ROM with logcat's `kernel` buffer is read through logcat (no root needed),
/// and only devices without it fall back to `dmesg`.
pub const BOOT_PROBES: &[Probe] = &[
    Probe {
        id: "boot-kernel",
        label: "内核日志缓冲区 logcat -b kernel",
        command: "logcat -d -v threadtime -b kernel -t 800",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "boot-dmesg",
        label: "内核环形缓冲 dmesg",
        command: "dmesg",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "boot-last-kmsg",
        label: "上次内核日志 /proc/last_kmsg",
        command: "cat /proc/last_kmsg",
        ingest: true,
        dir: None,
    },
    Probe {
        id: "boot-pstore",
        label: "pstore 目录 /sys/fs/pstore",
        command: "ls -1 /sys/fs/pstore",
        ingest: false,
        dir: Some("/sys/fs/pstore"),
    },
    Probe {
        id: "boot-props",
        label: "启动属性 sys.boot_completed / init.svc.bootanim",
        command: "getprop | grep -E 'sys.boot_completed|init.svc.bootanim|ro.boot.verifiedbootstate'",
        ingest: false,
        dir: None,
    },
];

/// `pstore` files worth reading, most useful first.
///
/// `console-ramoops-0` is the current crash record on most kernels;
/// `pmsg-ramoops-0` carries the tail of the previous boot's kernel messages.
/// These names are also the fallback used when the directory cannot be listed:
/// on a locked-down ROM `ls /sys/fs/pstore` may be refused while reading a known
/// file still works, and a device with no *previous* crash simply lists nothing.
pub const PSTORE_FILES: &[&str] = &[
    "console-ramoops-0",
    "console-ramoops",
    "pmsg-ramoops-0",
    "dmesg-ramoops-0",
];

/// Files to try when a directory probe could not be listed.
///
/// Empty for the ordinary directories: tombstone and dropbox names are generated
/// per crash, so a guess would only add noise. pstore names are fixed by the
/// kernel, which is what makes the fallback worth having.
#[must_use]
pub fn known_files(probe_id: &str) -> &'static [&'static str] {
    match probe_id {
        "boot-pstore" | "recovery-pstore" => PSTORE_FILES,
        _ => &[],
    }
}

/// Classifies probe output into a [`ProbeStatus`].
///
/// Diagnostics read **stderr only** when the command succeeded: log-shaped
/// output legitimately contains the words "Permission denied" as application
/// text, so scanning stdout for those words would misreport healthy probes.
#[must_use]
pub fn classify_output(output: &ExecOutput) -> ProbeStatus {
    let stdout = output.stdout.trim();
    if !stdout.is_empty() && output.code == 0 {
        return ProbeStatus::Found;
    }

    let stderr = output.stderr.to_lowercase();
    if stderr.contains("dmesg_restrict")
        || stderr.contains("klogctl")
        || stderr.contains("operation not permitted")
    {
        return ProbeStatus::Restricted;
    }
    if crate::crash::looks_like_permission_denied(&output.stderr) {
        return ProbeStatus::Denied;
    }
    if stderr.contains("no such file or directory") || stderr.contains("not found") {
        return ProbeStatus::Missing;
    }

    if stdout.is_empty() {
        // An empty stdout with no explanation is "nothing here", whether the
        // command exited zero (`ls` on an empty directory) or not.
        return if stderr.trim().is_empty() {
            ProbeStatus::Empty
        } else {
            ProbeStatus::Failed
        };
    }

    // Data arrived despite a non-zero exit: `logcat -d` does this routinely.
    ProbeStatus::Found
}

/// Trims a probe's output to the newest `limit` lines.
///
/// The newest lines are the interesting ones for every probe we run, and an
/// unbounded `logcat -b events` dump on a busy device is megabytes.
#[must_use]
pub fn tail_lines(text: &str, limit: usize) -> (String, bool) {
    let total = text.lines().count();
    if total <= limit {
        return (text.to_owned(), false);
    }
    let skip = total - limit;
    let kept = text.lines().skip(skip).collect::<Vec<_>>().join("\n");
    (kept, true)
}

/// The files of a directory listing worth reading, newest first.
///
/// Only bare names are kept: `ls -1` output already is one name per line, and a
/// name containing a path separator or whitespace cannot be trusted as a file
/// in the probed directory.
#[must_use]
pub fn listed_files(stdout: &str, limit: usize) -> Vec<String> {
    let mut names = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains('/') && !line.contains(char::is_whitespace))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort();
    names.reverse();
    names.truncate(limit);
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str, stderr: &str, code: i32) -> ExecOutput {
        ExecOutput {
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            code,
        }
    }

    #[test]
    fn successful_output_is_found() {
        assert_eq!(
            classify_output(&output("some log line\n", "", 0)),
            ProbeStatus::Found
        );
    }

    #[test]
    fn empty_directory_is_empty_not_missing() {
        assert_eq!(classify_output(&output("", "", 0)), ProbeStatus::Empty);
    }

    #[test]
    fn missing_path_is_reported_as_missing() {
        let out = output("", "ls: /data/tombstones: No such file or directory", 2);
        assert_eq!(classify_output(&out), ProbeStatus::Missing);
    }

    #[test]
    fn denied_path_is_distinguished_from_missing() {
        let out = output("", "cat: /data/anr/traces.txt: Permission denied", 1);
        assert_eq!(classify_output(&out), ProbeStatus::Denied);
        assert!(ProbeStatus::Denied.hint().is_some());
    }

    #[test]
    fn restricted_kernel_ring_has_its_own_status() {
        let out = output("", "dmesg: klogctl: Operation not permitted", 1);
        assert_eq!(classify_output(&out), ProbeStatus::Restricted);
        let hint = ProbeStatus::Restricted.hint().unwrap_or_default();
        assert!(hint.contains("dmesg_restrict"));
    }

    #[test]
    fn permission_denied_inside_log_text_is_not_a_denial() {
        // A crash buffer full of application lines that mention the phrase must
        // still count as a healthy probe.
        let out = output(
            "05-01 10:00:00.000  1  1 E Auditd  : avc: denied { read } Permission denied\n",
            "",
            0,
        );
        assert_eq!(classify_output(&out), ProbeStatus::Found);
    }

    #[test]
    fn nonzero_exit_with_data_is_still_found() {
        let out = output("05-01 10:00:00.000  1  1 I am_crash: x\n", "", 1);
        assert_eq!(classify_output(&out), ProbeStatus::Found);
    }

    #[test]
    fn unexplained_failure_is_failed() {
        let out = output("", "error: device offline", 1);
        assert_eq!(classify_output(&out), ProbeStatus::Failed);
    }

    #[test]
    fn tail_lines_keeps_the_newest() {
        let text = "1\n2\n3\n4\n5";
        let (kept, trimmed) = tail_lines(text, 2);
        assert!(trimmed);
        assert_eq!(kept, "4\n5");
        let (whole, trimmed) = tail_lines(text, 9);
        assert!(!trimmed);
        assert_eq!(whole, text);
    }

    #[test]
    fn follow_up_joins_the_directory_and_the_name() {
        assert_eq!(
            follow_up("/data/tombstones", "tombstone_09"),
            "cat /data/tombstones/tombstone_09"
        );
        // A trailing slash must not double up.
        assert_eq!(follow_up("/sys/fs/pstore/", "console-ramoops-0"), "cat /sys/fs/pstore/console-ramoops-0");
    }

    #[test]
    fn listed_files_keeps_only_plain_names_newest_first() {
        let listing = "tombstone_00\ntombstone_09\n\nnot a file\nsub/dir\n";
        assert_eq!(
            listed_files(listing, MAX_FOLLOW_UP_FILES),
            vec!["tombstone_09".to_owned(), "tombstone_00".to_owned()]
        );
        assert!(listed_files("a\nb", 1).len() == 1);
    }

    #[test]
    fn every_probe_id_is_unique_and_non_empty() {
        let mut ids: Vec<&str> = CRASH_PROBES
            .iter()
            .chain(RECOVERY_PROBES)
            .chain(BOOT_PROBES)
            .map(|probe| probe.id)
            .collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total);
        assert!(ids.iter().all(|id| !id.is_empty()));
    }

    #[test]
    fn listing_probes_name_a_directory_and_ingesting_ones_do_not() {
        for probe in CRASH_PROBES.iter().chain(RECOVERY_PROBES).chain(BOOT_PROBES) {
            assert!(!probe.command.is_empty());
            if probe.ingest {
                assert!(probe.dir.is_none(), "{} ingests a listing", probe.id);
                assert!(!probe.command.starts_with("ls -1"));
            } else if let Some(dir) = probe.dir {
                assert!(dir.starts_with('/'), "{} has a relative directory", probe.id);
            }
        }
    }
}
