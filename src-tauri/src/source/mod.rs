//! Log sources: the extension point of droidlog.
//!
//! A source owns three things and nothing else:
//!
//! 1. **metadata** ([`LogSourceSpec`]) — what it is, whether it needs root, which
//!    command it runs by default;
//! 2. **command construction** ([`LogSource::command`]) — a device-side shell
//!    string built from [`SourceOptions`];
//! 3. **decoding** ([`LogSource::parse_line`]) — one line in, at most one
//!    [`LogRecord`] out.
//!
//! Adding a source (e.g. `tombstones`, `bugreport`, a file tail) means adding a
//! variant, a spec constant and a `Box<dyn LogSource>` arm in [`build`]. Nothing
//! in [`crate::process`] or the UI needs to change: the frontend renders whatever
//! [`all_specs`] returns.

pub mod kernel;
pub mod logcat;
mod special;
pub mod shell;

use serde::Serialize;

use crate::error::{DroidLogError, Result};
use crate::source::special::SpecialSource;
use crate::parser::LogRecord;

pub use kernel::{DmesgSource, KmsgSource};
pub use logcat::LogcatSource;

/// Identifies a collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSourceKind {
    /// `logcat` — userspace logs.
    Logcat,
    /// `dmesg` — kernel ring buffer, printk text form.
    Dmesg,
    /// `/proc/kmsg` — kernel ring buffer, raw record form.
    Kmsg,
    /// Post-mortem: the previous crash's logcat plus the kernel's pstore records.
    Crash,
    /// The current boot: `dmesg` polled as soon as the device appears.
    Boot,
    /// A device booted into Recovery: its shell, its logs and its pstore.
    Recovery,
}

impl LogSourceKind {
    /// Every kind, in UI display order.
    #[must_use]
    pub fn all() -> [Self; 6] {
        [
            Self::Logcat,
            Self::Dmesg,
            Self::Kmsg,
            Self::Crash,
            Self::Boot,
            Self::Recovery,
        ]
    }

    /// Parses a kind name coming from the frontend.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "logcat" => Some(Self::Logcat),
            "dmesg" => Some(Self::Dmesg),
            "kmsg" => Some(Self::Kmsg),
            "crash" | "crashlog" => Some(Self::Crash),
            "boot" | "bootlog" => Some(Self::Boot),
            "recovery" => Some(Self::Recovery),
            _ => None,
        }
    }
}

impl std::fmt::Display for LogSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Logcat => "logcat",
            Self::Dmesg => "dmesg",
            Self::Kmsg => "kmsg",
            Self::Crash => "crash",
            Self::Boot => "boot",
            Self::Recovery => "recovery",
        })
    }
}

/// logcat ring buffers, mirroring `logcat -b <name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogcatBuffer {
    /// App and framework logs (the default).
    Main,
    /// System server and low-level framework logs.
    System,
    /// Crash dumps.
    Crash,
    /// `logcat -b events` binary event stream, rendered as text.
    Events,
    /// Radio and telephony.
    Radio,
    /// Kernel messages as relayed by logd.
    Kernel,
    /// Security / SELinux.
    Security,
    /// Statsd.
    Stats,
}

impl LogcatBuffer {
    /// The `-b` argument value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::System => "system",
            Self::Crash => "crash",
            Self::Events => "events",
            Self::Radio => "radio",
            Self::Kernel => "kernel",
            Self::Security => "security",
            Self::Stats => "stats",
        }
    }

    /// Buffers captured when the user expresses no preference.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        vec![Self::Main, Self::System, Self::Crash]
    }
}

/// Which decoder a source's output needs.
///
/// Exposed as part of the source abstraction (and to the frontend) so a source
/// can be described completely by data: id, label, command, root requirement and
/// parser. A custom command reuses the parser of the source it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParserKind {
    /// `<date> <time> <pid> <tid> <level> <tag>: <message>`.
    LogcatThreadtime,
    /// `[<uptime>] <subsystem>: <message>`.
    KernelDmesg,
    /// `<level>,<seq>,<usec>,<flags>;<message>`.
    KernelKmsg,
    /// Either grammar; used by the post-mortem collectors, whose probes mix
    /// `logcat` output with raw kernel text.
    Auto,
}

