//! Capture sessions: turning a request into a running log stream.
//!
//! A session is the unit the UI starts and stops. It owns:
//!
//! * an [`ExecTarget`](crate::executor::ExecTarget) plan (adb vs `su -c`);
//! * a child process streaming the source's output;
//! * a [`RingBuffer`](crate::ring::RingBuffer) of accepted records;
//! * a reader task that decodes, filters and batches records into Tauri events.
//!
//! The streaming loop itself lives in [`reader`] so this module stays about
//! lifecycle and types.

pub mod reader;

use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::oneshot;

use crate::adb::Adb;
use crate::device::resolve::AppResolver;
use crate::device::{AppTarget, Prefilter};
use crate::error::{DroidLogError, Result};
use crate::executor::ExecMode;
use crate::ring::{RingBuffer, RingStats};
use crate::source::{self, LogSourceKind, SourceOptions};
use crate::state::{AppState, LockExt};

use std::sync::Mutex as StdMutex;

pub use reader::{EVENT_RECORDS, EVENT_SESSION_STATUS};

/// Emitted whenever a bounded collection reports progress.
///
/// Progress needs its own channel rather than riding on the status event: status
/// changes a handful of times in a session, while the boot collector ticks every
/// couple of seconds, and the frontend keeps one session object per session
/// which has to be *updated* rather than replaced.
pub const EVENT_SESSION_PROGRESS: &str = "droidlog://session-progress";

/// Payload of [`EVENT_SESSION_PROGRESS`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProgressEvent {
    /// Session the progress belongs to.
    pub session_id: String,
    /// Current progress.
    pub progress: SessionProgress,
}

/// Capacity used when the caller expresses no preference.
pub const DEFAULT_SESSION_CAPACITY: usize = crate::ring::DEFAULT_CAPACITY;

/// What the frontend asks for when starting a capture.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    /// Target device serial.
    pub serial: String,
    /// Transport mode.
    pub mode: ExecMode,
    /// Collector to run.
    pub source: LogSourceKind,
    /// Per-source knobs.
    #[serde(default)]
    pub options: SourceOptions,
    /// Ring capacity override.
    #[serde(default)]
    pub capacity: Option<usize>,
}

/// Lifecycle state of a session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "message")]
pub enum SessionStatus {
    /// Process spawned, reader not yet confirmed.
    Starting,
    /// Streaming.
    Running,
    /// Finished cleanly (EOF or user stop).
    Stopped,
    /// Terminated because of an error.
    Failed(String),
}

impl SessionStatus {
    /// True while the session should still produce records.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

/// Immutable description of a session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSession {
    /// Stable id, unique within the process.
    pub id: String,
    /// Device the session reads from.
    pub serial: String,
    /// Transport mode in use.
    pub mode: ExecMode,
    /// Collector in use.
    pub source: LogSourceKind,
    /// Exact device-side command that was executed.
    pub command: String,
    /// Ring capacity for this session.
    pub capacity: usize,
    /// Wall-clock start time in milliseconds since the Unix epoch.
    pub started_at_ms: u64,
    /// Current lifecycle state.
    pub status: SessionStatus,
    /// Progress of a bounded collection; `None` for a streaming session.
    pub progress: Option<SessionProgress>,
}

/// Progress of a collection that ends by itself, for the UI countdown.
///
/// Only bounded collectors report progress: a `logcat` session runs until the
/// user stops it, so a countdown there would be a lie.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProgress {
    /// What is being collected.
    pub label: String,
    /// Wall-clock time the collection stops on its own, ms since the Unix epoch.
    pub ends_at_ms: u64,
    /// Ticks completed.
    pub done: u32,
    /// Ticks expected in total.
    pub total: u32,
}

/// A session plus its live buffer counters.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// Static session description and current status.
    pub session: CaptureSession,
    /// Ring occupancy and drop counters.
    pub stats: RingStats,
}

/// Runtime state for one session, held by [`AppState`].
pub struct SessionHandle {
    info: CaptureSession,
    buffer: Arc<StdMutex<RingBuffer<crate::parser::LogRecord>>>,
    /// Dropping the sender tells the reader task to shut down.
    shutdown: Option<oneshot::Sender<()>>,
}

impl SessionHandle {
    /// The session's description.
    #[must_use]
    pub fn info(&self) -> &CaptureSession {
        &self.info
    }

    /// Shared record buffer.
    #[must_use]
    pub fn buffer(&self) -> Arc<StdMutex<RingBuffer<crate::parser::LogRecord>>> {
        Arc::clone(&self.buffer)
    }

