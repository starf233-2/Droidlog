//! Command execution: *where* a command runs, not *what* it means.
//!
//! Two execution modes exist for every capture:
//!
//! * [`ExecMode::Adb`] — `adb -s SERIAL shell <cmd>`, the unprivileged path.
//! * [`ExecMode::Root`] — `adb -s SERIAL shell su -c <cmd>`, which is required
//!   for `dmesg` / `kmsg` on most production devices.
//!
//! The mode is a first-class value ([`ExecMode`]) rather than a boolean so that
//! adding e.g. a wireless-adb or a local-shell mode later is additive.

pub mod root;

use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::error::{DroidLogError, Result};

/// `CREATE_NO_WINDOW`, the Windows process-creation flag that suppresses the
/// console a console subsystem child would otherwise be given.
///
/// This is not cosmetic. The packaged application is built for the `windows`
/// subsystem and therefore has **no console**; every console child it starts
/// (`adb.exe`) is then handed a brand-new console window. The device poller runs
/// `adb devices` every two seconds, so the shipped build showed a stream of
/// terminal windows popping up for as long as the app was open — measured on the
/// packaged build: 8 console windows in 14 s.
///
/// It is invisible under `cargo tauri dev`, because there the parent is a console
/// process and children inherit its console instead of getting a new one, which
/// is exactly why this survived every development-stage check.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Builds the child process for `plan`, with the platform's console suppression.
///
/// Every spawn in this module goes through here so the flag cannot be forgotten
/// at a new call site — the failure mode (windows flashing on the user's screen)
/// is otherwise easy to miss during development.
fn command_for(plan: &CommandPlan) -> Command {
    let mut command = Command::new(&plan.program);
    command.args(&plan.args);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Transport used to reach a device shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecMode {
    /// Plain `adb shell`, subject to the device's normal app/shell permissions.
    #[default]
    Adb,
    /// `su -c` on the device; required for kernel ring buffers.
    Root,
}

impl ExecMode {
    /// Whether this mode escalates privileges on the device.
    #[must_use]
    pub fn is_privileged(self) -> bool {
        matches!(self, Self::Root)
    }

    /// All modes, in UI display order.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::Adb, Self::Root]
    }
}

impl std::fmt::Display for ExecMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Adb => "adb",
            Self::Root => "root",
        })
    }
}

/// Result of a completed (non-streaming) command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutput {
    /// Lossily decoded stdout — device output is not guaranteed to be UTF-8.
    pub stdout: String,
    /// Lossily decoded stderr.
    pub stderr: String,
    /// Process exit code, or `-1` when terminated by a signal.
    pub code: i32,
}

impl ExecOutput {
    /// True when the process exited successfully.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.code == 0
    }

    /// stdout split into lines, with trailing blank lines removed.
    #[must_use]
    pub fn stdout_lines(&self) -> Vec<&str> {
        let mut lines: Vec<&str> = self.stdout.lines().collect();
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        lines
    }
}

/// A fully resolved argv, ready to hand to the OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    /// Absolute path or bare name of the executable.
    pub program: String,
    /// Arguments, already escaped for the transport.
    pub args: Vec<String>,
}

impl CommandPlan {
    /// Human-readable form used in error messages and UI diagnostics.
    #[must_use]
    pub fn display(&self) -> String {
        let mut out = self.program.clone();
        for arg in &self.args {
            out.push(' ');
            // Quote only when the argument would otherwise be ambiguous.
            if arg.contains(' ') {
                out.push('"');
                out.push_str(arg);
                out.push('"');
            } else {
                out.push_str(arg);
            }
        }
        out
    }
}

/// Binds an [`ExecMode`] to a concrete device and adb binary.
#[derive(Debug, Clone)]
pub struct ExecTarget<'a> {
    /// Escalation strategy.
    pub mode: ExecMode,
    /// Device serial; `None` lets adb pick when exactly one device is attached.
    pub serial: Option<&'a str>,
    /// Path to (or name of) the `adb` executable.
    pub adb_program: &'a str,
}

impl<'a> ExecTarget<'a> {
    /// Creates a target.
    #[must_use]
    pub fn new(mode: ExecMode, serial: Option<&'a str>, adb_program: &'a str) -> Self {
        Self {
            mode,
            serial,
            adb_program,
        }
    }

    /// Builds the argv that runs `remote_command` inside the device shell.
    ///
    /// adb escapes each argument for the device shell itself, so `remote_command`
    /// is passed as a single argument and reaches `su -c` intact.
    #[must_use]
    pub fn plan(&self, remote_command: &str) -> CommandPlan {
        let mut args: Vec<String> = Vec::new();
        if let Some(serial) = self.serial {
            args.push("-s".to_owned());
            args.push(serial.to_owned());
        }
        args.extend(root::shell_args(self.mode, remote_command));

        CommandPlan {
            program: self.adb_program.to_owned(),
            args,
        }
    }

    /// Runs `remote_command` to completion and captures its output.
    ///
    /// # Errors
    ///
    /// Returns [`DroidLogError::Spawn`] when adb is missing and
    /// [`DroidLogError::AdbCommandFailed`] on a non-zero exit.
    pub async fn run(&self, remote_command: &str) -> Result<ExecOutput> {
        let plan = self.plan(remote_command);
        let output = run_once(&plan).await?;
        if output.is_success() {
            Ok(output)
        } else {
            Err(DroidLogError::command_failed(
                plan.display(),
                output.code,
                output.stderr.trim().to_owned(),
            ))
        }
    }