impl ParserKind {
    /// Short label for the UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LogcatThreadtime => "logcat threadtime",
            Self::Auto => "自动识别",
            Self::KernelDmesg => "kernel dmesg",
            Self::KernelKmsg => "kernel kmsg",
        }
    }
}

/// Per-session knobs handed to [`LogSource::command`].
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceOptions {
    /// Restrict to these PIDs where the source supports it.
    #[serde(default)]
    pub pids: Vec<i32>,
    /// Restrict to this uid where the source supports it.
    ///
    /// Preferred over [`SourceOptions::pids`] for logcat because a uid survives
    /// an app restart and a pid does not.
    #[serde(default)]
    pub uid: Option<i32>,
    /// Restrict to these logcat tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// logcat ring buffers; empty means [`LogcatBuffer::defaults`].
    #[serde(default)]
    pub buffers: Vec<LogcatBuffer>,
    /// Run this device-side command instead of the source's built-in one.
    ///
    /// The source still supplies the parser, so a custom command only makes
    /// sense when its output matches that grammar; the UI says as much.
    #[serde(default)]
    pub custom_command: Option<String>,
}

impl SourceOptions {
    /// Options that capture everything the source can produce.
    #[must_use]
    pub fn unrestricted() -> Self {
        Self::default()
    }

    /// The custom command, trimmed and rejected when blank.
    #[must_use]
    pub fn custom_command(&self) -> Option<&str> {
        self.custom_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
    }

    /// The effective buffer list, falling back to the defaults.
    #[must_use]
    pub fn effective_buffers(&self) -> Vec<LogcatBuffer> {
        if self.buffers.is_empty() {
            LogcatBuffer::defaults()
        } else {
            self.buffers.clone()
        }
    }

    /// `-b main,system,crash`, or nothing when no buffers apply.
    #[must_use]
    pub fn buffer_args(&self) -> Vec<String> {
        let buffers = self.effective_buffers();
        if buffers.is_empty() {
            return Vec::new();
        }
        let joined = buffers
            .iter()
            .map(|buffer| buffer.as_str())
            .collect::<Vec<_>>()
            .join(",");
        vec!["-b".to_owned(), joined]
    }
}

/// Static metadata describing a source.
///
/// Fields are `&'static str` so that the whole table can live in a `const`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSourceSpec {
    /// Stable identifier.
    pub kind: LogSourceKind,
    /// Short human label for the left rail.
    pub label: &'static str,
    /// One-line explanation.
    pub description: &'static str,
    /// True when the source cannot work without `su`.
    pub requires_root: bool,
    /// True when the source only makes sense on a device in Recovery mode.
    pub requires_recovery: bool,

    /// The command run when no options are given.
    pub default_command: &'static str,
    /// Which decoder the output needs.
    pub parser: ParserKind,
    /// Whether [`SourceOptions::custom_command`] is honoured for this source.
    pub supports_custom_command: bool,
    /// Where the records come from, shown in the UI as a hint.
    pub origin: &'static str,
}

/// A source spec plus whether the currently selected device can run it.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceAvailability {
    /// The underlying metadata.
    pub spec: LogSourceSpec,
    /// False when the device lacks the required privilege or the source is absent.
    pub available: bool,
    /// Why it is unavailable, when it is.
    pub unavailable_reason: Option<&'static str>,
}

impl SourceAvailability {
    /// Marks a spec available.
    #[must_use]
    pub fn available(spec: LogSourceSpec) -> Self {
        Self {
            spec,
            available: true,
            unavailable_reason: None,
        }
    }

    /// Marks a spec unavailable with an explanation.
    #[must_use]
    pub fn unavailable(spec: LogSourceSpec, reason: &'static str) -> Self {
        Self {
            spec,
            available: false,
            unavailable_reason: Some(reason),
        }
    }
}

/// A collector implementation.
pub trait LogSource: Send + Sync {
    /// Which kind this instance implements.
    fn kind(&self) -> LogSourceKind;

    /// Static metadata.
    fn spec(&self) -> &'static LogSourceSpec;

    /// Builds the device-side shell command for this session.
    ///
    /// Implementations must honour [`SourceOptions::custom_command`] when the
    /// spec reports `supports_custom_command`.
    fn command(&self, options: &SourceOptions) -> String;

