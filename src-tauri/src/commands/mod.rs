//! Tauri command boundary.
//!
//! Every command is a thin adapter: it resolves inputs, delegates to the module
//! that owns the behaviour, and lets [`crate::error::DroidLogError`] serialise
//! itself to the frontend. No business logic lives here.
//!
//! Argument naming follows Tauri's default: Rust `snake_case` parameters are
//! invoked from TypeScript as `camelCase`.

use tauri::{AppHandle, State};

use crate::adb::{locate, locate::AdbSource, Adb};
use crate::collect;
use crate::device::resolve::{self, AppTarget, RunningApp};
use crate::device::{self, DeviceInfo};
use crate::error::Result;
use crate::executor::ExecMode;
use crate::filter::FilterRule;
use crate::parser::LogRecord;
use crate::process::{self, CaptureRequest, CaptureSession, SessionSummary};
use crate::source::{self, SourceAvailability};
use crate::state::AppState;

/// Static build information, shown in the About/status area.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Crate name.
    pub name: String,
    /// Crate version.
    pub version: String,
    /// Operating system (`windows`, `linux`, `macos`).
    pub platform: String,
    /// CPU architecture.
    pub arch: String,
    /// True for a debug build.
    pub debug: bool,
}

/// Result of looking for the adb binary.
///
/// This never fails: "adb is missing" is an ordinary state the UI renders as an
/// empty-state hint, not an error toast.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdbProbe {
    /// Whether a usable binary was found.
    pub available: bool,
    /// Resolved path, when found.
    pub program: Option<String>,
    /// How it was resolved.
    pub source: Option<AdbSource>,
    /// First line of `adb version`, when it ran.
    pub version: Option<String>,
    /// Every candidate path considered, for troubleshooting.
    pub candidates: Vec<String>,
    /// Why it is unavailable, when it is.
    pub error: Option<String>,
}

/// Build metadata for the running application.
#[tauri::command]
#[must_use]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        debug: cfg!(debug_assertions),
    }
}

/// Locates adb and queries its version.
#[tauri::command]
pub async fn probe_adb() -> AdbProbe {
    let candidates = locate::candidate_paths()
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();

    match Adb::discover() {
        Ok(adb) => {
            // A missing version string is not fatal: the binary may still work.
            let version = adb.version().await.ok();
            AdbProbe {
                available: true,
                program: Some(adb.program().to_owned()),
                source: Some(adb.source()),
                version,
                candidates,
                error: None,
            }
        }
        Err(err) => AdbProbe {
            available: false,
            program: None,
            source: None,
            version: None,
            candidates,
            error: Some(err.to_string()),
        },
    }
}

/// Lists attached devices.
///
/// When `probe` is true, each usable device is additionally interrogated for its
/// Android version and root capability — two extra adb round trips per device,
/// so the UI enables it on demand rather than on every refresh.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::AdbUnavailable`] when adb is missing.
#[tauri::command]
pub async fn list_devices(probe: Option<bool>) -> Result<Vec<DeviceInfo>> {
    let adb = Adb::discover()?;
    let mut devices = adb.devices().await?;

    if probe.unwrap_or(false) {
        for device in devices.iter_mut().filter(|device| device.is_usable()) {
            // A failed probe leaves the fingerprint fields empty; the device row
            // is still worth showing.
            if let Ok(details) = device::probe_device(&adb, &device.serial).await {
                device.android_version = details.android_version;
                device.sdk = details.sdk;
                device.root_available = Some(details.root_available);
                device.root_reason = details.root_reason;
            }
        }
    }

    Ok(devices)
}

/// Lists every collector with its availability for the current privilege level.
#[tauri::command]
#[must_use]
pub fn list_sources(root_available: bool, recovery: bool) -> Vec<SourceAvailability> {
    source::availability(root_available, recovery)
}

/// Asks the device poller to re-probe now instead of at its next tick.
///
/// Returns immediately: the result arrives as a `droidlog://devices` event, so
/// the button that triggers this stays responsive and the device list has a
/// single writer (the poller) rather than two competing sources.
#[tauri::command]
#[must_use]
pub fn refresh_devices(state: State<'_, AppState>) -> bool {
    state.device_watcher().request_probe();
    true
}

/// Returns the active filter rules.
#[tauri::command]
#[must_use]
pub fn get_filters(state: State<'_, AppState>) -> Vec<FilterRule> {
    state.filters()
}

/// Replaces the active filter rules.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::InvalidFilter`] and leaves the previous
/// rules in place when any rule is invalid.
#[tauri::command]
pub fn set_filters(state: State<'_, AppState>, rules: Vec<FilterRule>) -> Result<()> {
    let summary = rules
        .iter()
        .filter(|rule| rule.enabled)
        .map(|rule| format!("{} {} {:?}", rule.field.label(), rule.op.label(), rule.value))
        .collect::<Vec<_>>()
        .join("; ");
    eprintln!(
        "droidlog: filters set ({} active): {}",
        rules.iter().filter(|rule| rule.enabled).count(),
        if summary.is_empty() { "none" } else { &summary }
    );
    state.set_filters(rules)
}

/// Starts a capture session.
///
/// # Errors
///
/// See [`process::start`].
#[tauri::command]
pub async fn start_capture(
    app: AppHandle,
    state: State<'_, AppState>,
    request: CaptureRequest,
) -> Result<CaptureSession> {
    process::start(&app, &state, request).await
}

