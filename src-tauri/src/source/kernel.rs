//! Kernel ring-buffer collectors: `dmesg -w` and `cat /proc/kmsg`.
//!
//! Both read the same underlying buffer in different grammars, so they share
//! [`crate::parser::parse_kernel`] and differ only in the command they run and
//! the parser their spec advertises.

use super::{
    effective_command, spec, LogSource, LogSourceKind, LogSourceSpec, SourceOptions,
};
use crate::parser::{parse_kernel, LogRecord};

/// Streams `dmesg -w` (printk text form).
#[derive(Debug, Clone, Copy, Default)]
pub struct DmesgSource;

impl DmesgSource {
    /// Creates the collector.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl LogSource for DmesgSource {
    fn kind(&self) -> LogSourceKind {
        LogSourceKind::Dmesg
    }

    fn spec(&self) -> &'static LogSourceSpec {
        spec(LogSourceKind::Dmesg)
    }

    fn command(&self, options: &SourceOptions) -> String {
        effective_command(options, DMESG_COMMAND)
    }

    fn parse_line(&self, line: &str, seq: u64) -> LogRecord {
        parse_kernel(line, LogSourceKind::Dmesg, seq)
            .unwrap_or_else(|| LogRecord::raw_line(self.kind(), seq, line))
    }
}

/// Streams `/proc/kmsg` (raw `level,seq,usec,flags;` records).
#[derive(Debug, Clone, Copy, Default)]
pub struct KmsgSource;

impl KmsgSource {
    /// Creates the collector.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl LogSource for KmsgSource {
    fn kind(&self) -> LogSourceKind {
        LogSourceKind::Kmsg
    }

    fn spec(&self) -> &'static LogSourceSpec {
        spec(LogSourceKind::Kmsg)
    }

    fn command(&self, options: &SourceOptions) -> String {
        effective_command(options, KMSG_COMMAND)
    }

    fn parse_line(&self, line: &str, seq: u64) -> LogRecord {
        parse_kernel(line, LogSourceKind::Kmsg, seq)
            .unwrap_or_else(|| LogRecord::raw_line(self.kind(), seq, line))
    }
}

/// `dmesg -w` follows the buffer; `-w` is supported by toybox and util-linux.
pub const DMESG_COMMAND: &str = "dmesg -w";

