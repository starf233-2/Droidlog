//! Tauri command boundary.
//!
//! Every command is a thin adapter: it resolves inputs, delegates to the module
//! that owns the behaviour, and lets [`crate::error::DroidLogError`] serialise
//! itself to the frontend. No business logic lives here.
//!
//! Argument naming follows Tauri's default: Rust `snake_case` parameters are
//! invoked from TypeScript as `camelCase`.

use tauri::{AppHandle, Manager, State};

use crate::adb::{locate, locate::AdbSource, Adb};
use crate::collect;
use crate::device::resolve::{self, AppTarget, RunningApp};
use crate::device::{self, DeviceInfo};
use crate::error::Result;
use crate::executor::ExecMode;
use crate::export::{self, ExportFormat, ExportOutcome};
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
            // Remember which logcat buffers this device has, while we are already talking to
            // it and while the user is waiting on an explicit refresh — *not* when a capture
            // starts, which is the moment that must stay free of extra adb round trips. The
            // buffer list is a property of the ROM and does not depend on root, so the plain
            // `adb shell` mode is the right one to ask with.
            //
            // What this buys: a live session can add the `events` buffer when it exists and
            // leave it alone when it does not, and the integrity report can answer "is that
            // buffer missing?" with a fact instead of "cannot confirm".
            let profile =
                crate::collect::profile::detect(&adb, crate::types::ExecMode::Adb, &device.serial)
                    .await;
            crate::collect::profile::remember_available_buffers(&profile.logcat_buffers);
        }
    }

    Ok(devices)
}

/// Lists every collector with its availability for the current privilege level.
///
/// `module` is whether the KernelSU boot-log module is installed, which the device probe
/// reports; without it the module source is listed as unavailable with the reason, rather than
/// hidden — a collector that silently disappears is harder to understand than one that says why.
#[tauri::command]
#[must_use]
pub fn list_sources(
    root_available: bool,
    recovery: bool,
    module: bool,
) -> Vec<SourceAvailability> {
    source::availability(root_available, recovery, module)
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
    // No console dump here: the watch re-resolves every second, and printing a target
  // (with its full pid list) each time buried every other line and cost real work.
  // Resolution failures are visible where they matter — the target panel says so.
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

/// Writes collected rows to a file under the user's downloads folder.
///
/// Formatting happens in the frontend, which holds the rows and knows what is on
/// screen; this side owns the destination, the file name and the encoding, so
/// there is only ever one definition of "the exported line".
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::InvalidInput`] for an unwritable
/// destination or a name that does not match the format.
#[tauri::command]
pub fn export_records(
    app: AppHandle,
    content: String,
    format: ExportFormat,
    file_name: String,
) -> Result<ExportOutcome> {
    let dir = export::export_dir(app.path().download_dir().ok());
    let name = export::sanitize_file_name(&file_name, format);
    export::write(&dir, &name, &content, format)
}

/// Opens the file manager with an exported file selected.
///
/// The notice reporting the path is not selectable text (only logs are), so this
/// is how the user actually gets to the file.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError::InvalidInput`] when the path is
/// missing or lies outside the export folder.
#[tauri::command]
pub fn reveal_export(app: AppHandle, path: String) -> Result<()> {
    let dir = export::export_dir(app.path().download_dir().ok());
    export::reveal(&dir, std::path::Path::new(&path))
}

