//! The `adb` transport: binary resolution plus the handful of one-shot queries
//! droidlog needs (`version`, `devices`, `shell`).
//!
//! Long-running captures are not built here — [`crate::executor::ExecTarget`]
//! owns process spawning so that streaming stays in one place.

pub mod locate;
pub mod server;

use crate::device::{parse_device_list, DeviceInfo};
use crate::error::{DroidLogError, Result};
use crate::executor::{run_once, CommandPlan, ExecMode, ExecOutput, ExecTarget};
use locate::AdbSource;

/// A resolved adb installation.
#[derive(Debug, Clone)]
pub struct Adb {
    program: String,
    source: AdbSource,
}

impl Adb {
    /// Resolves adb from the environment.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DroidLogError::AdbUnavailable`] when no binary exists.
    pub fn discover() -> Result<Self> {
        let (path, source) = locate::discover()?;
        Ok(Self {
            program: path.to_string_lossy().into_owned(),
            source,
        })
    }

    /// Wraps an explicit binary path or name without touching the filesystem.
    #[must_use]
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            source: AdbSource::EnvOverride,
        }
    }

    /// Path (or bare name) of the adb executable.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// How the binary was resolved.
    #[must_use]
    pub fn source(&self) -> AdbSource {
        self.source
    }

    /// Builds an execution target for a device under the given mode.
    #[must_use]
    pub fn target<'a>(&'a self, mode: ExecMode, serial: Option<&'a str>) -> ExecTarget<'a> {
        ExecTarget::new(mode, serial, &self.program)
    }

    /// `adb version`.
    pub fn version_plan(&self) -> CommandPlan {
        CommandPlan {
            program: self.program.clone(),
            args: vec!["version".to_owned()],
        }
    }

    /// `adb devices -l`.
    pub fn devices_plan(&self) -> CommandPlan {
        CommandPlan {
            program: self.program.clone(),
            args: vec!["devices".to_owned(), "-l".to_owned()],
        }
    }

    /// Runs `adb version`, returning the first output line.
    ///
    /// # Errors
    ///
    /// Propagates spawn failures and non-zero exits.
    pub async fn version(&self) -> Result<String> {
        let plan = self.version_plan();
        let output = run_once(&plan).await?;
        if !output.is_success() {
            return Err(crate::error::DroidLogError::command_failed(
                plan.display(),
                output.code,
                output.stderr.trim().to_owned(),
            ));
        }
        Ok(output
            .stdout_lines()
            .first()
            .map_or_else(String::new, |line| (*line).trim().to_owned()))
    }

    /// Runs `adb devices -l` and parses the attached device list.
    ///
    /// # Errors
    ///
    /// Propagates spawn failures and non-zero exits.
    pub async fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let output = self.devices_output().await?;
        Ok(parse_device_list(&output.stdout))
    }

    /// Runs `adb devices -l`, returning raw output for diagnostics.
    ///
    /// # Errors
    ///
    /// Propagates spawn failures and non-zero exits.
    pub async fn devices_output(&self) -> Result<ExecOutput> {
        self.devices_query(true).await
    }

    /// Like [`Adb::devices_output`], but skips the pre-flight `start-server`.
    ///
    /// For callers that poll on a timer: running `adb start-server` every couple
    /// of seconds would spawn a process forever for no benefit once the server is
    /// known to be up. The escalating recovery below still starts or rebuilds the
    /// server whenever a query actually fails, so skipping the pre-flight cannot
    /// leave the app without a daemon — it only delays that work until it is
    /// needed.
    ///
    /// The *first* call from such a caller must still use
    /// [`Adb::devices_output`] (or [`Adb::ensure_server`]), because a query that
    /// bootstraps the daemon implicitly hands it our pipes.
    ///
    /// # Errors
    ///
    /// Propagates spawn failures and non-zero exits.
    pub async fn devices_output_reusing_server(&self) -> Result<ExecOutput> {
        self.devices_query(false).await
    }

    /// Shared implementation behind the two `devices_output*` entry points.
    ///
    /// Recovery is deliberately **escalating**, because the two ways a daemon
    /// breaks need different medicine:
    ///
    /// 1. *Lost a cold-start race* — the winner's daemon is alive by now, so a
    ///    plain retry succeeds. Restarting here would kill a perfectly good
    ///    daemon that another tool just started.
    /// 2. *Wedged daemon* — a retry fails too, so the server is rebuilt.
    async fn devices_query(&self, ensure_first: bool) -> Result<ExecOutput> {
        if ensure_first {
            self.ensure_server().await?;
        }
        let plan = self.devices_plan();

        let first = run_once(&plan).await?;
        if first.is_success() {
            return Ok(first);
        }
        if !should_rebuild_server(&first) {
            return Err(DroidLogError::command_failed(
                plan.display(),
                first.code,
                first.stderr.trim().to_owned(),
            ));
        }

        // Step 1: the daemon is probably up now (we merely lost the race).
        // Best-effort: if this fails, the retry below still runs and its error
        // is the authoritative one.
        let _ = self.ensure_server().await;
        let second = run_once(&plan).await?;
        if second.is_success() {
            return Ok(second);
        }

        // Step 2: still deaf, so the daemon itself is wedged. Rebuild it.
        self.restart_server().await?;
        let third = run_once(&plan).await?;
        if third.is_success() {
            return Ok(third);
        }

        Err(DroidLogError::command_failed(
            plan.display(),
            third.code,
            format!("{}\n{}", third.stderr.trim(), server::RECOVERY_FAILED_HINT),
        ))
    }

    /// Ensures the adb server is running, with its stdio detached from ours.
    ///
    /// # Errors
    ///
    /// Propagates [`server::ensure`]'s errors.
    pub async fn ensure_server(&self) -> Result<()> {
        server::ensure(&self.program).await
    }

    /// Kills and rebuilds the adb server.
    ///
    /// # Errors
    ///
    /// Propagates [`server::restart`]'s errors.
    pub async fn restart_server(&self) -> Result<()> {
        server::restart(&self.program).await
    }

    /// Runs a one-shot command inside the device shell under `mode`.
    ///
    /// # Errors
    ///
    /// Propagates transport and non-zero-exit failures.
    pub async fn shell(
        &self,
        mode: ExecMode,
        serial: Option<&str>,
        remote_command: &str,
    ) -> Result<ExecOutput> {
        self.ensure_server().await?;
        self.target(mode, serial).run(remote_command).await
    }
}

