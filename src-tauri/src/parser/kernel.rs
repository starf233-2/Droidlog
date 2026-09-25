//! Kernel ring-buffer parsers: `dmesg` output and `/proc/kmsg` records.
//!
//! The two formats are parsed by two **independent, non-recursive** functions.
//! Format selection happens once, in [`parse_kernel`], so a malformed line can
//! never bounce between parsers.

use super::{LogLevel, LogRecord};
use crate::source::LogSourceKind;

/// Longest subsystem name we still treat as a tag rather than message text.
const MAX_TAG_LEN: usize = 24;
/// Most spaces allowed inside a subsystem name (`usb 1-1` qualifies, prose does not).
const MAX_TAG_SPACES: usize = 1;

/// Splits an optional leading `<N>` printk level prefix.
///
/// `N` may be a bare severity (0-7) or the packed `facility * 8 + severity` form
/// that some Android kernels write; [`LogLevel::from_kernel_digit`] reduces it.
fn split_printk_prefix(line: &str) -> (Option<u8>, &str) {
    let Some(rest) = line.strip_prefix('<') else {
        return (None, line);
    };
    let Some((digits, tail)) = rest.split_once('>') else {
        return (None, line);
    };
    // Multi-digit packed levels are real (`<12>`), so any digit run is accepted.
    match digits.parse::<u8>() {
        Ok(level) if !digits.is_empty() => (Some(level), tail),
        _ => (None, line),
    }
}

/// Extracts the `[ 1234.567890]` uptime bracket, returning `(seconds, rest)`.
fn split_uptime_bracket(line: &str) -> Option<(f64, &str)> {
    let rest = line.strip_prefix('[')?;
    let (inner, tail) = rest.split_once(']')?;
    // The bracket holds a single float, but trailing spaces are common.
    let seconds = inner.trim().parse::<f64>().ok()?;
    Some((seconds, tail.trim_start()))
}

/// Splits a leading `subsystem:` prefix into a tag when it looks like one.
///
/// Kernel messages are conventionally `<subsys>: <text>`, where `<subsys>` may
/// contain a single space (`usb 1-1`). Prose with a colon in the middle of a
/// sentence is deliberately *not* treated as a tag.
fn split_subsystem_tag(message: &str) -> (Option<&str>, &str) {
    let Some((head, tail)) = message.split_once(':') else {
        return (None, message);
    };
    let candidate = head.trim();
    let spaces = candidate.chars().filter(|c| *c == ' ').count();
    let plausible = !candidate.is_empty()
        && candidate.len() <= MAX_TAG_LEN
        && spaces <= MAX_TAG_SPACES
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.' | '/'));
    if plausible {
        (Some(candidate), tail.trim_start())
    } else {
        (None, message)
    }
}

/// Builds a record from an already-split body, defaulting to the dmesg source.
fn kernel_record(
    line: &str,
    seq: u64,
    source: LogSourceKind,
    level: LogLevel,
    uptime: Option<f64>,
    body: &str,
) -> LogRecord {
    let (tag, message) = split_subsystem_tag(body);
    let mut record = LogRecord::new(source, seq, line);
    record.level = level;
    record.uptime_seconds = uptime;
    record.tag = tag.map(str::to_owned);
    record.message = message.to_owned();
    record
}

/// Parses a kernel line in either of the two text shapes it appears in.
///
/// * `[    1.234567] usb 1-1: new device` — `dmesg`, with an uptime bracket.
/// * `<12>healthd: battery l=64` — what some Android kernels emit on
///   `/proc/kmsg`: a packed printk prefix, **no** bracket and no timestamp.
///
/// The second shape used to fall through both kernel parsers and arrive as a raw
/// row, which is why every column showed `-` when capturing kmsg from
/// `/proc/kmsg` on a device that uses it.
///
/// Returns `None` when the line is neither shape.
#[must_use]
pub fn parse_dmesg(line: &str, seq: u64) -> Option<LogRecord> {
    if line.trim().is_empty() {
        return None;
    }

    let (level_digit, after_prefix) = split_printk_prefix(line);
    let trimmed = after_prefix.trim_start();

    if let Some((uptime, body)) = split_uptime_bracket(trimmed) {
        let level = level_digit.map_or(LogLevel::Info, LogLevel::from_kernel_digit);
        return Some(kernel_record(
            line,
            seq,
            LogSourceKind::Dmesg,
            level,
            Some(uptime),
            body,
        ));
    }

    // No bracket: only accept it when there *was* a printk prefix, so ordinary
    // prose is not mistaken for a kernel message.
    let level_digit = level_digit?;
    Some(kernel_record(
        line,
        seq,
        LogSourceKind::Dmesg,
        LogLevel::from_kernel_digit(level_digit),
        None,
        trimmed,
    ))
}

/// Parses the raw `/proc/kmsg` form:
///
/// ```text
/// 6,1234,567890,-;usb 1-1: new high-speed USB device
/// ```
///
/// The header is `<level>,<sequence>,<usec>,<flags>`; `usec` becomes seconds of
/// uptime. Returns `None` when the header is absent or not numeric.
#[must_use]
pub fn parse_kmsg(line: &str, seq: u64) -> Option<LogRecord> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (header, message) = trimmed.split_once(';')?;
    let fields: Vec<&str> = header.split(',').collect();
    let [level, _sequence, usec, _flags] = fields.as_slice() else {
        return None;
    };

    let level = level.trim().parse::<u8>().ok()?;
    let uptime = usec.trim().parse::<f64>().ok().map(|us| us / 1_000_000.0);

    Some(kernel_record(
        line,
        seq,
        LogSourceKind::Kmsg,
        LogLevel::from_kernel_digit(level),
        uptime,
        message.trim_start(),
    ))
}

