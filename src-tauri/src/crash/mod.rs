//! Crash classification: what kind of failure a log line describes.
//!
//! A log stream tells you *that* something happened; this module answers *what*.
//! It is a pure text classifier with no device access, which is what makes it
//! testable — every rule below has a test built from a real message shape, and a
//! negative test guards against classifying ordinary chatter.
//!
//! The five families the UI distinguishes are the ones that actually end a boot
//! or kill a process on Android:
//!
//! | kind | what it means |
//! | --- | --- |
//! | kernel panic | the kernel gave up; `console-ramoops` will have the tail |
//! | oops | a kernel fault the kernel survived (often followed by a panic) |
//! | kernel BUG | an assertion inside the kernel (`BUG:` / `BUG_ON`) |
//! | watchdog | a CPU or subsystem stopped answering; often reboots the device |
//! | low memory | `lowmemorykiller`/`lmkd` killed a process to reclaim RAM |
//! | system server | `system_server` itself died — the device soft-reboots |
//! | native crash | a native process crashed (`Fatal signal`, tombstone) |
//!
//! Rules are checked most-specific first: an `lmkd` message that also contains
//! the word "watchdog" is a low-memory kill, not a watchdog bite, and a
//! `system_server` crash that prints a `Fatal signal` is a system-server crash
//! rather than a generic native one.

use serde::{Deserialize, Serialize};

use crate::parser::{LogLevel, LogRecord};

/// The failure families the UI marks and lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrashKind {
    /// The kernel halted: `Kernel panic - not syncing`.
    KernelPanic,
    /// A kernel fault that was handled: `Unable to handle kernel`, `Internal error`.
    Oops,
    /// A kernel assertion: `kernel BUG at`, `BUG: unable to handle`.
    KernelBug,
    /// A watchdog expired: hard/soft lockup, `Watchdog bite`.
    Watchdog,
    /// A process was killed to reclaim memory: `lowmemorykiller`, `lmkd`.
    LowMemory,
    /// The system did not answer in time: an ANR, in the app's own log or in the
    /// framework's `am_anr` event.
    Anr,
    /// `system_server` died, which restarts the Android runtime.
    SystemServer,
    /// A native process crashed: `Fatal signal`, tombstone, backtrace.
    NativeCrash,
}

impl CrashKind {
    /// Stable identifier, used by the frontend for icons and filtering.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::KernelPanic => "kernelPanic",
            Self::Oops => "oops",
            Self::KernelBug => "kernelBug",
            Self::Watchdog => "watchdog",
            Self::LowMemory => "lowMemory",
            Self::Anr => "anr",
            Self::SystemServer => "systemServer",
            Self::NativeCrash => "nativeCrash",
        }
    }

    /// Human label for the crash panel.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::KernelPanic => "内核 Panic",
            Self::Oops => "内核 Oops",
            Self::KernelBug => "内核 BUG",
            Self::Watchdog => "看门狗",
            Self::LowMemory => "低内存杀进程",
            Self::Anr => "ANR 无响应",
            Self::SystemServer => "系统服务崩溃",
            Self::NativeCrash => "Native 崩溃",
        }
    }

    /// Short badge for the log table's marker column.
    #[must_use]
    pub fn badge(self) -> char {
        match self {
            Self::KernelPanic => 'P',
            Self::Oops => 'O',
            Self::KernelBug => 'B',
            Self::Watchdog => 'W',
            Self::LowMemory => 'M',
            Self::Anr => 'A',
            Self::SystemServer => 'S',
            Self::NativeCrash => 'N',
        }
    }

    /// Parses an identifier produced by [`CrashKind::id`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::KernelPanic,
            Self::Oops,
            Self::KernelBug,
            Self::Watchdog,
            Self::LowMemory,
            Self::Anr,
            Self::SystemServer,
            Self::NativeCrash,
        ]
        .into_iter()
        .find(|kind| kind.id() == name)
    }

    /// Every kind, in display order.
    #[must_use]
    pub fn all() -> [Self; 8] {
        [
            Self::KernelPanic,
            Self::Oops,
            Self::KernelBug,
            Self::Watchdog,
            Self::LowMemory,
            Self::Anr,
            Self::SystemServer,
            Self::NativeCrash,
        ]
    }
}

