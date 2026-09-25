//! Asynchronous device interrogation: build fingerprints, root capability and
//! package → PID / UID resolution.
//!
//! Everything that crosses into a device shell is validated first — package
//! names are interpolated into a shell string, so they are checked against a
//! strict grammar instead of being trusted.

use crate::adb::Adb;
use crate::error::{DroidLogError, Result};
use crate::executor::{root, ExecMode};

/// Properties fetched in a single round trip to build the device fingerprint.
const PROP_COMMAND: &str = "getprop ro.build.version.release; getprop ro.build.version.sdk";

/// Extra device details filled in after `adb devices -l`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProbe {
    /// `ro.build.version.release`, e.g. `14`.
    pub android_version: Option<String>,
    /// `ro.build.version.sdk`, e.g. `34`.
    pub sdk: Option<i32>,
    /// Whether `su -c id` succeeded.
    pub root_available: bool,
    /// Why root is unavailable, when it is.
    pub root_reason: Option<String>,
}

/// Validates an Android package name before it is interpolated into a shell command.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] for anything outside `[A-Za-z0-9_.]`
/// or with an empty / dot-leading segment.
pub fn validate_package(package: &str) -> Result<()> {
    if package.is_empty() {
        return Err(DroidLogError::InvalidInput("包名为空".to_owned()));
    }
    if package.starts_with('.') || package.ends_with('.') || package.contains("..") {
        return Err(DroidLogError::InvalidInput(format!(
            "包名格式无效：{package}"
        )));
    }
    let valid = package
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
    if !valid {
        return Err(DroidLogError::InvalidInput(format!(
            "包名包含非法字符：{package}"
        )));
    }
    Ok(())
}

/// Parses the two-line output of [`PROP_COMMAND`].
///
/// The output is *not* trusted: `adb shell` merges the device's stderr into
/// stdout, so a device whose shell has no `getprop` (an old or crippled ROM, or
/// a non-Android shell) answers with `/bin/bash: getprop: command not found` on
/// stdout. Taken as a version that string then rendered as the device's Android
/// version. Values are therefore checked for shape before they are believed.
#[must_use]
pub fn parse_props(stdout: &str) -> (Option<String>, Option<i32>) {
    let lines: Vec<&str> = stdout.lines().collect();
    let release = lines
        .first()
        .map(|line| line.trim())
        .filter(|line| is_plausible_version(line))
        .map(str::to_owned);
    let sdk = lines
        .get(1)
        .map(|line| line.trim())
        .and_then(|line| line.parse::<i32>().ok())
        .filter(|sdk| *sdk > 0);
    (release, sdk)
}