/// Parses a kernel line for `source`, trying that source's native format first
/// and the other format second.
///
/// The resulting record always carries `source`, regardless of which grammar
/// happened to match.
#[must_use]
pub fn parse_kernel(line: &str, source: LogSourceKind, seq: u64) -> Option<LogRecord> {
    let parsed = match source {
        LogSourceKind::Kmsg => parse_kmsg(line, seq).or_else(|| parse_dmesg(line, seq)),
        _ => parse_dmesg(line, seq).or_else(|| parse_kmsg(line, seq)),
    };
    parsed.map(|mut record| {
        record.source = source;
        record
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmesg_with_printk_level_and_bracket() {
        let line = "<6>[    1.234567] usb 1-1: new high-speed USB device number 2";
        let Some(record) = parse_dmesg(line, 1) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.level, LogLevel::Info);
        assert_eq!(record.uptime_seconds, Some(1.234_567));
        assert_eq!(record.tag.as_deref(), Some("usb 1-1"));
        assert_eq!(record.message, "new high-speed USB device number 2");
        assert_eq!(record.source, LogSourceKind::Dmesg);
    }

    #[test]
    fn dmesg_error_level_maps_from_printk_digit() {
        let line = "<3>[   12.345678] binder: transaction failed";
        let Some(record) = parse_dmesg(line, 2) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.level, LogLevel::Error);
        assert_eq!(record.tag.as_deref(), Some("binder"));
        assert_eq!(record.message, "transaction failed");
    }

    #[test]
    fn dmesg_without_prefix_defaults_to_info() {
        let line = "[    0.000000] Linux version 5.10.0";
        let Some(record) = parse_dmesg(line, 3) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.level, LogLevel::Info);
        assert_eq!(record.message, "Linux version 5.10.0");
        assert_eq!(record.tag, None);
    }

    #[test]
    fn dmesg_skips_blank_and_non_bracketed_lines() {
        assert!(parse_dmesg("   ", 1).is_none());
        assert!(parse_dmesg("", 1).is_none());
        // No uptime bracket => not dmesg.
        assert!(parse_dmesg("plain text line", 1).is_none());
    }

    #[test]
    fn prose_with_a_colon_is_not_treated_as_a_tag() {
        let line = "[    1.000000] Some long sentence that is not a tag: and more";
        let Some(record) = parse_dmesg(line, 1) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.tag, None);
        assert!(record.message.starts_with("Some long sentence"));
    }

    #[test]
    fn kmsg_record_is_parsed() {
        let line = "6,1234,567890,-;usb 1-1: new device";
        let Some(record) = parse_kmsg(line, 5) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.level, LogLevel::Info);
        assert_eq!(record.uptime_seconds, Some(0.567_89));
        assert_eq!(record.source, LogSourceKind::Kmsg);
        assert_eq!(record.tag.as_deref(), Some("usb 1-1"));
        assert_eq!(record.message, "new device");
    }

    #[test]
    fn kmsg_emergency_level_maps_to_error() {
        let line = "3,100,2000000,-;Out of memory";
        let Some(record) = parse_kmsg(line, 6) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.level, LogLevel::Error);
        assert_eq!(record.uptime_seconds, Some(2.0));
    }

    #[test]
    fn kmsg_rejects_non_numeric_headers() {
        assert!(parse_kmsg("not,a,number,x;body", 1).is_none());
        assert!(parse_kmsg("1,2,3;body", 1).is_none(), "three fields");
        assert!(parse_kmsg("no separator at all", 1).is_none());
    }

    #[test]
    fn dmesg_does_not_consume_kmsg_lines() {
        // The two grammars stay independent: no cross-fallback inside them.
        assert!(parse_dmesg("7,55,1234567,-;debug only", 1).is_none());
        assert!(parse_kmsg("<4>[   10.000000] warning: something", 1).is_none());
    }

    #[test]
    fn dispatcher_tries_native_format_first() {
        let dmesg_line = "<4>[   10.000000] warning: something";
        let Some(record) = parse_kernel(dmesg_line, LogSourceKind::Dmesg, 7) else {
            unreachable!("dmesg sample must parse")
        };
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(record.uptime_seconds, Some(10.0));
        assert_eq!(record.source, LogSourceKind::Dmesg);
    }

    #[test]
    fn dispatcher_stamps_requested_source_on_fallback() {
        // A dmesg-shaped line inside a kmsg session still reports the session source.
        let dmesg_line = "<4>[   10.000000] warning: something";
        let Some(record) = parse_kernel(dmesg_line, LogSourceKind::Kmsg, 8) else {
            unreachable!("fallback must parse")
        };
        assert_eq!(record.source, LogSourceKind::Kmsg);
    }

    #[test]
    fn dispatcher_returns_none_for_garbage() {
        assert!(parse_kernel("total garbage", LogSourceKind::Dmesg, 1).is_none());
        assert!(parse_kernel("", LogSourceKind::Kmsg, 1).is_none());
    }
}
