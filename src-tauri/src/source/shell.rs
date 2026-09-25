//! POSIX shell quoting for device-side command strings.
//!
//! Source commands are executed as `adb shell "<command>"`: adb hands the string
//! to the device's `sh`, which re-parses it. User-supplied values (logcat tags,
//! for instance) are therefore interpolated through [`quote`] rather than
//! concatenated raw.

/// Characters that are safe to leave unquoted in a shell word.
const BARE_SAFE: &[char] = &[
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L',
    'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4',
    '5', '6', '7', '8', '9', '_', '-', '.', '=', ':', '/', '@', '+', ',',
];

/// True when `arg` needs no quoting.
#[must_use]
pub fn is_bare_safe(arg: &str) -> bool {
    !arg.is_empty() && arg.chars().all(|c| BARE_SAFE.contains(&c))
}

/// Quotes `arg` for `sh`, leaving already-safe words untouched.
///
/// Embedded single quotes are escaped with the standard `'\''` sequence.
#[must_use]
pub fn quote(arg: &str) -> String {
    if is_bare_safe(arg) {
        return arg.to_owned();
    }

    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Quotes every argument and joins them with spaces.
#[must_use]
pub fn join(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_words_are_left_alone() {
        assert!(is_bare_safe("logcat"));
        assert!(is_bare_safe("--pid=1234"));
        assert!(is_bare_safe("main,system,crash"));
        assert_eq!(quote("logcat -v threadtime"), "'logcat -v threadtime'");
        assert_eq!(quote("ActivityManager"), "ActivityManager");
    }

    #[test]
    fn spaces_and_metacharacters_are_quoted() {
        assert_eq!(quote("My Tag"), "'My Tag'");
        assert_eq!(quote("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(quote("$(id)"), "'$(id)'");
        assert_eq!(quote("`id`"), "'`id`'");
        assert_eq!(quote("a|b"), "'a|b'");
        assert_eq!(quote("a&b"), "'a&b'");
    }

    #[test]
    fn embedded_single_quotes_are_escaped() {
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn empty_string_is_quoted() {
        assert!(!is_bare_safe(""));
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn join_quotes_selectively() {
        let args = vec!["logcat".to_owned(), "-s".to_owned(), "My Tag".to_owned()];
        assert_eq!(join(&args), "logcat -s 'My Tag'");
    }
}