/// Lists installed applications with the names the **device** resolves.
///
/// An app's display name is reachable from no adb command, so a 4 KB dex embedded
/// in this binary is staged on the phone and run through `app_process`, asking
/// `PackageManager.getApplicationLabel()` for every package — 438 apps in 1.9 s on
/// the phone this was built against. Version, target SDK, install time and enabled
/// state come from a single `dumpsys package packages` dump.
///
/// The full list (system apps included) is cached on disk for
/// [`crate::device::installed::CACHE_TTL`], so toggling "show system apps" costs
/// nothing while the cache is fresh; filtering happens after the read.
///
/// # Errors
///
/// Returns [`crate::error::DroidLogError`] when adb cannot be located or the device
/// cannot be read.
#[tauri::command]
pub async fn list_installed_apps(
    app: AppHandle,
    serial: String,
    mode: ExecMode,
    include_system: bool,
) -> Result<Vec<crate::device::installed::InstalledApp>> {
    let adb = Adb::discover()?;
    let cache = app
        .path()
        .app_cache_dir()
        .ok()
        .map(|dir| crate::device::installed::cache_path(&dir, &serial));

    let all = match cache
        .as_ref()
        .and_then(|path| crate::device::installed::read_cache(path))
    {
        Some(cached) => cached,
        None => {
            let fetched = crate::device::installed::list(&adb, mode, &serial, true).await?;
            // Never cache a label-less list. That is what a failed probe yields, and
            // caching it would keep every application name wrong for the whole TTL
            // even after the device recovers — which is precisely the "real names for
            // a second, package names afterwards" report this guard exists for.
            if let Some(path) = cache.as_ref() {
                if fetched.iter().any(|entry| !entry.label.is_empty()) {
                    crate::device::installed::write_cache(path, &fetched);
                }
            }
            fetched
        }
    };

    Ok(all
        .into_iter()
        .filter(|entry| include_system || !entry.system)
        .collect())
}

