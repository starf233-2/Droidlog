//! Root (`su -c`) transport details and capability probing.

use super::{run_once, CommandPlan, ExecMode, ExecOutput};
use crate::error::Result;

/// Commands used to interrogate `su` availability on the device.
///
/// `id -u` first, then `id`: the numeric answer is the verdict (a number is never
/// localised), while the full `id` line is what the UI shows. A device with a
/// non-standard shell prints an error for the first and a usable line for the
/// second, and both are parsed.
const ID_COMMAND: &str = "id -u; id";

/// Builds the adb-side argument list for `remote_command` under `mode`.
///
/// * [`ExecMode::Adb`] → `shell <remote_command>`
/// * [`ExecMode::Root`] → `shell su -c <remote_command>`
///
/// adb performs device-shell escaping on each argument, so the command is passed
/// as one argument rather than pre-quoted into a single string.
#[must_use]
pub fn shell_args(mode: ExecMode, remote_command: &str) -> Vec<String> {
    match mode {
        ExecMode::Adb => vec!["shell".to_owned(), remote_command.to_owned()],
        ExecMode::Root => vec![
            "shell".to_owned(),
            "su".to_owned(),
            "-c".to_owned(),
            remote_command.to_owned(),
        ],
    }
}

/// Outcome of a root capability probe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootProbe {
    /// True when `su -c id` reported uid 0.
    pub available: bool,
    /// Raw `id` output when available, for the UI to display.
    pub identity: Option<String>,
    /// Why root is unavailable (no `su`, denied prompt, timeout, ...).
    pub reason: Option<String>,
}

impl RootProbe {
    /// A negative probe carrying a reason.
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            identity: None,
            reason: Some(reason.into()),
        }
    }
}

/// Returns true when the probe output shows the superuser account.
///
/// Two shapes are accepted, and neither depends on the device's language:
///
/// * `id -u` prints the bare uid — `0`;
/// * `id` prints the uid as the **first** field, which is `uid=0(root)` in
///   English and `用户id=0(root)` on a Chinese device. Only the leading field is
///   trusted: `组=0(root)` (the groups list) also ends in `=0(root)` and says
///   nothing about the effective uid, so a scan of the whole line would call a
///   privileged-but-not-root shell "root".
#[must_use]
pub fn is_root_identity(stdout: &str) -> bool {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .any(|line| {
            if line == "0" {
                return true;
            }
            // An `id` line: the first field carries `=<uid>(<name>)`.
            let Some(first) = line.split_whitespace().next() else {
                return false;
            };
            let looks_like_id = line.contains('=') || line.contains('：');
            looks_like_id
                && (first.ends_with("=0(root)") || first.ends_with("=0"))
        })
}

/// Probes whether `su -c` works on `serial`.
///
/// Never fails for "root is absent" — that is reported as
/// [`RootProbe::available`]` == false`. Only transport-level failures (adb
/// missing) come back as `Err`.
///
/// # Errors
///
/// Returns an error only when the probe command could not be executed at all.
pub async fn probe(adb_program: &str, serial: Option<&str>) -> Result<RootProbe> {
    let mut args: Vec<String> = Vec::new();
    if let Some(serial) = serial {
        args.push("-s".to_owned());
        args.push(serial.to_owned());
    }
    args.extend([
        "shell".to_owned(),
        "su".to_owned(),
        "-c".to_owned(),
        ID_COMMAND.to_owned(),
    ]);

    let plan = CommandPlan {
        program: adb_program.to_owned(),
        args,
    };

    let output: ExecOutput = run_once(&plan).await?;
    Ok(interpret(&output))
}

/// Turns an `su -c "id -u; id"` result into a [`RootProbe`].
#[must_use]
pub fn interpret(output: &ExecOutput) -> RootProbe {
    if output.is_success() && is_root_identity(&output.stdout) {
        return RootProbe {
            available: true,
            identity: Some(identity_line(&output.stdout)),
            reason: None,
        };
    }

    let stderr = output.stderr.trim();
    let stdout = identity_line(&output.stdout);
    let reason = if !stderr.is_empty() {
        stderr.to_owned()
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("su 返回退出码 {}", output.code)
    };

    RootProbe {
        available: false,
        identity: None,
        reason: Some(reason),
    }
}

/// The human-readable identity: the `id` line, not the bare uid before it.
fn identity_line(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str, stderr: &str, code: i32) -> ExecOutput {
        ExecOutput {
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            code,
        }
    }

    #[test]
    fn adb_mode_args_are_minimal() {
        assert_eq!(
            shell_args(ExecMode::Adb, "logcat"),
            vec!["shell".to_owned(), "logcat".to_owned()]
        );
    }

    #[test]
    fn root_mode_args_wrap_with_su() {
        assert_eq!(
            shell_args(ExecMode::Root, "dmesg -w"),
            vec![
                "shell".to_owned(),
                "su".to_owned(),
                "-c".to_owned(),
                "dmesg -w".to_owned()
            ]
        );
    }

    #[test]
    fn root_identity_detection() {
        assert!(is_root_identity("uid=0(root) gid=0(root) groups=0(root)"));
        assert!(is_root_identity("0"));
        assert!(!is_root_identity("uid=2000(shell) gid=2000(shell)"));
        assert!(!is_root_identity(""));
    }

    #[test]
    fn root_identity_survives_a_localised_id() {
        // Measured on a real device: `su -c id` answered in Chinese. The old
        // token comparison (`uid=0(root)`) missed it and the app reported
        // "无 root" on a device that had just handed out uid 0.
        assert!(is_root_identity("用户id=0(root) 组id=0(root) 组=0(root)"));
        assert!(is_root_identity("0\n用户id=0(root) 组id=0(root) 组=0(root)"));
        // Groups being 0 is not evidence of root on its own.
        assert!(!is_root_identity("uid=2000(shell) gid=2000(shell) 组=0(root)"));
    }

    #[test]
    fn successful_probe_reports_the_id_line_as_identity() {
        let probe = interpret(&output("0\nuid=0(root) gid=0(root)\n", "", 0));
        assert!(probe.available);
        assert_eq!(probe.identity.as_deref(), Some("uid=0(root) gid=0(root)"));
        assert_eq!(probe.reason, None);
    }

    #[test]
    fn non_root_probe_does_not_leak_the_bare_uid_into_the_reason() {
        let probe = interpret(&output("2000\nuid=2000(shell) gid=2000(shell)\n", "", 0));
        assert!(!probe.available);
        assert_eq!(
            probe.reason.as_deref(),
            Some("uid=2000(shell) gid=2000(shell)")
        );
    }

    #[test]
    fn successful_probe_reports_available() {
        let probe = interpret(&output("uid=0(root) gid=0(root)\n", "", 0));
        assert!(probe.available);
        assert_eq!(probe.reason, None);
    }

    #[test]
    fn denied_probe_reports_stderr_reason() {
        let probe = interpret(&output("", "su: not found", 127));
        assert!(!probe.available);
        assert_eq!(probe.reason.as_deref(), Some("su: not found"));
    }

    #[test]
    fn non_root_identity_is_unavailable() {
        let probe = interpret(&output("uid=2000(shell)\n", "", 0));
        assert!(!probe.available);
        assert_eq!(probe.reason.as_deref(), Some("uid=2000(shell)"));
    }

    #[test]
    fn unavailable_helper_sets_reason() {
        let probe = RootProbe::unavailable("no device");
        assert!(!probe.available);
        assert_eq!(probe.reason.as_deref(), Some("no device"));
    }
}