impl std::fmt::Display for CrashKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// True when `haystack` contains any of `needles`.
fn any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Classifies one log line, or `None` when it describes nothing unusual.
///
/// The whole line is searched, not just the message: kernel output often puts the
/// decisive phrase in the tag (`Kernel panic`) and the payload in the message, and
/// tombstone headers arrive as ordinary-looking lines whose *message* carries
/// `Fatal signal`.
#[must_use]
pub fn classify(text: &str) -> Option<CrashKind> {
    let lower = text.to_lowercase();

    // Kernel panic first: everything else is survivable by comparison, and panic
    // output usually also contains words like "oops" or "BUG".
    if any(
        &lower,
        &[
            "kernel panic",
            "panic - not syncing",
            "panic_on_oops",
            "end kernel panic",
        ],
    ) && !lower.contains("no kernel panic") // a clean-boot line says this explicitly
    {
        return Some(CrashKind::KernelPanic);
    }

    // system_server death: checked before the generic native-crash rule because a
    // dying system_server prints a Fatal signal too, and the caller cares which.
    if any(
        &lower,
        &[
            "watchdog killing system process",
            "system_server died",
            "system_server crash",
            "crash of system server",
            "system server crash",
        ],
    ) {
        return Some(CrashKind::SystemServer);
    }

    if lower.contains("system_server")
        && any(
            &lower,
            &["fatal signal", "fatal exception", "died", "sigsegv", "abort"],
        )
    {
        return Some(CrashKind::SystemServer);
    }

    // Low memory: lmkd's own lines, the legacy driver, and the kill summary.
    if any(
        &lower,
        &[
            "lowmemorykiller",
            "lmkd",
            "low memory killer",
            "killing '",
            "to free ",
        ],
    ) && any(
        &lower,
        &[
            "kill",
            "free",
            "reclaim",
            "oom",
            "adj:",
            "lmkd",
            "lowmemorykiller",
        ],
    ) {
        return Some(CrashKind::LowMemory);
    }

    // Watchdog: a bite, a lockup, or an explicit reset request.
    if any(
        &lower,
        &[
            "watchdog bite",
            "watchdog bite!",
            "hard lockup",
            "soft lockup",
            "watchdog:",
            "watchdog reset",
            "*** watchdog",
        ],
    ) {
        return Some(CrashKind::Watchdog);
    }

    // Kernel assertions and oopses.
    if any(&lower, &["kernel bug at", "bug_on", "bug: unable to handle"]) {
        return Some(CrashKind::KernelBug);
    }

    if any(
        &lower,
        &[
            "unable to handle kernel",
            "internal error: oops",
            "oops: ",
            "bug: soft lockup",
            "kernel oops",
        ],
    ) {
        return Some(CrashKind::Oops);
    }

    // ANR: the app stopped answering. Checked after the kernel families (an ANR
    // during a panic is not the interesting event) and before the native rule,
    // because "Application Not Responding" lines often also carry a stack dump.
    if any(
        &lower,
        &[
            "anr in ",
            "am_anr:",
            "anr: ",
            "application not responding",
            "input dispatching timed out",
        ],
    ) {
        return Some(CrashKind::Anr);
    }

    // Native crash: the tombstone header, or the signal line that precedes it.
    if any(
        &lower,
        &[
            "fatal signal",
            "fatal exception",
            "tombstone",
            "backtrace:",
            "signal 11 (sigsegv)",
            "signal 6 (sigabrt)",
            "signal 7 (sigbus)",
            "beginning of crash",
        ],
    ) {
        return Some(CrashKind::NativeCrash);
    }

    None
}

/// Classifies a parsed record, returning both the kind and the record's level.
///
/// The level is not used to decide the kind — an `lmkd` kill is logged at `I` on
/// some builds and `E` on others — but callers that want to *hide* noise can use
/// it. Kept as a separate function so the classifier stays a pure text function.
#[must_use]
pub fn classify_record(record: &LogRecord) -> Option<CrashKind> {
    let mut text = String::with_capacity(record.message.len() + record.raw.len() + 16);
    if let Some(tag) = &record.tag {
        text.push_str(tag);
        text.push(' ');
    }
    text.push_str(&record.message);
    if !record.parsed {
        text.push(' ');
        text.push_str(&record.raw);
    }
    classify(&text)
}