    /// Runs `remote_command` to completion, tolerating a non-zero exit.
    ///
    /// Used for capability probes such as `su -c id`, where "it failed" is a
    /// legitimate answer rather than an error.
    pub async fn run_tolerant(&self, remote_command: &str) -> Result<ExecOutput> {
        run_once(&self.plan(remote_command)).await
    }

    /// Starts `remote_command` as a long-running stream with piped stdio.
    ///
    /// The caller owns the [`Child`] and is responsible for draining and killing it.
    ///
    /// # Errors
    ///
    /// Returns [`DroidLogError::Spawn`] when the process cannot be started.
    pub fn spawn_stream(&self, remote_command: &str) -> Result<Child> {
        let plan = self.plan(remote_command);
        let mut command = command_for(&plan);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        command
            .spawn()
            .map_err(|source| DroidLogError::spawn(plan.display(), source))
    }
}

/// Runs a plan to completion, capturing stdout/stderr and the exit code.
///
/// # Errors
///
/// Returns [`DroidLogError::Spawn`] if the executable cannot be started.
pub async fn run_once(plan: &CommandPlan) -> Result<ExecOutput> {
    let output = command_for(plan)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|source| DroidLogError::spawn(plan.display(), source))?;

    Ok(ExecOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    })
}

/// Runs a plan with every stdio stream bound to the null device, discarding all
/// output and returning only the exit code.
///
/// This is not just tidiness. `adb start-server` leaves a long-lived daemon
/// behind that **inherits the launching process's stdio handles**, so
/// bootstrapping it from a piped reader (which is how every other call in this
/// crate runs) leaves the daemon holding pipes that close the moment we drop
/// them. The daemon then stops answering and every later call fails with
/// `could not read ok from ADB Server`. Detaching the bootstrap removes that
/// failure mode: the daemon inherits the null device instead, which never breaks.
///
/// # Errors
///
/// Returns [`DroidLogError::Spawn`] if the process cannot be started, or
/// [`DroidLogError::Internal`] if it overruns `timeout` (the process is killed
/// first, so a wedged daemon cannot pin this call open forever).
pub async fn run_detached(plan: &CommandPlan, timeout: Duration) -> Result<i32> {
    let mut command = command_for(plan);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|source| DroidLogError::spawn(plan.display(), source))?;

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status.code().unwrap_or(-1)),
        Ok(Err(source)) => Err(DroidLogError::Io(source)),
        Err(_elapsed) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(DroidLogError::Internal(format!(
                "`{}` 在 {}s 内没有返回",
                plan.display(),
                timeout.as_secs()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_mode_wraps_with_su() {
        let target = ExecTarget::new(ExecMode::Root, Some("ABC123"), "adb");
        let plan = target.plan("dmesg -w");
        assert_eq!(
            plan.args,
            vec!["-s", "ABC123", "shell", "su", "-c", "dmesg -w"]
        );
    }

    #[test]
    fn adb_mode_has_no_su() {
        let target = ExecTarget::new(ExecMode::Adb, None, "adb");
        let plan = target.plan("logcat -v threadtime");
        assert_eq!(plan.args, vec!["shell", "logcat -v threadtime"]);
    }

    #[test]
    fn display_quotes_only_when_needed() {
        let target = ExecTarget::new(ExecMode::Adb, Some("S"), "adb");
        assert_eq!(
            target.plan("logcat -v threadtime").display(),
            "adb -s S shell \"logcat -v threadtime\""
        );
    }

    #[test]
    fn exec_mode_serialises_lowercase() -> Result<()> {
        assert_eq!(serde_json::to_string(&ExecMode::Root)?, "\"root\"");
        assert_eq!(
            serde_json::from_str::<ExecMode>("\"adb\"")?,
            ExecMode::Adb
        );
        Ok(())
    }

    #[test]
    fn blank_trailing_lines_are_trimmed() {
        let output = ExecOutput {
            stdout: "a\nb\n\n\n".to_owned(),
            stderr: String::new(),
            code: 0,
        };
        assert_eq!(output.stdout_lines(), vec!["a", "b"]);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_detached_reports_the_exit_code() -> Result<()> {
        let plan = CommandPlan {
            program: "cmd".to_owned(),
            args: vec!["/c".to_owned(), "exit".to_owned(), "3".to_owned()],
        };
        assert_eq!(run_detached(&plan, Duration::from_secs(10)).await?, 3);
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_detached_kills_a_wedged_process_at_the_deadline() {
        // Ten one-second pings: without the timeout this would block for ~9s.
        let plan = CommandPlan {
            program: "ping".to_owned(),
            args: vec![
                "-n".to_owned(),
                "10".to_owned(),
                "127.0.0.1".to_owned(),
            ],
        };
        let started = std::time::Instant::now();
        let result = run_detached(&plan, Duration::from_millis(400)).await;

        assert!(result.is_err(), "a wedged process must be reported");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the call must return at its deadline, not after the child finishes"
        );
    }
}