/// Integrity findings for a session, for the collection report.
///
/// Item 8 of the crash-forensics work: the counters the reader accumulated
/// (`crash::session`) turned into the statements a report can make — dropped records,
/// the unparsed share, buffers the device does not have, time gaps, and crashes
/// without a stack. The buffers and crashes are still passed as empty lists because
/// the collector does not hand them over yet; the counter-based checks (records,
/// drops, unparsed) are already real, and an empty list is reported as "unknown"
/// rather than as "clean".
#[tauri::command]
pub fn get_integrity(state: State<'_, AppState>, session_id: String) -> Vec<crate::crash::integrity::IntegrityCheck> {
    // Which buffers the capture asked for, read from the session's own command line
    // (`logcat -v threadtime -b main,system,crash …`). That is real data with no device
    // access, and it is what makes "the `events` buffer is missing" answerable: the
    // *available* side stays empty here because the device's answer lives in the probe
    // outcomes, and an empty list is reported as "cannot confirm" rather than as a
    // missing buffer — claiming a buffer is absent without asking would be a guess.
    let requested_buffers: Vec<String> = state
        .session_summaries()
        .into_iter()
        .find(|summary| summary.session.id == session_id)
        .map(|summary| buffers_from_command(&summary.session.command))
        .unwrap_or_default();
    // The ring counts drops itself and its counter is absolute, so it is set as a value:
    // this command may run again for the same session and the numbers must not grow.
    let mut counters = crate::crash::session::counters(&session_id);
    if let Some(stats) = state
        .session_summaries()
        .into_iter()
        .find(|summary| summary.session.id == session_id)
        .map(|summary| summary.stats)
    {
        match counters.as_mut() {
            Some(existing) => existing.set_dropped(stats.total_dropped),
            None => {
                let mut fresh = crate::crash::integrity::CaptureCounters::new();
                fresh.set_dropped(stats.total_dropped);
                counters = Some(fresh);
            }
        }
    }
    crate::crash::pipeline::session_checks_with(
        counters,
        requested_buffers,
        crate::collect::profile::available_buffers(),
        crate::crash::live::crash_events(&session_id),
    )
}
/// Buffer names a logcat command asks for (`-b main,system,crash`).
///
/// Returns an empty list when the command has no `-b`, which is how a capture that
/// takes logcat's own default is described: the integrity check then reports that it
/// cannot name the requested buffers instead of inventing them.
fn buffers_from_command(command: &str) -> Vec<String> {
    let mut buffers = Vec::new();
    let mut tokens = command.split_whitespace();
    while let Some(token) = tokens.next() {
        let value = match token {
            "-b" => tokens.next(),
            other => other.strip_prefix("-b"),
        };
        let Some(value) = value else {
            continue;
        };
        for name in value.split(',') {
            let name = name.trim();
            if !name.is_empty() && !buffers.iter().any(|known| known == name) {
                buffers.push(name.to_owned());
            }
        }
    }
    buffers
}
/// Live crash events a session produced, for the timeline view.
///
/// The events come from the reader's per-session watch: crash blocks (replaced by id as
/// they grow), AMS signals and resource anomalies, each with the line index of the row
/// it came from so the timeline can jump back to it. Reading is a snapshot, not a drain,
/// because the view may be reopened.
#[tauri::command]
pub fn get_crash_events(session_id: String) -> Vec<crate::crash::live::LiveEvent> {
    crate::crash::live::events_snapshot(&session_id)
}
/// Everything the crash view needs for a session, computed from the session's own rows.
///
/// This is where the unified entry point (`pipeline::analyse`) finally runs on real data:
/// the session's captured text is handed to the analysers as-is — both the AMS and the
/// resource parsers receive the same lines and each finds what it recognises, which is
/// cheaper and more honest than pre-filtering with a second set of heuristics — and the
/// live watch supplies the structured crashes it already parsed.
///
/// The result carries a sequence map (see `ForensicsView`) because the analysers number
/// the lines they were given, while the table jumps by capture sequence.
#[tauri::command]
pub fn get_forensics(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<crate::crash::pipeline::ForensicsView> {
    build_forensics(state.inner(), &session_id)
}

/// Remembers the current capture's timeline, so a restart does not lose it.
///
/// Returns whether anything was written. A capture with no crash events is still worth
/// remembering — "nothing crashed" is a result — so only a missing directory or a failed write
/// makes this return `false`, and the reason goes to the dev console rather than to the user:
/// a timeline that could not be cached is a lost convenience, not a failed capture.
#[tauri::command]
pub fn save_timeline_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> bool {
    let Ok(view) = build_forensics(state.inner(), &session_id) else {
        eprintln!("[droidlog] 时间线快照未写入：无法分析会话 {session_id}");
        return false;
    };
    let snapshot = crate::crash::store::TimelineSnapshot::new(
        &session_id,
        crate::process::now_ms(),
        crate::crash::live::events_snapshot(&session_id),
        view,
    );
    let Ok(dir) = app.path().app_data_dir() else {
        eprintln!("[droidlog] 时间线快照未写入：无法定位应用数据目录");
        return false;
    };
    match crate::crash::store::save(&dir, &snapshot) {
        Ok(()) => true,
        Err(err) => {
            eprintln!("[droidlog] 时间线快照未写入：{err}");
            false
        }
    }
}

/// The remembered timeline, or `null` when there is nothing usable on disk.
///
/// Nothing usable includes a file from a future schema: a wrong timeline is worse than none.
#[tauri::command]
pub fn load_timeline_snapshot(app: AppHandle) -> Option<crate::crash::store::TimelineSnapshot> {
    let dir = app.path().app_data_dir().ok()?;
    crate::crash::store::load(&dir)
}

/// Forgets the remembered timeline.
#[tauri::command]
pub fn clear_timeline_snapshot(app: AppHandle) -> bool {
    let Ok(dir) = app.path().app_data_dir() else {
        return false;
    };
    crate::crash::store::clear(&dir).is_ok()
}

/// The analysis for one session, shared by the command and the snapshot writer.
///
/// Split out so that persisting a capture and answering the view's request cannot drift: both
/// run the same pipeline over the same rows, and there is one place where the line cap lives.
fn build_forensics(
    state: &AppState,
    session_id: &str,
) -> Result<crate::crash::pipeline::ForensicsView> {
    /// Upper bound on the rows analysed: a capture can hold a hundred thousand, and a
    /// report does not need all of them to describe what happened.
    const LINE_CAP: usize = 20_000;

    let records = state.session_records(session_id, Some(LINE_CAP))?;
    let mut lines: Vec<&str> = Vec::with_capacity(records.len());
    let mut seq_at: Vec<u64> = Vec::with_capacity(records.len());
    for record in &records {
        lines.push(record.raw.as_str());
        seq_at.push(record.seq);
    }

    let crashes = crate::crash::live::crash_events(session_id);
    let requested_buffers: Vec<String> = state
        .session_summaries()
        .into_iter()
        .find(|summary| summary.session.id == session_id)
        .map(|summary| buffers_from_command(&summary.session.command))
        .unwrap_or_default();

    let mut counters = crate::crash::session::counters(session_id);
    if let Some(stats) = state
        .session_summaries()
        .into_iter()
        .find(|summary| summary.session.id == session_id)
        .map(|summary| summary.stats)
    {
        match counters.as_mut() {
            Some(existing) => existing.set_dropped(stats.total_dropped),
            None => {
                let mut fresh = crate::crash::integrity::CaptureCounters::new();
                fresh.set_dropped(stats.total_dropped);
                counters = Some(fresh);
            }
        }
    }
    let available_buffers = crate::collect::profile::available_buffers();
    let integrity = match counters {
        Some(counters) => {
            counters.into_input(requested_buffers, available_buffers, crashes.clone())
        }
        None => crate::crash::integrity::IntegrityInput {
            requested_buffers,
            available_buffers,
            crashes: crashes.clone(),
            ..crate::crash::integrity::IntegrityInput::default()
        },
    };

    let analysis = crate::crash::pipeline::analyse(
        &crate::crash::pipeline::ForensicsInput {
            events: &crashes,
            ams_lines: &lines,
            resource_lines: &lines,
            integrity,
        },
        &crate::crash::integrity::Limits::default(),
        &crate::crash::ams::LinkOptions::default(),
    );
    Ok(crate::crash::pipeline::ForensicsView {
        analysis,
        seq_at,
        notice: capture_notice(&records, session_id),
    })
}
/// The capture-level remarks: mixed dates, and binary material that was left out.
///
/// Both are things the analysis cannot know (it only sees text) but the reader must be
/// told: one changes what "this capture" covers, the other explains missing material.
fn capture_notice(records: &[crate::parser::LogRecord], session_id: &str) -> Option<String> {
    let binary = crate::crash::session::counters(session_id)
        .map(|counters| counters.binary())
        .unwrap_or(0);
    let mut parts: Vec<String> = Vec::new();
    if let Some(dates) = mixed_date_notice(records) {
        parts.push(dates);
    }
    if binary > 0 {
        parts.push(format!(
            "已跳过 {binary} 行非文本内容（墓碑 .pb、压缩的 dropbox 等），它们没有混入文本流。"
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// A remark when a capture's rows do not all come from the same day.
///
/// The crash buffer is a ring: `logcat -b crash -d` legitimately returns crashes from
/// earlier days, so a capture taken on 10-02 can carry 10-01 rows. That is worth saying
/// out loud, because it silently changes what "this session" means — the timeline may
/// describe a crash that happened yesterday.
///
/// Dates are compared as text (`MM-DD`), not as instants: logcat timestamps have no year,
/// so a real comparison is impossible, and the honest statement is "more than one date",
/// with the counts.
fn mixed_date_notice(records: &[crate::parser::LogRecord]) -> Option<String> {
    let mut per_date: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for record in records {
        let Some(stamp) = record.timestamp.as_deref() else {
            continue;
        };
        let date = stamp.get(0..5).unwrap_or_default();
        if date.len() == 5 && date.as_bytes().get(2) == Some(&b'-') {
            *per_date.entry(date.to_owned()).or_insert(0) += 1;
        }
    }
    if per_date.len() < 2 {
        return None;
    }
    let total: usize = per_date.values().sum();
    let detail = per_date
        .iter()
        .map(|(date, count)| format!("{date} 有 {count} 行"))
        .collect::<Vec<_>>()
        .join("、");
    Some(format!(
        "检测到历史日志混入：本次采集 {total} 行来自不止一个日期（{detail}）。崩溃缓冲区是环形缓冲，会带回更早的崩溃。"
    ))
}
