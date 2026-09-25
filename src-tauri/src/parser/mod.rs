//! Log line parsing.
//!
//! Every parser is a pure `&str -> Option<LogRecord>` function, which keeps them
//! unit-testable without a device and lets [`crate::source`] stay a thin shell
//! around string building.
//!
//! Unparseable lines are *not* errors: logcat and dmesg interleave banners,
//! separators and continuation lines. Returning `None` means "skip", and the
//! caller keeps streaming.

pub mod kernel;
pub mod logcat;

use serde::{Deserialize, Serialize};

use crate::source::LogSourceKind;

pub use kernel::{parse_dmesg, parse_kernel, parse_kmsg};
pub use logcat::{parse_logcat, parse_logcat_brief, parse_logcat_threadtime};

/// Severity of a log record, normalised across sources.
///
/// Ordering is meaningful: `Verbose` is least severe. `Unknown` is deliberately
/// last so that a minimum-level rule never hides a line whose severity could not
/// be determined — see [`LogLevel::passes_min`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// logcat `V` — most verbose.
    Verbose,
    /// logcat `D`.
    Debug,
    /// logcat `I`, and the default for unmarked kernel lines.
    Info,
    /// logcat `W`, kernel levels 4.
    Warn,
    /// logcat `E`, kernel levels 0-3.
    Error,
    /// logcat `F` — fatal.
    Fatal,
    /// Severity could not be determined.
    Unknown,
}

impl LogLevel {
    /// Maps a logcat severity character.
    #[must_use]
    pub fn from_logcat_char(letter: char) -> Self {
        match letter.to_ascii_uppercase() {
            'V' => Self::Verbose,
            'D' => Self::Debug,
            'I' => Self::Info,
            'W' => Self::Warn,
            'E' => Self::Error,
            'F' | 'A' => Self::Fatal,
            // 'S' means "silent": the line was suppressed, so severity is unknown.
            _ => Self::Unknown,
        }
    }

    /// Maps a kernel `printk` level to a log level.
    ///
    /// The value comes from two places that agree by accident: the `<N>` prefix
    /// on printk text, and the leading field of a `/dev/kmsg` record. A
    /// `/dev/kmsg` level is a bare severity (0-7), but the `<N>` prefix packs
    /// `facility * 8 + severity` — a Xiaomi kernel writes `<12>`, i.e. facility 1
    /// with severity 4. Reducing modulo 8 is correct for both: it is a no-op for
    /// a bare severity and extracts the severity from a packed one.
    #[must_use]
    pub fn from_kernel_digit(digit: u8) -> Self {
        match digit % 8 {
            0..=3 => Self::Error,
            4 => Self::Warn,
            5..=6 => Self::Info,
            7 => Self::Debug,
            _ => Self::Unknown,
        }
    }

    /// The single-letter badge shown in the log table.
    #[must_use]
    pub fn badge(self) -> char {
        match self {
            Self::Verbose => 'V',
            Self::Debug => 'D',
            Self::Info => 'I',
            Self::Warn => 'W',
            Self::Error => 'E',
            Self::Fatal => 'F',
            Self::Unknown => '?',
        }
    }

    /// Parses a level name coming from the frontend (`"warn"`, `"W"`, ...).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "verbose" | "v" => Some(Self::Verbose),
            "debug" | "d" => Some(Self::Debug),
            "info" | "i" => Some(Self::Info),
            "warn" | "warning" | "w" => Some(Self::Warn),
            "error" | "e" => Some(Self::Error),
            "fatal" | "f" | "assert" | "a" => Some(Self::Fatal),
            "unknown" | "?" | "" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Whether a record at this level survives a `>= min` rule.
    ///
    /// [`LogLevel::Unknown`] always passes so unclassified lines are never
    /// silently swallowed.
    #[must_use]
    pub fn passes_min(self, min: Self) -> bool {
        if self == Self::Unknown {
            return true;
        }
        self >= min
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Verbose => "verbose",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
            Self::Unknown => "unknown",
        })
    }
}