    /// Replaces the recorded status.
    pub fn set_status(&mut self, status: SessionStatus) {
        self.info.status = status;
    }

    /// Replaces the recorded collection progress.
    pub fn set_progress(&mut self, progress: SessionProgress) {
        self.info.progress = Some(progress);
    }

    /// Occupancy of the session's ring.
    #[must_use]
    pub fn stats(&self) -> RingStats {
        self.buffer.lock_ignore_poison().stats()
    }

    /// A serialisable view.
    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            session: self.info.clone(),
            stats: self.stats(),
        }
    }

    /// Signals the reader task to stop; the task kills the child itself.
    pub fn request_shutdown(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            // A send error means the reader already finished — not a failure.
            let _ = sender.send(());
        }
    }
}

/// Milliseconds since the Unix epoch, or 0 if the clock is before it.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Starts a capture session and registers it in `state`.
///
/// # Errors
///
/// * [`DroidLogError::AdbUnavailable`] when adb cannot be found;
/// * [`DroidLogError::InvalidInput`] when the mode cannot run the source;
/// * [`DroidLogError::Spawn`] when the child process cannot be started;
/// * [`DroidLogError::SessionAlreadyRunning`] on an id collision (should not
///   happen — ids come from a monotonic counter).
pub async fn start(
    app: &AppHandle,
    state: &AppState,
    request: CaptureRequest,
) -> Result<CaptureSession> {
    let spec = source::spec(request.source);
    if spec.requires_root && !request.mode.is_privileged() {
        return Err(DroidLogError::InvalidInput(format!(
            "{} 需要 root 模式，请切换到 Root（su -c）后重试",
            spec.label
        )));
    }

    let adb = Adb::discover()?;
    let source_impl = source::build(request.source);

    // A custom command is arbitrary text destined for the device shell, so it is
    // validated before anything is spawned.
    if let Some(custom) = request.options.custom_command() {
        source::validate_custom_command(custom)?;
    }

    // Fold the followed application into the session: how the stream is narrowed
    // on the device is decided here, because it depends on the collector.
    let mut options = request.options.clone();
    let mut target = state.app_target();
    if let Some(active) = target.as_mut() {
        let resolver = state.app_resolver();
        apply_target_to_options(&adb, &request, active, &mut options, &resolver).await;
        // Remember the device-side narrowing for the UI and for status events.
        state.set_app_target(target.clone());
    }
    let command = source_impl.command(&options);

    // The server must already be running before we spawn the stream. `adb shell`
    // inherits its stdio from us, so if this call were the one to bootstrap the
    // daemon, the daemon would inherit the capture's pipes and go deaf as soon
    // as the session ends. See `crate::adb::server`.
    adb.ensure_server().await?;

    let child = adb
        .target(request.mode, Some(&request.serial))
        .spawn_stream(&command)?;

    let capacity = request.capacity.unwrap_or(DEFAULT_SESSION_CAPACITY);
    let session_id = state.next_session_id();
    let info = CaptureSession {
        id: session_id.clone(),
        serial: request.serial.clone(),
        mode: request.mode,
        source: request.source,
        command,
        capacity,
        started_at_ms: now_ms(),
        status: SessionStatus::Running,
        progress: None,
    };

    let buffer = Arc::new(StdMutex::new(RingBuffer::new(capacity)));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let handle = SessionHandle {
        info: info.clone(),
        buffer: Arc::clone(&buffer),
        shutdown: Some(shutdown_tx),
    };

    state.insert_session(handle)?;

    reader::spawn(
        app.clone(),
        session_id,
        child,
        source_impl,
        buffer,
        shutdown_rx,
    );

    Ok(info)
}

/// Decides how a session narrows the stream on the device, and records the
/// decision on the target so the host-side filter knows what is left to do.
///
/// The preference order matters:
///
/// * **uid, when the device supports `logcat --uid=`** — a uid is stable across
///   an app restart, so the device-side narrowing cannot go stale;
/// * **pids, otherwise** — correct at start, but it silently stops matching if
///   the app restarts, which is exactly why the host-side filter is authoritative
///   and why the target is re-resolved every 30 s;
/// * **nothing** for `dmesg`/`kmsg`, which have no per-process notion at all —
///   those are filtered entirely on the host.
async fn apply_target_to_options(
    adb: &Adb,
    request: &CaptureRequest,
    target: &mut AppTarget,
    options: &mut SourceOptions,
    resolver: &AppResolver,
) {
    if request.source != LogSourceKind::Logcat {
        target.set_prefilter(Prefilter::None);
        return;
    }

    let supports_uid = crate::device::resolve::probe_logcat_uid(
        adb,
        request.mode,
        &request.serial,
        resolver,
    )
    .await
    .unwrap_or(false);

    if supports_uid && target.uid.is_some() {
        // Hand the uid to logcat and stop matching pids here: after a restart the
        // pid list is stale, and re-checking it would discard the very records
        // the device just correctly selected.
        options.uid = target.uid;
        options.pids.clear();
        target.set_prefilter(Prefilter::Uid);
        return;
    }

    if !target.pids.is_empty() {
        options.pids = target.pids.clone();
        target.set_prefilter(Prefilter::Pid);
        return;
    }

    target.set_prefilter(Prefilter::None);
}

