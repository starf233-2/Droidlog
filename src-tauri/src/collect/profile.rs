//! What this particular device can actually give us.
//!
//! Kernel and ROM generations moved the previous-boot log around, and the
//! difference is not cosmetic:
//!
//! * **≤ 3.4** kept it in `/proc/last_kmsg`;
//! * **≥ 3.5** (pstore/ramoops) keeps it under `/sys/fs/pstore`, as
//!   `console-ramoops`, `console-ramoops-0` or `pmsg-ramoops-0` — `/proc/last_kmsg`
//!   was removed, so probing it on a modern phone can only ever fail.
//!
//! The logcat buffers are the same story in miniature: a ROM may ship `main`,
//! `system`, `crash` and `kernel` but no `events` buffer at all, and
//! `logcat -b events` then fails with `Logcat read failure: No such file or
//! directory`. Asking first turns a failed probe into a piece of information.
//!
//! Both answers come from two read-only commands, so every collection run starts
//! by asking.

use crate::adb::Adb;
use crate::executor::ExecMode;

/// Where the previous boot's kernel log lives, by kernel generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelEra {
    /// ≤ 3.4: `/proc/last_kmsg`.
    LegacyLastKmsg,
    /// ≥ 3.5: pstore/ramoops under `/sys/fs/pstore`.
    Pstore,
    /// `uname` did not answer: read both, modern first.
    Unknown,
}

impl KernelEra {
    /// Label for the report.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LegacyLastKmsg => "≤3.4 内核：/proc/last_kmsg",
            Self::Pstore => "≥3.5 内核：pstore/ramoops（/sys/fs/pstore）",
            Self::Unknown => "内核版本未知：两条路径都试",
        }
    }

    /// Whether the legacy `/proc/last_kmsg` probe is worth running.
    #[must_use]
    pub fn reads_last_kmsg(self) -> bool {
        matches!(self, Self::LegacyLastKmsg | Self::Unknown)
    }

    /// Whether the pstore directory is worth reading.
    #[must_use]
    pub fn reads_pstore(self) -> bool {
        matches!(self, Self::Pstore | Self::Unknown)
    }
}

/// Commands run to build a profile. Both are read-only and cheap.
const KERNEL_COMMAND: &str = "uname -r";
/// `logcat -g` lists each buffer with its ring size.
const BUFFERS_COMMAND: &str = "logcat -g";

/// What the device told us about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// Raw `uname -r`, when it answered.
    pub kernel_raw: Option<String>,
    /// Parsed `(major, minor)`.
    pub kernel: Option<(u32, u32)>,
    /// Which storage generation the kernel belongs to.
    pub era: KernelEra,
    /// logcat buffers the ROM actually has, as reported by `logcat -g`.
    pub logcat_buffers: Vec<String>,
    /// One line per conclusion, for the report.
    pub notes: Vec<String>,
}

impl DeviceProfile {
    /// A profile for a device that answered nothing: probe both eras, assume the
    /// standard buffers.
    #[must_use]
    pub fn unknown(note: impl Into<String>) -> Self {
        Self {
            kernel_raw: None,
            kernel: None,
            era: KernelEra::Unknown,
            logcat_buffers: Vec::new(),
            notes: vec![note.into()],
        }
    }

    /// Whether the device has a given logcat buffer.
    ///
    /// An unknown buffer list answers `true` for the standard buffers: refusing to
    /// probe because `logcat -g` itself failed would hide the logs, which is the
    /// opposite of the intent. Only a *known* list may exclude a buffer.
    #[must_use]
    pub fn has_buffer(&self, name: &str) -> bool {
        if self.logcat_buffers.is_empty() {
            return matches!(name, "main" | "system" | "crash");
        }
        self.logcat_buffers.iter().any(|buffer| buffer == name)
    }

    /// Human summary of the buffer list, for the report.
    #[must_use]
    pub fn buffers_label(&self) -> String {
        if self.logcat_buffers.is_empty() {
            "未知（logcat -g 无输出）".to_owned()
        } else {
            self.logcat_buffers.join(" / ")
        }
    }
}