/// One decoded log line — the unit the table, the filter and the exporter share.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    /// Monotonic capture-local sequence number; the frontend keys rows by this.
    pub seq: u64,
    /// Which collector produced the line.
    pub source: LogSourceKind,
    /// Normalised severity.
    pub level: LogLevel,
    /// Device-supplied timestamp text, kept verbatim (`09-01 12:34:56.789`).
    pub timestamp: Option<String>,
    /// Kernel uptime in seconds, when the source reports one.
    pub uptime_seconds: Option<f64>,
    /// Originating process id.
    pub pid: Option<i32>,
    /// Originating thread id.
    pub tid: Option<i32>,
    /// Owning uid, when the source reports one.
    pub uid: Option<i32>,
    /// logcat tag / kernel subsystem.
    pub tag: Option<String>,
    /// Package name once a PID has been resolved to one.
    pub package: Option<String>,
    /// The message body, without the metadata prefix.
    pub message: String,
    /// The original line, so nothing is ever lost to a parser gap.
    pub raw: String,
    /// Host wall-clock time this record was decoded, in milliseconds.
    ///
    /// Deliberately the *host* clock, not the device's: a logcat timestamp is
    /// `MM-DD HH:MM:SS.mmm` with no year and no timezone, so it cannot be
    /// compared against a user-chosen instant. A rolling "last N seconds" window
    /// is what a live tail actually needs, and this is the honest input for it.
    pub received_at_ms: u64,
    /// False when the line did not match the source's grammar and was kept
    /// verbatim as a *raw* record.
    ///
    /// Unparseable lines are never dropped. logcat and dmesg interleave banners,
    /// separators, multi-line dumps and vendor-specific formats, and those are
    /// frequently the lines worth reading — silently discarding them would make
    /// the collector lie about what the device emitted.
    pub parsed: bool,
}

impl LogRecord {
    /// A record skeleton for a line that *did* match the source's grammar.
    ///
    /// Callers fill in the parsed fields; `parsed` starts as `true`.
    #[must_use]
    pub fn new(source: LogSourceKind, seq: u64, raw: &str) -> Self {
        Self {
            seq,
            source,
            level: LogLevel::Unknown,
            timestamp: None,
            uptime_seconds: None,
            pid: None,
            tid: None,
            uid: None,
            tag: None,
            package: None,
            message: String::new(),
            raw: raw.to_owned(),
            received_at_ms: crate::process::now_ms(),
            parsed: true,
        }
    }

    /// A record for a line the source's parser could not decode.
    ///
    /// The whole line is preserved in both `raw` and `message`, so it survives
    /// to the table, the ring buffer and any future export unchanged.
    #[must_use]
    pub fn raw_line(source: LogSourceKind, seq: u64, line: &str) -> Self {
        Self {
            seq,
            source,
            level: LogLevel::Unknown,
            timestamp: None,
            uptime_seconds: None,
            pid: None,
            tid: None,
            uid: None,
            tag: None,
            package: None,
            message: line.to_owned(),
            raw: line.to_owned(),
            received_at_ms: crate::process::now_ms(),
            parsed: false,
        }
    }

    /// A single-line rendering used by the text/CSV exporters.
    #[must_use]
    pub fn to_export_line(&self) -> String {
        let stamp = self.timestamp.as_deref().unwrap_or("-");
        let pid = self.pid.map_or_else(|| "-".to_owned(), |v| v.to_string());
        let tid = self.tid.map_or_else(|| "-".to_owned(), |v| v.to_string());
        let tag = self.tag.as_deref().unwrap_or("-");
        format!(
            "{stamp} {level} {pid} {tid} {tag}: {message}",
            level = self.level.badge(),
            message = self.message
        )
    }
}

/// Decodes a line without being told which grammar it uses.
///
/// The post-mortem collectors read from sources that disagree about format:
/// `logcat -L -d` prints threadtime records, `pstore`/`last_kmsg` print raw kernel
/// text, and `dmesg` prints the bracketed printk form. Each grammar is tried in
/// turn — strictest first, so a threadtime line is never mistaken for kernel text
/// — and anything that matches nothing is kept as a raw record rather than
/// dropped, exactly like the single-grammar parsers.
#[must_use]
pub fn parse_auto(source: LogSourceKind, line: &str, seq: u64) -> LogRecord {
    if let Some(record) = crate::parser::logcat::parse_logcat(line, seq) {
        return retag(record, source);
    }
    if let Some(record) = crate::parser::kernel::parse_dmesg(line, seq) {
        return retag(record, source);
    }
    LogRecord::raw_line(source, seq, line)
}