/// Stops a capture session.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::SessionNotFound`] for an unknown id.
#[tauri::command]
pub fn stop_capture(state: State<'_, AppState>, session_id: String) -> Result<()> {
    process::stop(&state, &session_id)
}

/// Starts a crash-log collection (`logcat -b crash`, tombstones, dropbox).
///
/// Returns as soon as the session exists: probes run in the background and
/// report through `droidlog://records`, `droidlog://crash` and
/// `droidlog://collect-report`.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::SessionAlreadyRunning`] on an id
/// collision, which cannot happen in practice.
#[tauri::command]
pub fn collect_crash(
    app: AppHandle,
    state: State<'_, AppState>,
    request: collect::CollectRequest,
) -> Result<CaptureSession> {
    collect::start_crash(&app, &state, request)
}

/// Starts a boot-log collection: one snapshot, then polling until boot ends.
///
/// # Errors
///
/// Same as [`collect_crash`].
#[tauri::command]
pub fn collect_boot(
    app: AppHandle,
    state: State<'_, AppState>,
    request: collect::CollectRequest,
) -> Result<CaptureSession> {
    collect::start_boot(&app, &state, request)
}

/// Starts a recovery-mode log collection.
///
/// Recovery has no `logcat`; the probes read `/tmp/recovery.log`,
/// `/cache/recovery/last_log`, the kernel ring and pstore.
///
/// # Errors
///
/// Same as [`collect_crash`].
#[tauri::command]
pub fn collect_recovery(
    app: AppHandle,
    state: State<'_, AppState>,
    request: collect::CollectRequest,
) -> Result<CaptureSession> {
    collect::start_recovery(&app, &state, request)
}

/// The report of a finished collection run.
///
/// Returns `None` while the run is still going, so a reloaded window can ask
/// again rather than relying on having seen the event.
#[tauri::command]
#[must_use]
pub fn get_collect_report(
    state: State<'_, AppState>,
    session_id: String,
) -> Option<collect::CollectionReport> {
    state.collect_report(&session_id)
}

/// Resolves one line of user input into an application identity.
///
/// Accepts a PID, a package name or a UID and works out which it is, then
/// returns package, uid and every live pid (including `pkg:remote` children) so
/// the UI can render its chip. "Not running" is a normal result, not an error.
///
/// `serial` and `mode` come from the caller rather than from backend state: the
/// selected device and execution mode live in the UI, and mirroring them here
/// would create a second copy that can disagree.
///
/// # Errors
///
/// Propagates transport failures, and rejects empty or malformed input.
#[tauri::command]
pub async fn resolve_app(
    state: State<'_, AppState>,
    serial: String,
    mode: ExecMode,
    input: String,
    force: Option<bool>,
) -> Result<AppTarget> {
    let adb = Adb::discover()?;
    let resolver = state.app_resolver();
    resolve::resolve(&adb, mode, &serial, &input, &resolver, force.unwrap_or(false)).await
}

/// Sets — or clears, when `input` is blank — the application being followed.
///
/// # Errors
///
/// Propagates transport failures.
#[tauri::command]
pub async fn set_app_target(
    state: State<'_, AppState>,
    serial: String,
    mode: ExecMode,
    input: String,
) -> Result<Option<AppTarget>> {
    if input.trim().is_empty() {
        state.set_app_target(None);
        return Ok(None);
    }
    let adb = Adb::discover()?;
    let resolver = state.app_resolver();
    // `force`: an explicit selection must never be answered from a cache that
    // predates the user's action.
    let target = resolve::resolve(&adb, mode, &serial, &input, &resolver, true).await?;
    // Resolution is the sort of thing that is invisible when it goes wrong (the
    // table just stays empty), so the outcome is reported on the dev console.
    eprintln!(
        "droidlog: app target '{}' -> found={} package={:?} uid={:?} pids={:?}",
        target.input, target.found, target.package, target.uid, target.pids
    );
    state.set_app_target(Some(target.clone()));
    Ok(Some(target))
}

/// Returns the application currently being followed.
#[tauri::command]
#[must_use]
pub fn get_app_target(state: State<'_, AppState>) -> Option<AppTarget> {
    state.app_target()
}

/// Lists the running applications, for picking one without typing.
///
/// # Errors
///
/// Propagates transport failures.
#[tauri::command]
pub async fn list_running_apps(
    serial: String,
    mode: ExecMode,
) -> Result<Vec<RunningApp>> {
    let adb = Adb::discover()?;
    let rows = resolve::ps_table(&adb, mode, &serial).await?;
    Ok(resolve::running_apps(&rows))
}

/// Stops every running session in one call.
///
/// The frontend keeps this as its "stop all" path: issuing N separate
/// `stop_capture` calls would leave the UI half-stopped if one of them raced a
/// natural exit.
#[tauri::command]
#[must_use]
pub fn stop_all_captures(state: State<'_, AppState>) -> usize {
    let stopped = state.session_count();
    process::stop_all(&state);
    stopped
}

/// Lists all known sessions with their buffer counters.
#[tauri::command]
#[must_use]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<SessionSummary> {
    state.session_summaries()
}

/// Returns buffered records for a session, newest `limit` first-class.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::SessionNotFound`] for an unknown id.
#[tauri::command]
pub fn drain_records(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<usize>,
) -> Result<Vec<LogRecord>> {
    state.session_records(&session_id, limit)
}