/// Reads the kernel ring buffer in its **structured** form.
///
/// `/dev/kmsg` rather than `/proc/kmsg`, deliberately:
///
/// * it yields `level,seq,usec,flags;message`, so the kernel timestamp and the
///   severity come through and the parser has real fields to fill;
/// * it is **non-consuming** — every reader keeps its own position — whereas
///   `/proc/kmsg` hands each message to exactly one reader, so opening it steals
///   messages from anything else watching (logd, for instance);
/// * several Android kernels write a bare `<N>text` form on `/proc/kmsg` with no
///   timestamp and no structure at all, which is what made every kmsg column show
///   `-`. The parser still accepts that shape as a fallback for custom commands.
///
/// Root-only, like every other way of reading the kernel buffer.
pub const KMSG_COMMAND: &str = "cat /dev/kmsg";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::LogLevel;

    #[test]
    fn dmesg_source_metadata() {
        let source = DmesgSource::new();
        assert_eq!(source.kind(), LogSourceKind::Dmesg);
        assert!(source.spec().requires_root);
        assert_eq!(source.command(&SourceOptions::unrestricted()), "dmesg -w");
    }

    #[test]
    fn kmsg_source_metadata() {
        let source = KmsgSource::new();
        assert_eq!(source.kind(), LogSourceKind::Kmsg);
        assert!(source.spec().requires_root);
        assert_eq!(
            source.command(&SourceOptions::unrestricted()),
            "cat /dev/kmsg"
        );
    }

    #[test]
    fn dmesg_lines_map_kernel_levels_to_log_levels() {
        let source = DmesgSource::new();
        let error = source.parse_line("<3>[   12.345678] binder: failed", 1);
        assert_eq!(error.level, LogLevel::Error);
        assert_eq!(error.uptime_seconds, Some(12.345_678));
        assert_eq!(error.source, LogSourceKind::Dmesg);
        assert!(error.parsed);

        let warn = source.parse_line("<4>[   10.000000] warning: slow", 2);
        assert_eq!(warn.level, LogLevel::Warn);

        let info = source.parse_line("[    0.000000] Linux version 5.10.0", 3);
        assert_eq!(info.level, LogLevel::Info);
    }

    #[test]
    fn kmsg_records_are_parsed_with_kernel_timestamps() {
        let source = KmsgSource::new();
        let record = source.parse_line("6,1234,567890,-;usb 1-1: new device", 1);
        assert_eq!(record.source, LogSourceKind::Kmsg);
        assert_eq!(record.level, LogLevel::Info);
        assert_eq!(record.uptime_seconds, Some(0.567_89));
        assert!(record.parsed);
    }

    #[test]
    fn kernel_sources_keep_unparseable_lines() {
        // Plain prose with neither a bracket nor a printk prefix cannot be a
        // kernel record, so it stays raw rather than being guessed at.
        for (source, line) in [
            (&DmesgSource::new() as &dyn LogSource, "no bracket here at all"),
            (&KmsgSource::new() as &dyn LogSource, "garbage,not,a,record"),
        ] {
            let record = source.parse_line(line, 5);
            assert!(!record.parsed, "expected raw for {line:?}");
            assert_eq!(record.raw, line);
            assert_eq!(record.message, line);
        }
    }

    #[test]
    fn proc_kmsg_plain_form_is_parsed_not_dropped() {
        // The exact shape this device's /proc/kmsg emits. It has a packed level
        // (`<12>` = facility 1 + severity 4) and no timestamp, and used to arrive
        // as a raw row with every column empty.
        let record = KmsgSource::new().parse_line("<12>healthd: battery l=64 v=4039", 1);
        assert!(record.parsed, "the plain <N>text form must be parsed");
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(record.tag.as_deref(), Some("healthd"));
        assert_eq!(record.message, "battery l=64 v=4039");
        assert_eq!(record.uptime_seconds, None, "this form carries no timestamp");
    }

    #[test]
    fn dev_kmsg_structured_form_is_parsed() {
        // And the shape /dev/kmsg emits, which is the source's default command.
        let record = KmsgSource::new()
            .parse_line("7,2402,7977843,-;[Awinic]aw_dev_i2s_enable: enter, i2s_enable: 0", 1);
        assert!(record.parsed);
        assert_eq!(record.level, LogLevel::Debug);
        assert_eq!(record.uptime_seconds, Some(7.977_843));
        assert!(record.message.contains("aw_dev_i2s_enable"));
    }

    #[test]
    fn blank_lines_are_kept_as_raw_too() {
        // Blank lines are cheap and keeping them preserves the stream's shape.
        let record = DmesgSource::new().parse_line("   ", 1);
        assert!(!record.parsed);
    }

    #[test]
    fn custom_commands_override_kernel_defaults() {
        let options = SourceOptions {
            custom_command: Some("dmesg -w -l err".to_owned()),
            ..SourceOptions::default()
        };
        assert_eq!(DmesgSource::new().command(&options), "dmesg -w -l err");
        assert_eq!(KmsgSource::new().command(&options), "dmesg -w -l err");
    }

    #[test]
    fn kernel_sources_do_not_use_pid_or_tag_options() {
        // Those knobs are logcat-only; they must not leak into kernel commands.
        let options = SourceOptions {
            pids: vec![1234],
            tags: vec!["ActivityManager".to_owned()],
            ..SourceOptions::default()
        };
        assert_eq!(DmesgSource::new().command(&options), "dmesg -w");
        assert_eq!(KmsgSource::new().command(&options), "cat /dev/kmsg");
    }
}