/// Rewrites a parsed record's source to the orchestrated kind.
fn retag(mut record: LogRecord, source: LogSourceKind) -> LogRecord {
    record.source = source;
    record
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logcat_letters_map_to_levels() {
        assert_eq!(LogLevel::from_logcat_char('V'), LogLevel::Verbose);
        assert_eq!(LogLevel::from_logcat_char('d'), LogLevel::Debug);
        assert_eq!(LogLevel::from_logcat_char('I'), LogLevel::Info);
        assert_eq!(LogLevel::from_logcat_char('W'), LogLevel::Warn);
        assert_eq!(LogLevel::from_logcat_char('E'), LogLevel::Error);
        assert_eq!(LogLevel::from_logcat_char('F'), LogLevel::Fatal);
        assert_eq!(LogLevel::from_logcat_char('S'), LogLevel::Unknown);
    }

    #[test]
    fn kernel_digits_follow_printk_levels() {
        assert_eq!(LogLevel::from_kernel_digit(0), LogLevel::Error);
        assert_eq!(LogLevel::from_kernel_digit(3), LogLevel::Error);
        assert_eq!(LogLevel::from_kernel_digit(4), LogLevel::Warn);
        assert_eq!(LogLevel::from_kernel_digit(6), LogLevel::Info);
        assert_eq!(LogLevel::from_kernel_digit(7), LogLevel::Debug);
    }

    #[test]
    fn packed_facility_and_severity_are_reduced_to_severity() {
        // Observed on a Xiaomi kernel: `<12>` is facility 1 + severity 4 (warn).
        assert_eq!(LogLevel::from_kernel_digit(12), LogLevel::Warn);
        // `<14>` = facility 1 + severity 6 (info).
        assert_eq!(LogLevel::from_kernel_digit(14), LogLevel::Info);
        // `<9>` = facility 1 + severity 1 (alert) -> error.
        assert_eq!(LogLevel::from_kernel_digit(9), LogLevel::Error);
        // Bare severities are unaffected by the reduction.
        for digit in 0..=7u8 {
            assert_eq!(LogLevel::from_kernel_digit(digit), LogLevel::from_kernel_digit(digit % 8));
        }
    }

    #[test]
    fn min_level_filtering_keeps_unknown() {
        assert!(LogLevel::Warn.passes_min(LogLevel::Warn));
        assert!(LogLevel::Error.passes_min(LogLevel::Warn));
        assert!(!LogLevel::Info.passes_min(LogLevel::Warn));
        assert!(LogLevel::Unknown.passes_min(LogLevel::Fatal));
    }

    #[test]
    fn names_round_trip_through_from_name() {
        for level in [
            LogLevel::Verbose,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
            LogLevel::Fatal,
            LogLevel::Unknown,
        ] {
            assert_eq!(LogLevel::from_name(&level.to_string()), Some(level));
        }
        assert_eq!(LogLevel::from_name("W"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_name("nope"), None);
    }

    #[test]
    fn records_serialise_with_camel_case_fields() -> crate::error::Result<()> {
        let record = LogRecord::new(LogSourceKind::Logcat, 7, "raw line");
        let json = serde_json::to_value(&record)?;
        assert_eq!(json.get("seq").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(
            json.get("source").and_then(|v| v.as_str()),
            Some("logcat"),
            "source must serialise to a lowercase tag"
        );
        assert!(json.get("uptimeSeconds").is_some());
        Ok(())
    }

    #[test]
    fn export_line_renders_placeholders_for_missing_fields() {
        let mut record = LogRecord::new(LogSourceKind::Dmesg, 1, "x");
        record.message = "hello".to_owned();
        assert_eq!(record.to_export_line(), "- ? - - -: hello");
    }
}
