//! Boot-log collection: one snapshot, then polling until boot completes.
//!
//! Boot evidence is not a stream. `dmesg` is a ring buffer that already holds
//! everything since power-on, `last_kmsg` holds the *previous* boot, and pstore
//! holds a crash that survived a reboot. What is missing is the tail: a device
//! that has not finished booting still has messages to produce, so the snapshot
//! is followed by polling until `sys.boot_completed` flips or the window runs
//! out.
//!
//! The window is bounded and reported, because a collection that silently runs
//! forever is indistinguishable from a hang.

use std::collections::HashSet;
use std::time::Duration;

use crate::adb::Adb;
use crate::executor::ExecMode;

use super::probe::{self, ProbeOutcome, ProbeStatus};
use super::profile::DeviceProfile;
use super::{CollectRequest, Ingestor, MAX_PROBE_LINES};

/// Default polling window: two minutes covers a cold boot with room to spare.
pub const DEFAULT_WINDOW_MS: u64 = 120_000;

/// Default poll interval.
pub const DEFAULT_INTERVAL_MS: u64 = 2_000;

/// Shortest interval a caller may ask for.
pub const MIN_INTERVAL_MS: u64 = 500;

/// Longest window a caller may ask for.
pub const MAX_WINDOW_MS: u64 = 900_000;

/// The command re-run on every tick, when the ROM has no logcat `kernel` buffer.
pub const POLL_COMMAND: &str = "dmesg";

/// The modern equivalent: the kernel ring through logcat, readable without root.
pub const POLL_COMMAND_KERNEL: &str = "logcat -d -v threadtime -b kernel";

/// Snapshot probes that read the kernel ring, either of which means the poller
/// must align with the snapshot instead of re-emitting it.
pub const KERNEL_SNAPSHOT_PROBES: [&str; 2] = ["boot-kernel", "boot-dmesg"];

/// The command that tells us boot finished.
pub const BOOT_STATE_COMMAND: &str = "getprop sys.boot_completed; getprop dev.bootcomplete";

/// Bounded polling parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Total polling window in milliseconds.
    pub window_ms: u64,
    /// Delay between two polls in milliseconds.
    pub interval_ms: u64,
}

impl Window {
    /// Builds a window from the request, clamped to sane bounds.
    ///
    /// A caller asking for a zero window gets a single snapshot rather than an
    /// error: the snapshot is the useful part, the polling is the bonus.
    #[must_use]
    pub fn new(duration_ms: Option<u64>, interval_ms: Option<u64>) -> Self {
        Self {
            window_ms: duration_ms.unwrap_or(DEFAULT_WINDOW_MS).min(MAX_WINDOW_MS),
            interval_ms: interval_ms
                .unwrap_or(DEFAULT_INTERVAL_MS)
                .max(MIN_INTERVAL_MS),
        }
    }

    /// How many polls fit in the window.
    #[must_use]
    pub fn expected_polls(&self) -> u32 {
        let polls = self.window_ms / self.interval_ms.max(1);
        u32::try_from(polls).unwrap_or(u32::MAX).max(1)
    }
}

