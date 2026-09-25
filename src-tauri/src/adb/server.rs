//! adb server (daemon) lifecycle.
//!
//! Every `adb` client talks to a background **server** on `127.0.0.1:5037`, which
//! the first client silently spawns. On Windows that spawn has two properties
//! that make adb front-ends fail in a very specific way:
//!
//! 1. the daemon **inherits the launcher's stdio handles**, and
//! 2. it outlives the client that started it.
//!
//! So a daemon bootstrapped from a client whose stdout/stderr are pipes is left
//! holding descriptors that close as soon as the client drops them. It then
//! stops answering, and every later call fails with:
//!
//! ```text
//! * daemon not running; starting now at tcp:5037
//! could not read ok from ADB Server
//! * failed to start daemon
//! adb.exe: failed to check server version: cannot connect to daemon
//! ```
//!
//! droidlog therefore never lets the daemon be bootstrapped implicitly. It calls
//! [`ensure`] first, which runs `adb start-server` with **all three streams on
//! the null device** (see [`crate::executor::run_detached`]), so the daemon it
//! creates inherits a null device that cannot break.
//!
//! The second hazard is a **cold-start race**, which is the failure that was
//! actually observed in practice:
//!
//! ```text
//! * daemon not running; starting now at tcp:5037
//! could not read ok from ADB Server
//! * failed to start daemon
//! adb.exe: failed to check server version: cannot connect to daemon
//! ```
//!
//! `adb start-server` is not atomic. When no daemon is running and two clients
//! start at once, exactly one wins the bind on 5037 and the others die with the
//! text above. Reproduced here with four simultaneous clients: one succeeded,
//! three failed. Because losing the race is *transient* — the winner's daemon is
//! up moments later — the caller retries before escalating, and only rebuilds
//! the server if a retry still fails (see [`crate::adb::Adb::devices_output`]).

use std::time::Duration;

use crate::error::{DroidLogError, Result};
use crate::executor::{run_detached, CommandPlan};

/// Budget for `adb start-server`. It normally returns in well under a second;
/// this is a guard against a wedged daemon, not a performance budget.
pub const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for `adb kill-server`.
pub const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// stderr fragments that mean *the daemon is unusable*, as opposed to adb having
/// run fine and reported a device-level problem.
///
/// Matched case-insensitively; adb has kept these strings stable for years.
const DAEMON_FAILURE_MARKERS: [&str; 6] = [
    "could not read ok from adb server",
    "failed to start daemon",
    "cannot connect to daemon",
    "failed to check server version",
    "adb server didn't ack",
    "connection refused",
];

/// Hint appended when recovery itself fails.
pub const RECOVERY_FAILED_HINT: &str =
    "（已重试并重建 adb server，仍未成功；5037 端口可能被其它程序占用，或被安全软件拦截）";

/// `adb start-server`.
#[must_use]
pub fn start_plan(program: &str) -> CommandPlan {
    CommandPlan {
        program: program.to_owned(),
        args: vec!["start-server".to_owned()],
    }
}

/// `adb kill-server`.
#[must_use]
pub fn kill_plan(program: &str) -> CommandPlan {
    CommandPlan {
        program: program.to_owned(),
        args: vec!["kill-server".to_owned()],
    }
}

/// True when `text` looks like a broken/missing adb server rather than a normal
/// command failure.
#[must_use]
pub fn is_daemon_failure(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let folded = text.to_ascii_lowercase();
    DAEMON_FAILURE_MARKERS
        .iter()
        .any(|marker| folded.contains(marker))
}

/// Starts the adb server if it is not already running.
///
/// Safe to call before every daemon-dependent command: when the server is
/// already up this connects and returns almost immediately.
///
/// # Errors
///
/// Returns [`DroidLogError::AdbCommandFailed`] when `adb start-server` exits
/// non-zero, or [`DroidLogError::Internal`] if it overruns [`BOOTSTRAP_TIMEOUT`].
pub async fn ensure(program: &str) -> Result<()> {
    let plan = start_plan(program);
    let code = run_detached(&plan, BOOTSTRAP_TIMEOUT).await?;
    if code == 0 {
        Ok(())
    } else {
        Err(DroidLogError::command_failed(
            plan.display(),
            code,
            "adb server 启动失败",
        ))
    }
}

/// Kills any running adb server and starts a fresh one, safely detached.
///
/// # Errors
///
/// Propagates [`ensure`]'s errors. A failed `kill-server` is ignored on purpose:
/// it usually just means there was nothing to kill.
pub async fn restart(program: &str) -> Result<()> {
    let kill = kill_plan(program);
    let _ = run_detached(&kill, KILL_TIMEOUT).await;
    ensure(program).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_are_well_formed() {
        assert_eq!(start_plan("adb").args, vec!["start-server".to_owned()]);
        assert_eq!(kill_plan("/x/adb").args, vec!["kill-server".to_owned()]);
        assert_eq!(kill_plan("/x/adb").program, "/x/adb");
    }

    #[test]
    fn recognises_the_reported_failure_verbatim() {
        // The exact text the user hit, as adb emits it.
        let stderr = "* daemon not running; starting now at tcp:5037\n\
                      could not read ok from ADB Server\n\
                      * failed to start daemon\n\
                      adb.exe: failed to check server version: cannot connect to daemon";
        assert!(is_daemon_failure(stderr));
    }

    #[test]
    fn recognises_each_marker_individually() {
        for marker in DAEMON_FAILURE_MARKERS {
            assert!(
                is_daemon_failure(&format!("adb.exe: {marker}")),
                "marker not detected: {marker}"
            );
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_daemon_failure("Cannot Connect To Daemon"));
    }

    #[test]
    fn ordinary_failures_are_not_daemon_failures() {
        // A device-level problem must not trigger a server restart.
        assert!(!is_daemon_failure("error: device 'ABC' not found"));
        assert!(!is_daemon_failure("error: no devices/emulators found"));
        assert!(!is_daemon_failure(""));
    }

    #[test]
    fn timeouts_are_generous_but_bounded() {
        assert!(BOOTSTRAP_TIMEOUT >= Duration::from_secs(5));
        assert!(KILL_TIMEOUT >= Duration::from_secs(1));
    }
}