/// Opens a session whose records are produced by collector probes rather than
/// by a single streaming child process.
///
/// The session is registered exactly like a streaming one, so the UI can page,
/// filter and export it with no special case; only the producer differs.
///
/// # Errors
///
/// Returns [`DroidLogError::SessionAlreadyRunning`] on an id collision, which
/// cannot happen in practice — ids come from a monotonic counter.
pub fn open_manual_session(
    state: &AppState,
    serial: &str,
    mode: ExecMode,
    source: LogSourceKind,
    command: String,
    capacity: usize,
) -> Result<(CaptureSession, Arc<StdMutex<RingBuffer<crate::parser::LogRecord>>>)> {
    let info = CaptureSession {
        id: state.next_session_id(),
        serial: serial.to_owned(),
        mode,
        source,
        command,
        capacity,
        started_at_ms: now_ms(),
        status: SessionStatus::Running,
        progress: None,
    };

    let buffer = Arc::new(StdMutex::new(RingBuffer::new(capacity)));
    let handle = SessionHandle {
        info: info.clone(),
        buffer: Arc::clone(&buffer),
        // No reader task to stop: the collector owns its own lifetime.
        shutdown: None,
    };
    state.insert_session(handle)?;

    Ok((info, buffer))
}

/// Stops a running session.
///
/// # Errors
///
/// Returns [`DroidLogError::SessionNotFound`] when `session_id` is unknown.
pub fn stop(state: &AppState, session_id: &str) -> Result<()> {
    let mut handle = state
        .remove_session(session_id)
        .ok_or_else(|| DroidLogError::SessionNotFound(session_id.to_owned()))?;
    handle.request_shutdown();
    Ok(())
}

/// Stops every session; used on shutdown.
pub fn stop_all(state: &AppState) {
    for mut handle in state.take_sessions() {
        handle.request_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_activity_flag() {
        assert!(SessionStatus::Starting.is_active());
        assert!(SessionStatus::Running.is_active());
        assert!(!SessionStatus::Stopped.is_active());
        assert!(!SessionStatus::Failed("boom".to_owned()).is_active());
    }

    #[test]
    fn status_serialises_as_adjacently_tagged_union() -> Result<()> {
        let json = serde_json::to_value(SessionStatus::Failed("boom".to_owned()))?;
        assert_eq!(json.get("state").and_then(|v| v.as_str()), Some("failed"));
        assert_eq!(json.get("message").and_then(|v| v.as_str()), Some("boom"));

        let stopped = serde_json::to_value(SessionStatus::Stopped)?;
        assert_eq!(stopped.get("state").and_then(|v| v.as_str()), Some("stopped"));
        Ok(())
    }

    #[test]
    fn capture_request_deserialises_from_frontend_payload() -> Result<()> {
        let json = r#"{
            "serial": "emulator-5554",
            "mode": "root",
            "source": "dmesg",
            "options": { "pids": [42], "tags": [], "buffers": [] },
            "capacity": 500
        }"#;
        let request: CaptureRequest = serde_json::from_str(json)?;
        assert_eq!(request.serial, "emulator-5554");
        assert_eq!(request.mode, ExecMode::Root);
        assert_eq!(request.source, LogSourceKind::Dmesg);
        assert_eq!(request.capacity, Some(500));
        assert_eq!(request.options.pids, vec![42]);
        Ok(())
    }

    #[test]
    fn capture_request_options_default_to_empty() -> Result<()> {
        let json = r#"{"serial":"S","mode":"adb","source":"logcat"}"#;
        let request: CaptureRequest = serde_json::from_str(json)?;
        assert!(request.options.pids.is_empty());
        assert_eq!(request.capacity, None);
        Ok(())
    }

    #[test]
    fn now_ms_is_after_the_epoch() {
        assert!(now_ms() > 0);
    }
}