    /// Decodes one line of output into a record.
    ///
    /// Returns a record **unconditionally**: a line that does not match the
    /// grammar becomes a raw record via [`LogRecord::raw_line`]. Dropping lines
    /// here would silently lose device output, which is the one thing a log
    /// collector must never do.
    fn parse_line(&self, line: &str, seq: u64) -> LogRecord;
}

/// The command to run, preferring a validated custom override.
///
/// Kept here rather than duplicated in every source so the precedence rule and
/// its fallback behaviour live in exactly one place.
#[must_use]
pub fn effective_command(options: &SourceOptions, default_command: &str) -> String {
    match options.custom_command() {
        Some(custom) => custom.to_owned(),
        None => default_command.to_owned(),
    }
}

/// Longest custom command accepted.
pub const MAX_CUSTOM_COMMAND_LEN: usize = 512;

/// Validates a user-supplied device-side command before it is run.
///
/// The command is executed inside the device shell, so it is deliberately not
/// restricted to a whitelist — running arbitrary diagnostics is the point of the
/// feature. The checks here only reject values that cannot be a single command
/// line at all.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] when the command is blank, too long,
/// or contains a NUL / newline (which would smuggle in a second command in a way
/// the UI could never display).
pub fn validate_custom_command(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        return Err(DroidLogError::InvalidInput(
            "自定义命令不能为空".to_owned(),
        ));
    }
    if command.chars().count() > MAX_CUSTOM_COMMAND_LEN {
        return Err(DroidLogError::InvalidInput(format!(
            "自定义命令过长（上限 {MAX_CUSTOM_COMMAND_LEN} 字符）"
        )));
    }
    if command.contains('\0') || command.contains('\n') || command.contains('\r') {
        return Err(DroidLogError::InvalidInput(
            "自定义命令不能包含空字符或换行".to_owned(),
        ));
    }
    Ok(())
}

/// Metadata for `logcat`.
const LOGCAT_SPEC: LogSourceSpec = LogSourceSpec {
    kind: LogSourceKind::Logcat,
    label: "logcat",
    description: "用户态日志：应用、框架与系统服务",
    requires_root: false,
    requires_recovery: false,

    default_command: "logcat -v threadtime",
    parser: ParserKind::LogcatThreadtime,
    supports_custom_command: true,
    origin: "logd",
};

/// Metadata for `dmesg`.
const DMESG_SPEC: LogSourceSpec = LogSourceSpec {
    kind: LogSourceKind::Dmesg,
    label: "dmesg",
    description: "内核环形缓冲区（printk 文本格式）",
    requires_root: true,
    requires_recovery: false,

    default_command: "dmesg -w",
    parser: ParserKind::KernelDmesg,
    supports_custom_command: true,
    origin: "dmesg",
};

/// Metadata for `/dev/kmsg`.
const KMSG_SPEC: LogSourceSpec = LogSourceSpec {
    kind: LogSourceKind::Kmsg,
    label: "kmsg",
    description: "内核环形缓冲区（/dev/kmsg 结构化记录格式，含内核时间戳）",
    requires_root: true,
    requires_recovery: false,

    default_command: "cat /dev/kmsg",
    parser: ParserKind::KernelKmsg,
    supports_custom_command: true,
    origin: "/dev/kmsg",
};

/// Metadata for the post-mortem collector.
const CRASH_SPEC: LogSourceSpec = LogSourceSpec {
    kind: LogSourceKind::Crash,
    label: "崩溃日志",
    description: "上次崩溃：logcat -L 与 pstore / last_kmsg 持久化记录",
    requires_root: false,
    requires_recovery: false,
    default_command: "logcat -L -d; cat /sys/fs/pstore/console-ramoops-0; cat /proc/last_kmsg",
    parser: ParserKind::Auto,
    supports_custom_command: false,
    origin: "logcat -L / pstore / last_kmsg",
};

/// Metadata for the boot-time collector.
const BOOT_SPEC: LogSourceSpec = LogSourceSpec {
    kind: LogSourceKind::Boot,
    label: "开机日志",
    description: "本次开机：设备一出现就轮询 dmesg，抢在缓冲区被覆写之前",
    requires_root: false,
    requires_recovery: false,
    default_command: "dmesg",
    parser: ParserKind::Auto,
    supports_custom_command: false,
    origin: "dmesg (polled)",
};