/// `uname -r` output → `(major, minor)`.
///
/// Releases read like `5.10.302-qgki-odin-QiuChenly`, `4.14.190-gabcdef`, `3.4.0`.
/// Anything unparsable returns `None` so the caller can fall back to trying both
/// generations rather than guessing.
#[must_use]
pub fn parse_kernel_release(text: &str) -> Option<(u32, u32)> {
    let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut parts = first.split(['.', '-', '_', ' ']);
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor_text = parts.next()?;
    // `5.10.302` → minor is the digits before the next dot; `5.10` → all of it.
    let minor_digits: String = minor_text
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let minor = minor_digits.parse::<u32>().ok()?;
    Some((major, minor))
}

/// Maps a parsed kernel version onto its storage generation.
#[must_use]
pub fn era_of(version: Option<(u32, u32)>) -> KernelEra {
    match version {
        // 3.5 introduced pstore; 3.4 and older only have last_kmsg.
        Some((major, minor)) => {
            if (major, minor) > (3, 4) {
                KernelEra::Pstore
            } else {
                KernelEra::LegacyLastKmsg
            }
        }
        None => KernelEra::Unknown,
    }
}

/// `logcat -g` output → buffer names.
///
/// Lines look like `main: ring buffer is 2 MiB (1 MiB consumed, 9 MiB readable),
/// max entry is 5120 B, max payload is 4068 B`. Only the part before the first
/// colon is a buffer name; anything else (a `logcat: …` error line, banner text)
/// is ignored, which is what keeps a failed `logcat -g` from inventing buffers.
#[must_use]
pub fn parse_logcat_buffers(text: &str) -> Vec<String> {
    let mut buffers: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !rest.contains("ring buffer") {
            continue;
        }
        let valid = !name.is_empty()
            && name.len() <= 16
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit());
        if valid && !buffers.iter().any(|known| known == name) {
            buffers.push(name.to_owned());
        }
    }
    buffers
}