/// True when a probe's stderr says the read was refused rather than absent.
///
/// The distinction drives the advice the user gets: `Permission denied` means
/// "switch to Root mode", while an absent file means "this kernel does not keep
/// that record" — and those need different answers (see feature 6 of the spec).
#[must_use]
pub fn looks_like_permission_denied(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    any(
        &lower,
        &[
            "permission denied",
            "operation not permitted",
            "not permitted",
            "access denied",
            "dmesg_restrict",
        ],
    )
}

/// True when the message is the kernel telling us it has no crash to report.
#[must_use]
pub fn is_clean_boot_note(text: &str) -> bool {
    let lower = text.to_lowercase();
    any(
        &lower,
        &[
            "no kernel panic",
            "no crash",
            "no valid data",
            "record is empty",
        ],
    )
}

/// Whether a record's level is worth scanning at all.
///
/// Used to skip the classifier on the overwhelming majority of `V`/`D` lines.
/// `Unknown` is included because unparsed kernel lines are exactly where panics
/// hide — the parser marks them `parsed: false`, and their level is `Unknown`.
#[must_use]
pub fn is_scannable(level: LogLevel) -> bool {
    !matches!(level, LogLevel::Verbose | LogLevel::Debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::LogSourceKind;

    fn record(source: LogSourceKind, tag: Option<&str>, message: &str) -> LogRecord {
        let mut record = LogRecord::raw_line(source, 1, message);
        record.tag = tag.map(ToString::to_string);
        record.message = message.to_owned();
        record
    }

    #[test]
    fn kernel_panic_is_recognised() {
        assert_eq!(
            classify("Kernel panic - not syncing: Attempted to kill init!"),
            Some(CrashKind::KernelPanic)
        );
        assert_eq!(
            classify("[    2.345678] Kernel panic - not syncing: Fatal exception"),
            Some(CrashKind::KernelPanic)
        );
    }

    #[test]
    fn a_clean_boot_note_is_not_a_panic() {
        // Some kernels print this at every boot; classifying it would make the
        // crash panel useless on healthy devices.
        assert_eq!(classify("pstore: no kernel panic记录"), None);
        assert_eq!(classify("Last boot: no kernel panic"), None);
    }

    #[test]
    fn oops_and_kernel_bug_are_told_apart() {
        assert_eq!(
            classify("Unable to handle kernel paging request at virtual address 00000000"),
            Some(CrashKind::Oops)
        );
        assert_eq!(classify("Internal error: Oops: 96000004 [#1] PREEMPT SMP"), Some(CrashKind::Oops));
        assert_eq!(
            classify("kernel BUG at mm/slub.c:3413!"),
            Some(CrashKind::KernelBug)
        );
        assert_eq!(
            classify("BUG: unable to handle kernel NULL pointer dereference"),
            Some(CrashKind::KernelBug)
        );
    }

    #[test]
    fn watchdog_is_recognised() {
        assert_eq!(
            classify("watchdog: watchdog0: watchdog did not stop!"),
            Some(CrashKind::Watchdog)
        );
        assert_eq!(
            classify("[  123.456] Watchdog bite! Sending reset"),
            Some(CrashKind::Watchdog)
        );
        assert_eq!(
            classify("watchdog: BUG: soft lockup - CPU#2 stuck for 22s!"),
            Some(CrashKind::Watchdog)
        );
    }

    #[test]
    fn low_memory_kills_are_recognised() {
        assert_eq!(
            classify("lowmemorykiller: Kill 'com.example.app' (1234), adj 900, to free 65536kB"),
            Some(CrashKind::LowMemory)
        );
        assert_eq!(
            classify("lmkd: Kill 'com.example.app' (1234), uid 10123, oom_score_adj 900"),
            Some(CrashKind::LowMemory)
        );
    }

    #[test]
    fn system_server_death_outranks_a_bare_fatal_signal() {
        // A dying system_server prints both; the more specific answer wins.
        assert_eq!(
            classify("WATCHDOG KILLING SYSTEM PROCESS: Blocked in handler on main thread"),
            Some(CrashKind::SystemServer)
        );
        assert_eq!(
            classify("system_server: Fatal signal 11 (SIGSEGV), code 1, fault addr 0x0"),
            Some(CrashKind::SystemServer)
        );
    }

    #[test]
    fn native_crashes_are_recognised() {
        assert_eq!(
            classify("Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR), fault addr 0x0"),
            Some(CrashKind::NativeCrash)
        );
        assert_eq!(
            classify("*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***"),
            None // the tombstone banner alone is not decisive
        );
        assert_eq!(classify("backtrace:"), Some(CrashKind::NativeCrash));
        assert_eq!(
            classify("tombstone: /data/tombstones/tombstone_07"),
            Some(CrashKind::NativeCrash)
        );
    }

    #[test]
    fn anr_is_a_family_of_its_own() {
        // Both shapes matter: the application's own line in the crash buffer, and
        // the framework's `am_anr` event. Neither may be reported as a native
        // crash, because an ANR has no signal and no tombstone.
        assert_eq!(classify("ANR in com.example.demo"), Some(CrashKind::Anr));
        assert_eq!(
            classify("am_anr: [1234,0,com.example.demo,10201,Input dispatching timed out]"),
            Some(CrashKind::Anr)
        );
        assert_eq!(classify("Application Not Responding: com.example.demo"), Some(CrashKind::Anr));
        assert_eq!(CrashKind::Anr.id(), "anr");
        assert_eq!(CrashKind::Anr.badge(), 'A');
        assert_eq!(CrashKind::from_name("anr"), Some(CrashKind::Anr));
        assert_eq!(CrashKind::all().len(), 8);
    }

    #[test]
    fn ordinary_log_lines_are_not_crashes() {
        for line in [
            "ActivityManager: Start proc 1234:com.example/u0a123 for activity",
            "chatty: uid=10123 expire 12 lines",
            "OpenGLRenderer: Davey! duration=1234ms",
            "PhoneWindowManager: mSecureLockDisplayMode=0",
            "WifiService: mWifiLogProto.txBad=0",
        ] {
            assert_eq!(classify(line), None, "{line} must not classify");
        }
    }

    #[test]
    fn classification_reads_the_tag_as_well_as_the_message() {
        let record = record(
            LogSourceKind::Logcat,
            Some("Kernel panic"),
            "not syncing: Fatal exception",
        );
        assert_eq!(classify_record(&record), Some(CrashKind::KernelPanic));
    }

    #[test]
    fn record_classification_covers_unparsed_lines() {
        // A panic line that the grammar cannot parse still has to be classified:
        // this is why the raw text is scanned for unparsed records.
        let record = record(
            LogSourceKind::Kmsg,
            None,
            "<0>[    1.234567] Kernel panic - not syncing: hard lockup",
        );
        assert!(!record.parsed);
        assert_eq!(classify_record(&record), Some(CrashKind::KernelPanic));
    }

    #[test]
    fn verbosity_levels_are_skipped_but_unknown_is_not() {
        assert!(is_scannable(LogLevel::Info));
        assert!(is_scannable(LogLevel::Warn));
        assert!(is_scannable(LogLevel::Unknown));
        assert!(!is_scannable(LogLevel::Verbose));
        assert!(!is_scannable(LogLevel::Debug));
    }

    #[test]
    fn permission_errors_are_distinguished_from_missing_files() {
        assert!(looks_like_permission_denied("cat: /sys/fs/pstore/x: Permission denied"));
        assert!(looks_like_permission_denied("dmesg: Operation not permitted"));
        assert!(!looks_like_permission_denied("cat: /proc/last_kmsg: No such file or directory"));
        assert!(!looks_like_permission_denied(""));
    }

    #[test]
    fn ids_round_trip_and_are_unique() {
        let mut seen = Vec::new();
        for kind in CrashKind::all() {
            assert_eq!(CrashKind::from_name(kind.id()), Some(kind));
            assert_eq!(kind.badge().len_utf8(), 1);
            assert!(!kind.label().is_empty());
            assert!(!seen.contains(&kind.id()), "duplicate id {}", kind.id());
            seen.push(kind.id());
        }
        assert_eq!(CrashKind::from_name("nonsense"), None);
    }

    #[test]
    fn classification_is_camel_case_on_the_wire() -> crate::error::Result<()> {
        let json = serde_json::to_string(&CrashKind::NativeCrash)?;
        assert_eq!(json, "\"nativeCrash\"");
        assert_eq!(
            serde_json::from_str::<CrashKind>("\"systemServer\"")?,
            CrashKind::SystemServer
        );
        Ok(())
    }
}