/// Metadata for the recovery collector.
const RECOVERY_SPEC: LogSourceSpec = LogSourceSpec {
    kind: LogSourceKind::Recovery,
    label: "Recovery 日志",
    description: "Recovery 模式：dmesg、logcat -d、/tmp/recovery.log 与 pstore",
    requires_root: false,
    requires_recovery: true,
    default_command: "dmesg; logcat -d; cat /tmp/recovery.log; cat /sys/fs/pstore/console-ramoops-0",
    parser: ParserKind::Auto,
    supports_custom_command: false,
    origin: "recovery shell / pstore",
};
/// Every source's metadata, in UI display order.
static SPECS: [LogSourceSpec; 6] = [
    LOGCAT_SPEC,
    DMESG_SPEC,
    KMSG_SPEC,
    CRASH_SPEC,
    BOOT_SPEC,
    RECOVERY_SPEC,
];

/// Reason reported when a rooted source is selected without root.
pub const ROOT_REQUIRED_REASON: &str = "需要切换到 Root 模式（su -c）";

/// Reason reported when a recovery-only source is selected without a recovery device.
pub const RECOVERY_REQUIRED_REASON: &str = "仅当设备处于 Recovery 模式时可用";

/// All source metadata.
#[must_use]
pub fn all_specs() -> &'static [LogSourceSpec] {
    &SPECS
}

/// Metadata for one kind.
///
/// Total by construction: the match is exhaustive over [`LogSourceKind`], so no
/// lookup can fail.
#[must_use]
pub fn spec(kind: LogSourceKind) -> &'static LogSourceSpec {
    match kind {
        LogSourceKind::Logcat => &LOGCAT_SPEC,
        LogSourceKind::Dmesg => &DMESG_SPEC,
        LogSourceKind::Kmsg => &KMSG_SPEC,
        LogSourceKind::Crash => &CRASH_SPEC,
        LogSourceKind::Boot => &BOOT_SPEC,
        LogSourceKind::Recovery => &RECOVERY_SPEC,
    }
}

/// Builds the collector for `kind`.
#[must_use]
pub fn build(kind: LogSourceKind) -> Box<dyn LogSource> {
    match kind {
        LogSourceKind::Logcat => Box::new(LogcatSource::new()),
        LogSourceKind::Dmesg => Box::new(DmesgSource::new()),
        LogSourceKind::Kmsg => Box::new(KmsgSource::new()),
        LogSourceKind::Crash | LogSourceKind::Boot | LogSourceKind::Recovery => {
            Box::new(SpecialSource::new(kind))
        }
    }
}

/// Availability of every source for a device with the given root capability.
#[must_use]
pub fn availability(root_available: bool, recovery: bool) -> Vec<SourceAvailability> {
    SPECS
        .iter()
        .map(|spec| {
            if spec.requires_recovery && !recovery {
                SourceAvailability::unavailable(*spec, RECOVERY_REQUIRED_REASON)
            } else if spec.requires_root && !root_available {
                SourceAvailability::unavailable(*spec, ROOT_REQUIRED_REASON)
            } else {
                SourceAvailability::available(*spec)
            }
        })
        .collect()
}