/// Runs the boot collection for a request, returning its probe outcomes.
pub(crate) async fn collect(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    ingest: &mut Ingestor,
    request: &CollectRequest,
    profile: &DeviceProfile,
) -> Vec<ProbeOutcome> {
    let window = Window::new(request.duration_ms, request.interval_ms);
    let probes = super::Run::Boot.probes(profile);
    let outcomes = super::run_probes(adb, mode, serial, &probes, ingest).await;
    let mut outcomes = outcomes;

    // Which command polls depends on the device: logcat's `kernel` buffer where
    // the ROM has one (no root needed), `dmesg` otherwise. Polling with both
    // would ingest every line twice.
    let poll_command = if profile.has_buffer("kernel") {
        POLL_COMMAND_KERNEL
    } else {
        POLL_COMMAND
    };

    // When the snapshot already read the kernel ring, the first poll is used
    // only to learn its tail. Ingesting it again would duplicate every line —
    // the collectors would appear to have collected the same boot twice.
    //
    // Either kernel probe counts: which one ran depends on the ROM (logcat's
    // `kernel` buffer, or `dmesg` without it), and checking only for `dmesg` is
    // what let a device on the logcat path ingest the whole buffer twice.
    let mut seed_only = outcomes
        .iter()
        .any(|outcome| KERNEL_SNAPSHOT_PROBES.contains(&outcome.id.as_str()) && outcome.records > 0);

    let ends_at_ms = crate::process::now_ms().saturating_add(window.window_ms);
    let total = window.expected_polls();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(window.window_ms);

    let mut polls = 0_u32;
    let mut records = 0_usize;
    let mut skip: HashSet<u64> = HashSet::new();
    let mut booted = false;
    let mut note = None;

    loop {
        polls = polls.saturating_add(1);
        ingest.set_progress("启动日志采集中", ends_at_ms, polls, total);

        match adb.target(mode, Some(serial)).run_tolerant(poll_command).await {
            Ok(output) => {
                let status = probe::classify_output(&output);
                if status == ProbeStatus::Restricted || status == ProbeStatus::Denied {
                    note = Some(
                        status
                            .hint()
                            .unwrap_or("内核日志不可读")
                            .to_owned(),
                    );
                    break;
                }
                let (text, _) = probe::tail_lines(&output.stdout, MAX_PROBE_LINES);
                if seed_only {
                    // Align with the snapshot instead of re-emitting it.
                    skip = super::tail_hashes(&text);
                    seed_only = false;
                } else {
                    let (accepted, tail) = ingest.feed_incremental(&text, &skip);
                    records += accepted;
                    skip = tail;
                }
            }
            Err(err) => {
                note = Some(format!("轮询失败：{err}"));
                break;
            }
        }

        booted = boot_completed(adb, mode, serial).await;
        if booted || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(window.interval_ms)).await;
    }

    let label = if polls <= 1 && booted {
        "设备已启动完成，仅采集一次内核缓冲".to_owned()
    } else if booted {
        format!("轮询内核缓冲 {polls} 次（设备已启动完成）")
    } else {
        format!("轮询内核缓冲 {polls} 次（窗口 {} 秒结束）", window.window_ms / 1000)
    };

    outcomes.push(ProbeOutcome {
        id: "boot-poll".to_owned(),
        label,
        command: poll_command.to_owned(),
        status: if records > 0 {
            ProbeStatus::Found
        } else {
            ProbeStatus::Empty
        },
        records,
        files: Vec::new(),
        detail: note.or_else(|| {
            if records == 0 {
                Some("快照已包含内核缓冲的全部内容，轮询期间没有新增".to_owned())
            } else {
                None
            }
        }),
        hint: None,
    });

    outcomes
}

/// Whether the device reports a finished boot.
///
/// Both properties are checked because vendors populate different ones;
/// an unreadable answer counts as "not yet", which only costs one more poll.
async fn boot_completed(adb: &Adb, mode: ExecMode, serial: &str) -> bool {
    match adb
        .target(mode, Some(serial))
        .run_tolerant(BOOT_STATE_COMMAND)
        .await
    {
        Ok(output) => output
            .stdout
            .lines()
            .map(str::trim)
            .any(|line| line == "1"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_used_when_absent() {
        let window = Window::new(None, None);
        assert_eq!(window.window_ms, DEFAULT_WINDOW_MS);
        assert_eq!(window.interval_ms, DEFAULT_INTERVAL_MS);
        assert_eq!(window.expected_polls(), 60);
    }

    #[test]
    fn a_zero_window_still_yields_one_poll() {
        let window = Window::new(Some(0), Some(0));
        assert_eq!(window.window_ms, 0);
        assert_eq!(window.interval_ms, MIN_INTERVAL_MS);
        assert_eq!(window.expected_polls(), 1);
    }

    #[test]
    fn the_window_is_clamped() {
        let window = Window::new(Some(u64::MAX), Some(1));
        assert_eq!(window.window_ms, MAX_WINDOW_MS);
        assert_eq!(window.interval_ms, MIN_INTERVAL_MS);
    }

    #[test]
    fn commands_are_single_read_only_shell_lines() {
        for command in [POLL_COMMAND, POLL_COMMAND_KERNEL, BOOT_STATE_COMMAND] {
            assert!(!command.is_empty());
            assert!(!command.contains('\n'));
            assert!(!command.contains("rm "));
            assert!(!command.contains('>'));
        }
    }
}
