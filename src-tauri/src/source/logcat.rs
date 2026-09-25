//! The `logcat` collector.

use super::shell;
use super::{
    effective_command, spec, LogSource, LogSourceKind, LogSourceSpec, SourceOptions,
};
use crate::parser::{parse_logcat, LogRecord};

/// Streams `logcat -v threadtime` from the device.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogcatSource;

impl LogcatSource {
    /// Creates the collector.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl LogSource for LogcatSource {
    fn kind(&self) -> LogSourceKind {
        LogSourceKind::Logcat
    }

    fn spec(&self) -> &'static LogSourceSpec {
        spec(LogSourceKind::Logcat)
    }

    fn command(&self, options: &SourceOptions) -> String {
        // A custom command replaces the built-in invocation entirely: the user
        // asked for exactly this string to run on the device.
        effective_command(options, &build_command(options))
    }

    fn parse_line(&self, line: &str, seq: u64) -> LogRecord {
        parse_logcat(line, seq).unwrap_or_else(|| LogRecord::raw_line(self.kind(), seq, line))
    }
}

/// Builds the built-in `logcat` invocation for `options`.
///
/// Shape: `logcat -v threadtime [-b main,system,crash] [--uid=N] [--pid=N ...] [-s tag ...]`.
///
/// `threadtime` is pinned because it is the only stable, fully populated text
/// format; the UI never has to cope with a second layout.
///
/// Device-side narrowing is a bandwidth optimisation, never the source of truth:
/// the backend filter remains authoritative (see [`crate::filter`]), because a
/// pid pushed here goes stale the moment the app restarts.
///
/// `--uid` is emitted in preference to `--pid` when both are known — the caller
/// only sets `uid` after probing that this device's logcat accepts it.
#[must_use]
pub fn build_command(options: &SourceOptions) -> String {
    let mut parts: Vec<String> = vec![
        "logcat".to_owned(),
        "-v".to_owned(),
        "threadtime".to_owned(),
    ];

    parts.extend(options.buffer_args());

    if let Some(uid) = options.uid {
        if uid >= 0 {
            parts.push(format!("--uid={uid}"));
        }
    }

    for pid in &options.pids {
        if *pid > 0 {
            parts.push(format!("--pid={pid}"));
        }
    }

    // `-s` switches logcat into "only these tags" mode, so it must not be
    // emitted unless at least one usable tag survives.
    let tags: Vec<String> = options
        .tags
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .map(shell::quote)
        .collect();

    if !tags.is_empty() {
        parts.push("-s".to_owned());
        parts.extend(tags);
    }

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::LogcatBuffer;

    #[test]
    fn default_command_pins_threadtime() {
        let command = build_command(&SourceOptions::unrestricted());
        assert_eq!(command, "logcat -v threadtime -b main,system,crash");
    }

    #[test]
    fn pids_are_pushed_down() {
        let options = SourceOptions {
            pids: vec![1234, 5678],
            ..SourceOptions::default()
        };
        assert_eq!(
            build_command(&options),
            "logcat -v threadtime -b main,system,crash --pid=1234 --pid=5678"
        );
    }

    #[test]
    fn non_positive_pids_are_ignored() {
        let options = SourceOptions {
            pids: vec![0, -1],
            ..SourceOptions::default()
        };
        assert!(!build_command(&options).contains("--pid"));
    }

    #[test]
    fn uid_is_pushed_down_and_preferred_over_pid() {
        let options = SourceOptions {
            uid: Some(10123),
            pids: vec![5063],
            ..SourceOptions::default()
        };
        assert_eq!(
            build_command(&options),
            "logcat -v threadtime -b main,system,crash --uid=10123 --pid=5063"
        );
    }

    #[test]
    fn negative_uid_is_ignored() {
        let options = SourceOptions {
            uid: Some(-1),
            ..SourceOptions::default()
        };
        assert!(!build_command(&options).contains("--uid"));
    }

    #[test]
    fn tags_switch_to_silent_mode_and_are_quoted() {
        let options = SourceOptions {
            tags: vec!["ActivityManager".to_owned(), "My Tag".to_owned()],
            ..SourceOptions::default()
        };
        assert_eq!(
            build_command(&options),
            "logcat -v threadtime -b main,system,crash -s ActivityManager 'My Tag'"
        );
    }

    #[test]
    fn blank_tags_do_not_emit_silent_mode() {
        let options = SourceOptions {
            tags: vec!["   ".to_owned(), String::new()],
            ..SourceOptions::default()
        };
        assert!(!build_command(&options).contains(" -s "));
    }

    #[test]
    fn buffers_can_be_narrowed() {
        let options = SourceOptions {
            buffers: vec![LogcatBuffer::Radio],
            ..SourceOptions::default()
        };
        assert!(build_command(&options).contains("-b radio"));
    }

    #[test]
    fn injection_through_tags_is_neutralised() {
        let options = SourceOptions {
            tags: vec!["x; rm -rf /".to_owned()],
            ..SourceOptions::default()
        };
        let command = build_command(&options);
        assert!(
            command.contains("'x; rm -rf /'"),
            "tag must be quoted, got: {command}"
        );
    }

    #[test]
    fn a_custom_command_replaces_the_built_in_one() {
        let source = LogcatSource::new();
        let options = SourceOptions {
            custom_command: Some("logcat -v threadtime -s MyTag".to_owned()),
            // Ignored on purpose: the custom command wins outright.
            pids: vec![42],
            ..SourceOptions::default()
        };
        assert_eq!(source.command(&options), "logcat -v threadtime -s MyTag");
    }

    #[test]
    fn a_blank_custom_command_falls_back_to_the_built_in_one() {
        let source = LogcatSource::new();
        let options = SourceOptions {
            custom_command: Some("   ".to_owned()),
            ..SourceOptions::default()
        };
        assert_eq!(
            source.command(&options),
            "logcat -v threadtime -b main,system,crash"
        );
    }

    #[test]
    fn threadtime_lines_parse() {
        let source = LogcatSource::new();
        let record = source.parse_line("09-01 12:34:56.789  1  2 I Tag: hi", 1);
        assert_eq!(record.level, crate::parser::LogLevel::Info);
        assert!(record.parsed);
    }

    #[test]
    fn unparseable_lines_are_kept_as_raw_records() {
        // The regression this guards: banners and vendor formats used to be
        // dropped entirely.
        let source = LogcatSource::new();
        for line in [
            "--------- beginning of main",
            "totally unstructured vendor output",
            "",
        ] {
            let record = source.parse_line(line, 7);
            assert_eq!(record.seq, 7);
            assert!(!record.parsed, "expected a raw record for {line:?}");
            assert_eq!(record.raw, line);
            assert_eq!(record.message, line, "the text must survive for display");
            assert_eq!(record.level, crate::parser::LogLevel::Unknown);
        }
    }

    #[test]
    fn effective_command_prefers_the_override() {
        let with = SourceOptions {
            custom_command: Some("dmesg -w".to_owned()),
            ..SourceOptions::default()
        };
        assert_eq!(effective_command(&with, "fallback"), "dmesg -w");
        assert_eq!(
            effective_command(&SourceOptions::default(), "fallback"),
            "fallback"
        );
    }
}