/// Resolves a source name coming from the frontend.
///
/// # Errors
///
/// Returns [`DroidLogError::UnknownSource`] for an unknown name.
pub fn parse_kind(name: &str) -> Result<LogSourceKind> {
    LogSourceKind::from_name(name).ok_or_else(|| DroidLogError::UnknownSource(name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_spec() {
        for kind in LogSourceKind::all() {
            let spec = spec(kind);
            assert_eq!(spec.kind, kind);
            assert!(!spec.label.is_empty(), "{kind} has no label");
            assert!(
                !spec.default_command.is_empty(),
                "{kind} has no default command"
            );
        }
    }

    #[test]
    fn all_specs_covers_every_kind_once() {
        assert_eq!(all_specs().len(), LogSourceKind::all().len());
        for kind in LogSourceKind::all() {
            let matches = all_specs().iter().filter(|s| s.kind == kind).count();
            assert_eq!(matches, 1, "{kind} should appear exactly once");
        }
    }

    #[test]
    fn kernel_sources_require_root_but_logcat_does_not() {
        assert!(!spec(LogSourceKind::Logcat).requires_root);
        assert!(spec(LogSourceKind::Dmesg).requires_root);
        assert!(spec(LogSourceKind::Kmsg).requires_root);
    }

    #[test]
    fn availability_gates_rooted_and_recovery_sources() {
        // A normal, unrooted device: capture sources are offered, kernel ones
        // are not, and the recovery collector waits for recovery mode.
        let plain = availability(false, false);
        assert_eq!(plain.len(), LogSourceKind::all().len());
        for kind in [
            LogSourceKind::Logcat,
            LogSourceKind::Crash,
            LogSourceKind::Boot,
        ] {
            let entry = plain.iter().find(|entry| entry.spec.kind == kind);
            assert_eq!(entry.map(|entry| entry.available), Some(true), "{kind}");
        }
        for kind in [
            LogSourceKind::Dmesg,
            LogSourceKind::Kmsg,
            LogSourceKind::Recovery,
        ] {
            let entry = plain.iter().find(|entry| entry.spec.kind == kind);
            assert_eq!(entry.map(|entry| entry.available), Some(false), "{kind}");
            assert!(
                entry.and_then(|entry| entry.unavailable_reason).is_some(),
                "{kind} should explain itself"
            );
        }

        // Root unlocks the kernel sources, but not the recovery collector.
        let rooted = availability(true, false);
        for kind in [
            LogSourceKind::Logcat,
            LogSourceKind::Dmesg,
            LogSourceKind::Kmsg,
            LogSourceKind::Crash,
            LogSourceKind::Boot,
        ] {
            let entry = rooted.iter().find(|entry| entry.spec.kind == kind);
            assert_eq!(entry.map(|entry| entry.available), Some(true), "{kind}");
        }
        let recovery = rooted
            .iter()
            .find(|entry| entry.spec.kind == LogSourceKind::Recovery);
        assert_eq!(recovery.map(|entry| entry.available), Some(false));

        // A device in recovery mode: only the recovery collector can work, and
        // the reason shown for the others is still the recovery one.
        let in_recovery = availability(false, true);
        let recovery = in_recovery
            .iter()
            .find(|entry| entry.spec.kind == LogSourceKind::Recovery);
        assert_eq!(recovery.map(|entry| entry.available), Some(true));
    }

    #[test]
    fn only_the_recovery_collector_requires_recovery_mode() {
        for spec in all_specs() {
            assert_eq!(
                spec.requires_recovery,
                spec.kind == LogSourceKind::Recovery,
                "{}",
                spec.kind
            );
        }
    }

    #[test]
    fn kind_names_round_trip() {
        for kind in LogSourceKind::all() {
            assert_eq!(LogSourceKind::from_name(&kind.to_string()), Some(kind));
        }
        assert_eq!(
            LogSourceKind::from_name("LOGCAT"),
            Some(LogSourceKind::Logcat)
        );
        assert_eq!(parse_kind("nope").map_err(|e| e.kind()), Err("unknownSource"));
    }

    #[test]
    fn buffer_args_default_and_custom() {
        assert_eq!(
            SourceOptions::unrestricted().buffer_args(),
            vec!["-b".to_owned(), "main,system,crash".to_owned()]
        );

        let radio_only = SourceOptions {
            buffers: vec![LogcatBuffer::Radio],
            ..SourceOptions::default()
        };
        assert_eq!(
            radio_only.buffer_args(),
            vec!["-b".to_owned(), "radio".to_owned()]
        );
    }

    #[test]
    fn specs_serialise_camel_case() -> Result<()> {
        let json = serde_json::to_value(spec(LogSourceKind::Dmesg))?;
        assert!(json.get("requiresRoot").is_some());
        assert!(json.get("defaultCommand").is_some());
        Ok(())
    }

    #[test]
    fn build_returns_matching_kind() {
        for kind in LogSourceKind::all() {
            assert_eq!(build(kind).kind(), kind);
        }
    }

    #[test]
    fn source_options_deserialise_from_frontend_payload() -> Result<()> {
        let options: SourceOptions =
            serde_json::from_str(r#"{"pids":[123],"tags":["ActivityManager"],"buffers":["radio"]}"#)?;
        assert_eq!(options.pids, vec![123]);
        assert_eq!(options.buffers, vec![LogcatBuffer::Radio]);
        Ok(())
    }
}