/// Whether `value` can be an Android release string.
///
/// Releases look like `4.4.2`, `13`, `12L`, `14QPR1`: alphanumerics and dots
/// only, no spaces, at least one digit, and short. Anything else is the shell
/// talking, not the property.
fn is_plausible_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && !value.starts_with('.')
        && value.chars().any(|c| c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// Parses `pidof` output into a de-duplicated, ascending PID list.
#[must_use]
pub fn parse_pids(stdout: &str) -> Vec<i32> {
    let mut pids: Vec<i32> = stdout
        .split_whitespace()
        .filter_map(|token| token.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Extracts `userId=` from `dumpsys package <pkg>` output.
#[must_use]
pub fn parse_uid_from_dumpsys(stdout: &str) -> Option<i32> {
    stdout.lines().find_map(|line| {
        let trimmed = line.trim();
        let value = trimmed.strip_prefix("userId=")?;
        // The line may continue with other fields, so take the first token.
        value.split_whitespace().next()?.parse::<i32>().ok()
    })
}

/// Reads the Android version, SDK level and root capability of `serial`.
///
/// The root probe never fails for "root is absent"; that is data, not an error.
///
/// # Errors
///
/// Propagates transport failures (adb missing, device gone).
pub async fn probe_device(adb: &Adb, serial: &str) -> Result<DeviceProbe> {
    let props = adb
        .target(ExecMode::Adb, Some(serial))
        .run(PROP_COMMAND)
        .await;
    let (android_version, sdk) = match props {
        Ok(output) => parse_props(&output.stdout),
        // An offline device still deserves a row in the list, so fingerprint
        // failures degrade to "unknown" rather than bubbling up.
        Err(_) => (None, None),
    };

    let probe = root::probe(adb.program(), Some(serial)).await?;

    Ok(DeviceProbe {
        android_version,
        sdk,
        root_available: probe.available,
        root_reason: probe.reason,
    })
}

/// Resolves every PID belonging to `package`.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] for a malformed package name, and
/// propagates transport failures.
pub async fn resolve_pids(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    package: &str,
) -> Result<Vec<i32>> {
    validate_package(package)?;

    let primary = format!("pidof {package}");
    let output = adb
        .target(mode, Some(serial))
        .run_tolerant(&primary)
        .await?;
    let pids = parse_pids(&output.stdout);
    if !pids.is_empty() {
        return Ok(pids);
    }

    // `pidof` is absent from some older toolboxes; `pgrep -f` is the fallback.
    let fallback = format!("pgrep -f {package}");
    let output = adb
        .target(mode, Some(serial))
        .run_tolerant(&fallback)
        .await?;
    Ok(parse_pids(&output.stdout))
}

/// Resolves the main PID of `package` (lowest PID, i.e. the app's root process).
///
/// # Errors
///
/// Propagates the same failures as [`resolve_pids`].
pub async fn resolve_pid(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    package: &str,
) -> Result<Option<i32>> {
    Ok(resolve_pids(adb, mode, serial, package)
        .await?
        .into_iter()
        .next())
}

/// Resolves the Linux UID that owns `package`.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] for a malformed package name, and
/// propagates transport failures.
pub async fn resolve_uid(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    package: &str,
) -> Result<Option<i32>> {
    validate_package(package)?;
    let command = format!("dumpsys package {package}");
    let output = adb
        .target(mode, Some(serial))
        .run_tolerant(&command)
        .await?;
    Ok(parse_uid_from_dumpsys(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_parse_version_and_sdk() {
        let (version, sdk) = parse_props("14\n34\n");
        assert_eq!(version.as_deref(), Some("14"));
        assert_eq!(sdk, Some(34));
    }

    #[test]
    fn props_tolerate_missing_second_line() {
        let (version, sdk) = parse_props("14\n");
        assert_eq!(version.as_deref(), Some("14"));
        assert_eq!(sdk, None);
    }

    #[test]
    fn props_treat_blank_release_as_unknown() {
        let (version, sdk) = parse_props("\n34\n");
        assert_eq!(version, None);
        assert_eq!(sdk, Some(34));
    }

    #[test]
    fn props_reject_shell_errors_masquerading_as_a_version() {
        // Measured on a Nexus 4 whose `adb shell` runs bash without getprop in
        // PATH: the error arrives on stdout and used to be shown as the version.
        let (version, sdk) = parse_props("/bin/bash:行1: getprop：未找到命令\n/bin/bash:行1: getprop：未找到命令\n");
        assert_eq!(version, None);
        assert_eq!(sdk, None);
        let (version, _) = parse_props("getprop: not found\n");
        assert_eq!(version, None);
    }

    #[test]
    fn props_accept_every_real_release_shape() {
        for release in ["4.4.2", "13", "12L", "14QPR1", "9"] {
            let (version, _) = parse_props(&format!("{release}\n33\n"));
            assert_eq!(version.as_deref(), Some(release), "{release}");
        }
        // Not plausible: empty, too long, no digits, leading dot.
        for bad in ["", ".", "..............x", "not-a-version", ".13"] {
            let (version, _) = parse_props(&format!("{bad}\n"));
            assert_eq!(version, None, "{bad}");
        }
    }

    #[test]
    fn props_reject_a_nonsense_sdk() {
        let (_, sdk) = parse_props("13\n-1\n");
        assert_eq!(sdk, None);
        let (_, sdk) = parse_props("13\nabc\n");
        assert_eq!(sdk, None);
    }

    #[test]
    fn pids_are_sorted_and_deduplicated() {
        assert_eq!(parse_pids("4321 1234 4321\n"), vec![1234, 4321]);
        assert!(parse_pids("no pids here").is_empty());
        assert!(parse_pids("0 -5").is_empty());
    }

    #[test]
    fn uid_is_read_from_dumpsys() {
        let dump =
            "Packages:\n  Package [com.example.app]:\n    userId=10123\n    pkg=Package{abc}\n";
        assert_eq!(parse_uid_from_dumpsys(dump), Some(10123));
    }

    #[test]
    fn uid_missing_returns_none() {
        assert_eq!(parse_uid_from_dumpsys("Packages:\n"), None);
    }

    #[test]
    fn package_validation_accepts_real_names() {
        for name in ["com.example.app", "com.example_app", "a", "com.a1.b2"] {
            assert!(validate_package(name).is_ok(), "rejected {name}");
        }
    }

    #[test]
    fn package_validation_rejects_injection_attempts() {
        for name in [
            "",
            "com.example; rm -rf /",
            "com.example`id`",
            "com.example$(id)",
            ".leading",
            "trailing.",
            "double..dot",
            "com example",
        ] {
            assert!(validate_package(name).is_err(), "accepted {name}");
        }
    }

    #[test]
    fn validation_errors_are_invalid_input() {
        assert_eq!(
            validate_package("bad name").map_err(|e| e.kind()),
            Err("invalidInput")
        );
    }
}
