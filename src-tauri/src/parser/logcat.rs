//! logcat line parsers (`threadtime` first, `brief` as fallback).

use super::{LogLevel, LogRecord};
use crate::source::LogSourceKind;

/// Characters logcat uses for severity.
const LEVEL_CHARS: &str = "VDIWEFSA";

/// Extracts a single character from `token`, or `None` when it is not exactly one char.
fn single_char(token: &str) -> Option<char> {
    let mut chars = token.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(first)
}

/// True for `MM-DD` and `YYYY-MM-DD`.
fn is_date_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    if !(bytes.len() == 5 || bytes.len() == 10) {
        return false;
    }
    token
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-')
}

/// True for `HH:MM:SS.mmm` and `HH:MM:SS`.
fn is_time_token(token: &str) -> bool {
    let mut parts = token.split(':');
    let (Some(h), Some(m), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Ok(hours) = h.parse::<u32>() else {
        return false;
    };
    let Ok(minutes) = m.parse::<u32>() else {
        return false;
    };
    let seconds = s.split('.').next().unwrap_or_default();
    let Ok(seconds) = seconds.parse::<u32>() else {
        return false;
    };
    hours < 24 && minutes < 60 && seconds < 61
}

/// Removes exactly one leading space, preserving any further indentation.
fn strip_one_space(text: &str) -> &str {
    text.strip_prefix(' ').unwrap_or(text)
}

/// Decodes the uid field that `logcat -v uid` (or `-v threadtime,uid`) emits.
///
/// The field is numeric for app processes but a **name** for system ones
/// (`root`, `system`, `shell`, `radio`), and `u<user>_a<app>` on secondary
/// users. All three forms are decoded, so uid-based filtering works for every
/// process rather than only the numeric ones.
#[must_use]
pub fn parse_uid_token(token: &str) -> Option<i32> {
    let bare = token.strip_prefix("uid=").unwrap_or(token).trim();
    if bare.is_empty() {
        return None;
    }
    if let Ok(numeric) = bare.parse::<i32>() {
        return Some(numeric);
    }

    match bare {
        "root" => Some(0),
        "system" => Some(1000),
        "radio" => Some(1001),
        "shell" => Some(2000),
        _ => {
            // `u<user>_a<app>` follows the Android multi-user UID layout.
            let rest = bare.strip_prefix('u')?;
            let (user, app) = rest.split_once("_a")?;
            let user = user.parse::<i32>().ok()?;
            let app = app.parse::<i32>().ok()?;
            Some(user.saturating_mul(100_000).saturating_add(10_000 + app))
        }
    }
}

/// Splits `MM-DD HH:MM:SS.mmm <rest>` (or the year-prefixed variant).
fn split_date_time(line: &str) -> Option<(&str, &str, &str)> {
    let date_end = line.find(' ')?;
    let date = line.get(..date_end)?;
    if !is_date_token(date) {
        return None;
    }

    let after_date = line.get(date_end + 1..)?.trim_start();
    let time_end = after_date.find(' ')?;
    let time = after_date.get(..time_end)?;
    if !is_time_token(time) {
        return None;
    }

    let rest = after_date.get(time_end + 1..)?;
    Some((date, time, rest))
}

/// Parses the default `logcat -v threadtime` line:
///
/// ```text
/// 09-01 12:34:56.789  1234  5678 I ActivityManager: Start proc
/// ```
///
/// A five-token metadata block (as produced by `-v uid`) is also accepted, with
/// the leading token read as the uid — either `10123` or `uid=10123`.
#[must_use]
pub fn parse_logcat_threadtime(line: &str, seq: u64) -> Option<LogRecord> {
    let (date, time, rest) = split_date_time(line)?;

    // Everything before the first ':' is metadata; the message may contain ':'.
    let (meta, message) = rest.split_once(':')?;
    let tokens: Vec<&str> = meta.split_whitespace().collect();

    let (uid_token, pid_token, tid_token, level_token, tag) = match tokens.as_slice() {
        [pid, tid, level, tag] => (None, *pid, *tid, *level, *tag),
        [uid, pid, tid, level, tag] => (Some(*uid), *pid, *tid, *level, *tag),
        _ => return None,
    };

    let level_char = single_char(level_token)?;
    if !LEVEL_CHARS.contains(level_char) {
        return None;
    }

    let pid = pid_token.parse::<i32>().ok()?;
    let tid = tid_token.parse::<i32>().ok()?;

    let mut record = LogRecord::new(LogSourceKind::Logcat, seq, line);
    record.level = LogLevel::from_logcat_char(level_char);
    record.timestamp = Some(format!("{date} {time}"));
    record.pid = Some(pid);
    record.tid = Some(tid);
    record.uid = uid_token.and_then(parse_uid_token);
    record.tag = Some(tag.to_owned());
    record.message = strip_one_space(message).to_owned();
    Some(record)
}

/// Parses the legacy `logcat -v brief` line:
///
/// ```text
/// I/ActivityManager( 1234): Start proc
/// ```
///
/// Newer builds omit the PID, so `I/ActivityManager: message` is accepted too.
#[must_use]
pub fn parse_logcat_brief(line: &str, seq: u64) -> Option<LogRecord> {
    let (meta, message) = line.split_once(':')?;
    let (level_token, remainder) = meta.split_once('/')?;
    let level_char = single_char(level_token)?;
    if !LEVEL_CHARS.contains(level_char) {
        return None;
    }

    let (tag, pid) = match remainder.split_once('(') {
        Some((tag, tail)) => (
            tag,
            tail.strip_suffix(')')
                .and_then(|pid| pid.trim().parse::<i32>().ok()),
        ),
        None => (remainder, None),
    };

    if tag.trim().is_empty() {
        return None;
    }

    let mut record = LogRecord::new(LogSourceKind::Logcat, seq, line);
    record.level = LogLevel::from_logcat_char(level_char);
    record.pid = pid;
    record.tag = Some(tag.trim().to_owned());
    record.message = strip_one_space(message).to_owned();
    Some(record)
}

/// Tries `threadtime`, then `brief`.
///
/// Returns `None` for banner and separator lines such as
/// `--------- beginning of main`.
#[must_use]
pub fn parse_logcat(line: &str, seq: u64) -> Option<LogRecord> {
    parse_logcat_threadtime(line, seq).or_else(|| parse_logcat_brief(line, seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREADTIME: &str = "09-01 12:34:56.789  1234  5678 I ActivityManager: Start proc 4321:com.example/u0a123";

    #[test]
    fn threadtime_line_is_fully_decoded() {
        let record = parse_logcat_threadtime(THREADTIME, 1);
        let Some(record) = record else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.level, LogLevel::Info);
        assert_eq!(record.timestamp.as_deref(), Some("09-01 12:34:56.789"));
        assert_eq!(record.pid, Some(1234));
        assert_eq!(record.tid, Some(5678));
        assert_eq!(record.tag.as_deref(), Some("ActivityManager"));
        assert_eq!(
            record.message,
            "Start proc 4321:com.example/u0a123",
            "colons inside the message must be preserved"
        );
        assert_eq!(record.raw, THREADTIME);
    }

    #[test]
    fn year_prefixed_timestamp_is_accepted() {
        let line = "2024-09-01 12:34:56.789  1234  5678 E AndroidRuntime: FATAL";
        let Some(record) = parse_logcat_threadtime(line, 2) else {
            unreachable!("year-prefixed sample must parse")
        };
        assert_eq!(record.timestamp.as_deref(), Some("2024-09-01 12:34:56.789"));
        assert_eq!(record.level, LogLevel::Error);
    }

    #[test]
    fn uid_variant_is_read() {
        let line = "09-01 12:34:56.789  10123  1234  5678 I Tag: hello";
        let Some(record) = parse_logcat_threadtime(line, 3) else {
            unreachable!("uid sample must parse")
        };
        assert_eq!(record.uid, Some(10123));
        assert_eq!(record.pid, Some(1234));
        assert_eq!(record.tag.as_deref(), Some("Tag"));
    }

    #[test]
    fn uid_name_forms_are_decoded() {
        // `logcat -v threadtime,uid` prints names for system processes.
        for (token, expected) in [
            ("root", 0),
            ("system", 1000),
            ("radio", 1001),
            ("shell", 2000),
            ("u0_a143", 10_143),
            ("u10_a25", 1_010_025),
            ("uid=10143", 10_143),
        ] {
            assert_eq!(parse_uid_token(token), Some(expected), "token {token}");
        }
        assert_eq!(parse_uid_token("nonsense"), None);
        assert_eq!(parse_uid_token(""), None);
    }

    #[test]
    fn threadtime_uid_format_is_parsed_end_to_end() {
        // Verbatim from `logcat -v threadtime,uid` on Android 16.
        let line = "09-25 01:46:48.101 10270  3798 11699 D JavaheapMonitor: Java heap used=3M";
        let Some(record) = parse_logcat_threadtime(line, 1) else {
            unreachable!("threadtime,uid sample must parse")
        };
        assert_eq!(record.uid, Some(10270));
        assert_eq!(record.pid, Some(3798));
        assert_eq!(record.tid, Some(11699));
        assert_eq!(record.level, LogLevel::Debug);
        assert_eq!(record.tag.as_deref(), Some("JavaheapMonitor"));
        assert_eq!(record.message, "Java heap used=3M");
    }

    #[test]
    fn threadtime_uid_format_with_a_name_still_parses() {
        let line = "09-25 01:46:48.240  root  1749  1749 I adbd    : in ShellService";
        let Some(record) = parse_logcat_threadtime(line, 1) else {
            unreachable!("named-uid sample must parse")
        };
        assert_eq!(record.uid, Some(0));
        assert_eq!(record.pid, Some(1749));
        assert_eq!(record.tag.as_deref(), Some("adbd"));
    }

    #[test]
    fn uid_prefix_form_is_read() {
        let line = "09-01 12:34:56.789  uid=10123  1234  5678 I Tag: hello";
        let Some(record) = parse_logcat_threadtime(line, 4) else {
            unreachable!("uid= sample must parse")
        };
        assert_eq!(record.uid, Some(10123));
    }

    #[test]
    fn separator_lines_are_skipped() {
        assert!(parse_logcat("--------- beginning of main", 1).is_none());
        assert!(parse_logcat("--------- beginning of system", 2).is_none());
        assert!(parse_logcat("", 3).is_none());
    }

    #[test]
    fn malformed_threadtime_is_rejected() {
        // Non-numeric pid/tid must not be silently accepted.
        assert!(parse_logcat_threadtime("09-01 12:34:56.789  xx  yy I Tag: msg", 1).is_none());
        // Impossible clock values are rejected.
        assert!(parse_logcat_threadtime("09-01 99:34:56.789  1  2 I Tag: msg", 1).is_none());
    }

    #[test]
    fn brief_line_is_parsed() {
        let line = "W/ActivityManager( 1234): Slow operation";
        let Some(record) = parse_logcat_brief(line, 1) else {
            unreachable!("brief sample must parse")
        };
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(record.pid, Some(1234));
        assert_eq!(record.tag.as_deref(), Some("ActivityManager"));
        assert_eq!(record.message, "Slow operation");
        assert_eq!(record.timestamp, None);
    }

    #[test]
    fn brief_without_pid_is_parsed() {
        let Some(record) = parse_logcat_brief("D/Tag: no pid here", 1) else {
            unreachable!("pid-less brief sample must parse")
        };
        assert_eq!(record.pid, None);
        assert_eq!(record.tag.as_deref(), Some("Tag"));
    }

    #[test]
    fn dispatch_prefers_threadtime() {
        let Some(record) = parse_logcat(THREADTIME, 9) else {
            unreachable!("dispatch must parse the threadtime sample")
        };
        assert_eq!(record.timestamp.as_deref(), Some("09-01 12:34:56.789"));
    }

    #[test]
    fn dispatch_falls_back_to_brief() {
        let Some(record) = parse_logcat("E/Tag( 7): boom", 1) else {
            unreachable!("dispatch must fall back to brief")
        };
        assert_eq!(record.level, LogLevel::Error);
    }

    #[test]
    fn message_keeps_internal_indentation() {
        let line = "09-01 12:34:56.789  1  2 I Tag:   indented";
        let Some(record) = parse_logcat_threadtime(line, 1) else {
            unreachable!("sample must parse")
        };
        assert_eq!(record.message, "  indented");
    }
}