/// Asks the device for its kernel release and its logcat buffers.
///
/// Never fails: a device that cannot answer yields
/// [`KernelEra::Unknown`], which reads both generations rather than none.
pub async fn detect(adb: &Adb, mode: ExecMode, serial: &str) -> DeviceProfile {
    let target = adb.target(mode, Some(serial));

    let kernel_raw = target
        .run_tolerant(KERNEL_COMMAND)
        .await
        .ok()
        .map(|output| output.stdout.trim().to_owned())
        .filter(|text| !text.is_empty());
    let kernel = kernel_raw.as_deref().and_then(parse_kernel_release);
    let era = era_of(kernel);

    let logcat_buffers = target
        .run_tolerant(BUFFERS_COMMAND)
        .await
        .ok()
        .map(|output| parse_logcat_buffers(&output.stdout))
        .unwrap_or_default();

    let mut notes = Vec::new();
    match kernel {
        Some((major, minor)) => {
            notes.push(format!(
                "内核 {}（{}.{}）：{}",
                kernel_raw.as_deref().unwrap_or("?"),
                major,
                minor,
                era.label()
            ));
        }
        None => notes.push(
            "无法读取内核版本（uname -r 无输出）：将同时尝试 pstore 与 /proc/last_kmsg".to_owned(),
        ),
    }
    if logcat_buffers.is_empty() {
        notes.push("logcat -g 没有列出缓冲区：按标准缓冲区探测".to_owned());
    } else {
        notes.push(format!("可用 logcat 缓冲区：{}", logcat_buffers.join(" / ")));
    }

    DeviceProfile {
        kernel_raw,
        kernel,
        era,
        logcat_buffers,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_releases_are_parsed_from_real_shapes() {
        // The exact string a Xiaomi MIX 4 on a 5.10 QGKI kernel reports.
        assert_eq!(
            parse_kernel_release("5.10.302-qgki-odin-QiuChenly\n"),
            Some((5, 10))
        );
        assert_eq!(parse_kernel_release("4.14.190-gabcdef"), Some((4, 14)));
        assert_eq!(parse_kernel_release("3.4.0"), Some((3, 4)));
        assert_eq!(parse_kernel_release("6.1.25-android14"), Some((6, 1)));
        assert_eq!(parse_kernel_release("  5.15.94  \n"), Some((5, 15)));
    }

    #[test]
    fn unparsable_kernel_output_is_none() {
        assert_eq!(parse_kernel_release(""), None);
        assert_eq!(parse_kernel_release("\n \n"), None);
        assert_eq!(parse_kernel_release("uname: not found"), None);
        assert_eq!(parse_kernel_release("Linux"), None);
    }

    #[test]
    fn era_splits_at_3_4() {
        assert_eq!(era_of(Some((3, 4))), KernelEra::LegacyLastKmsg);
        assert_eq!(era_of(Some((3, 0))), KernelEra::LegacyLastKmsg);
        assert_eq!(era_of(Some((3, 5))), KernelEra::Pstore);
        assert_eq!(era_of(Some((5, 10))), KernelEra::Pstore);
        assert_eq!(era_of(None), KernelEra::Unknown);
        assert!(KernelEra::Pstore.reads_pstore());
        assert!(!KernelEra::Pstore.reads_last_kmsg());
        assert!(KernelEra::LegacyLastKmsg.reads_last_kmsg());
        assert!(!KernelEra::LegacyLastKmsg.reads_pstore());
        assert!(KernelEra::Unknown.reads_pstore() && KernelEra::Unknown.reads_last_kmsg());
    }

    #[test]
    fn logcat_buffers_are_read_from_g_output() {
        let text = "main: ring buffer is 2 MiB (1 MiB consumed, 9 MiB readable), max entry is 5120 B, max payload is 4068 B\n\
                    system: ring buffer is 2 MiB (1 MiB consumed, 6 MiB readable), max entry is 5120 B, max payload is 4068 B\n\
                    crash: ring buffer is 2 MiB (512 KiB consumed, 666 B readable), max entry is 5120 B, max payload is 4068 B\n\
                    kernel: ring buffer is 2 MiB (846 KiB consumed, 2 MiB readable), max entry is 5120 B, max payload is 4068 B\n";
        assert_eq!(
            parse_logcat_buffers(text),
            vec!["main", "system", "crash", "kernel"]
        );
    }

    #[test]
    fn logcat_errors_do_not_invent_buffers() {
        // A failed `logcat -g` on a device without the buffer must not register one.
        assert!(parse_logcat_buffers("logcat: Logcat read failure: No such file or directory\n").is_empty());
        assert!(parse_logcat_buffers("").is_empty());
        assert!(parse_logcat_buffers("usage: logcat [options] [filterspecs]\n").is_empty());
    }

    #[test]
    fn a_known_buffer_list_decides_availability() {
        let profile = DeviceProfile {
            kernel_raw: Some("5.10.302-qgki-odin".to_owned()),
            kernel: Some((5, 10)),
            era: KernelEra::Pstore,
            logcat_buffers: vec!["main".to_owned(), "system".to_owned(), "crash".to_owned(), "kernel".to_owned()],
            notes: Vec::new(),
        };
        assert!(profile.has_buffer("crash"));
        assert!(profile.has_buffer("kernel"));
        // The device really has no events buffer; asking for it fails.
        assert!(!profile.has_buffer("events"));
        assert_eq!(profile.buffers_label(), "main / system / crash / kernel");
    }

    #[test]
    fn an_unknown_buffer_list_still_probes_the_standard_buffers() {
        let profile = DeviceProfile::unknown("no answer");
        assert!(profile.has_buffer("crash"));
        assert!(profile.has_buffer("system"));
        assert!(profile.has_buffer("main"));
        // But not a speculative one.
        assert!(!profile.has_buffer("security"));
        assert!(profile.buffers_label().contains("未知"));
    }
}