/// Whether a failed `adb devices -l` result means the server needs rebuilding.
///
/// Kept separate from [`Adb::devices_output`] so the recovery *policy* can be
/// tested on its own: a device-level error must never restart the server, while
/// a deaf-daemon error must.
#[must_use]
pub fn should_rebuild_server(output: &ExecOutput) -> bool {
    !output.is_success() && server::is_daemon_failure(&output.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether a failed `adb devices -l` should trigger a server rebuild.
    ///
    /// Extracted from [`Adb::devices_output`] so the recovery policy is
    /// unit-testable without a real adb binary or a live daemon.
    fn rebuild_needed(output: &ExecOutput) -> bool {
        should_rebuild_server(output)
    }

    fn failure(stderr: &str) -> ExecOutput {
        ExecOutput {
            stdout: String::new(),
            stderr: stderr.to_owned(),
            code: 1,
        }
    }

    #[test]
    fn version_plan_is_adb_version() {
        let adb = Adb::with_program("adb");
        assert_eq!(adb.version_plan().args, vec!["version".to_owned()]);
    }

    #[test]
    fn devices_plan_requests_long_format() {
        let adb = Adb::with_program("adb");
        assert_eq!(
            adb.devices_plan().args,
            vec!["devices".to_owned(), "-l".to_owned()]
        );
    }

    #[test]
    fn target_carries_program_and_serial() {
        let adb = Adb::with_program("/opt/platform-tools/adb");
        let plan = adb
            .target(ExecMode::Adb, Some("SER1"))
            .plan("logcat -v threadtime");
        assert_eq!(plan.program, "/opt/platform-tools/adb");
        assert_eq!(plan.args.first().map(String::as_str), Some("-s"));
        assert_eq!(plan.args.get(1).map(String::as_str), Some("SER1"));
    }

    #[test]
    fn explicit_program_reports_env_override_origin() {
        let adb = Adb::with_program("adb");
        assert_eq!(adb.program(), "adb");
        assert_eq!(adb.source(), AdbSource::EnvOverride);
    }

    #[test]
    fn a_deaf_daemon_triggers_a_rebuild() {
        // This is the exact failure the app used to surface verbatim.
        let stderr = "* daemon not running; starting now at tcp:5037\n\
                      could not read ok from ADB Server\n\
                      * failed to start daemon\n\
                      adb.exe: failed to check server version: cannot connect to daemon";
        assert!(rebuild_needed(&failure(stderr)));
    }

    #[test]
    fn a_successful_query_never_triggers_a_rebuild() {
        let ok = ExecOutput {
            stdout: "List of devices attached\n".to_owned(),
            stderr: String::new(),
            code: 0,
        };
        assert!(!rebuild_needed(&ok));
    }

    #[test]
    fn ordinary_device_errors_do_not_restart_the_server() {
        // Restarting the server here would be a wasteful, surprising side effect.
        assert!(!rebuild_needed(&failure("error: no devices/emulators found")));
        assert!(!rebuild_needed(&failure("error: device 'ABC' not found")));
        assert!(!rebuild_needed(&failure("")));
    }
}
