//! Boot, crash and recovery collection.
//!
//! The streaming collectors in [`crate::process`] read one long-lived command.
//! These three cannot: their evidence is scattered across files, some of which
//! may not exist and some of which may be unreadable, and the UI has to be told
//! which is which. So collection is expressed as a list of [`Probe`]s run in
//! sequence, where each probe decides for itself whether it produced records
//! (["collect it"]) or a diagnosis (["switch to root"]).
//!
//! All three share one ingestion path — [`Ingestor`] — which is deliberately the
//! same filter/target/batching logic the streaming reader uses, so a crash
//! session behaves exactly like a `logcat` session in the UI.

pub mod boot;
pub mod probe;
pub mod profile;

use std::sync::{Arc, Mutex as StdMutex};

use tauri::{AppHandle, Emitter, Manager};

use crate::adb::Adb;
use crate::crash::{self, CrashKind};
use crate::device::AppTarget;
use crate::error::Result;
use crate::executor::ExecMode;
use crate::filter::FilterSet;
use crate::parser::LogRecord;
use crate::process::reader::{self, RecordsEvent, SessionStatusEvent};
use crate::process::{CaptureSession, SessionStatus, DEFAULT_SESSION_CAPACITY};
use crate::ring::RingBuffer;
use crate::source::LogSourceKind;
use crate::state::{AppState, LockExt};

use probe::{Probe, ProbeOutcome, ProbeStatus};
use profile::{DeviceProfile, KernelEra};

pub use probe::{Probe as CollectProbe, ProbeStatus as CollectProbeStatus};

/// Emitted with batches of `{seq, kind}` for records the classifier recognised.
pub const EVENT_CRASH: &str = "droidlog://crash";

/// Emitted once when a collection run finishes, with its [`CollectionReport`].
pub const EVENT_COLLECT_REPORT: &str = "droidlog://collect-report";

/// Records per emitted batch; mirrors the streaming reader.
pub const BATCH_SIZE: usize = 200;

/// Lines ingested per probe at most. The newest lines win.
pub const MAX_PROBE_LINES: usize = 4000;

/// Kernel lines remembered between two polls of the boot collector.
///
/// **Must exceed [`MAX_PROBE_LINES`]**, because that is how many lines a poll can
/// ingest: the fingerprint has to cover everything that was just emitted, or the
/// next poll re-emits the part it no longer remembers. It was 512, and a device
/// whose `logcat -b kernel` returns thousands of lines therefore ingested the
/// whole buffer again on every tick (measured: 4000 records for a 4-second boot
/// window that had produced nothing new).
pub const TAIL_MEMORY: usize = MAX_PROBE_LINES + 512;

// A poll can ingest `MAX_PROBE_LINES` lines, so the fingerprint has to remember at
// least that many — otherwise the next poll re-emits what it no longer remembers.
// Checked at compile time: this is a relationship, not a preference.
const _: () = assert!(TAIL_MEMORY > MAX_PROBE_LINES);

/// One recognised crash, as the UI needs it: which row, and which family.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashEntry {
    /// `seq` of the record that matched.
    pub seq: u64,
    /// Failure family.
    pub kind: CrashKind,
}

/// Payload of [`EVENT_CRASH`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashEvent {
    /// Session the entries belong to.
    pub session_id: String,
    /// Recognised crashes, oldest first.
    pub entries: Vec<CrashEntry>,
}

/// What a collection run found, probe by probe.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionReport {
    /// Session the report belongs to.
    pub session_id: String,
    /// Collector that ran.
    pub source: LogSourceKind,
    /// One outcome per probe, in execution order.
    pub outcomes: Vec<ProbeOutcome>,
    /// Probes that yielded records.
    pub sources_found: usize,
    /// Records accepted across all probes.
    pub records: usize,
    /// Wall-clock finish time, milliseconds since the Unix epoch.
    pub finished_at_ms: u64,
    /// Nothing found, with the reason the probes gave.
    pub failure: Option<String>,
}

impl CollectionReport {
    /// Builds a report from finished probe outcomes.
    #[must_use]
    pub fn new(
        session_id: String,
        source: LogSourceKind,
        outcomes: Vec<ProbeOutcome>,
        finished_at_ms: u64,
    ) -> Self {
        let sources_found = outcomes.iter().filter(|o| o.status == ProbeStatus::Found).count();
        let records = outcomes.iter().map(|o| o.records).sum();
        let failure = if outcomes.iter().any(|outcome| outcome.status.is_ok()) {
            None
        } else {
            Some("设备上没有可读取的日志源：请查看各项提示后重试".to_owned())
        };
        Self {
            session_id,
            source,
            outcomes,
            sources_found,
            records,
            finished_at_ms,
            failure,
        }
    }

    /// Probes that produced records.
    #[must_use]
    pub fn found_labels(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|o| o.status == ProbeStatus::Found)
            .map(|o| o.label.as_str())
            .collect()
    }

    /// Every remedy the report can offer, de-duplicated, most specific first.
    #[must_use]
    pub fn hints(&self) -> Vec<&str> {
        let mut hints: Vec<&str> = Vec::new();
        for hint in self.outcomes.iter().filter_map(|o| o.hint.as_deref()) {
            if !hints.contains(&hint) {
                hints.push(hint);
            }
        }
        hints
    }
}

/// Request for one collection run, from the frontend.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectRequest {
    /// Target device serial.
    pub serial: String,
    /// Transport mode in use.
    pub mode: ExecMode,
    /// Ring capacity override.
    #[serde(default)]
    pub capacity: Option<usize>,
    /// Boot collection: total polling window, milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Boot collection: poll interval, milliseconds.
    #[serde(default)]
    pub interval_ms: Option<u64>,
}

/// Ingests probe output the same way a streaming session ingests its stream.
///
/// It owns the batch, so a probe producing ten thousand lines still reaches the
/// frontend in [`BATCH_SIZE`] chunks, and the ring — not the event — stays the
/// durable copy.
pub(crate) struct Ingestor {
    app: AppHandle,
    session_id: String,
    source: LogSourceKind,
    buffer: Arc<StdMutex<RingBuffer<LogRecord>>>,
    filters: FilterSet,
    filters_version: u64,
    target: Option<AppTarget>,
    target_version: u64,
    batch: Vec<LogRecord>,
    crashes: Vec<CrashEntry>,
    accepted: usize,
}

impl Ingestor {
    pub(crate) fn new(
        app: &AppHandle,
        session_id: String,
        source: LogSourceKind,
        buffer: Arc<StdMutex<RingBuffer<LogRecord>>>,
    ) -> Self {
        Self {
            app: app.clone(),
            session_id,
            source,
            buffer,
            filters: FilterSet::accept_all(),
            filters_version: 0,
            target: None,
            target_version: 0,
            batch: Vec::with_capacity(BATCH_SIZE),
            crashes: Vec::new(),
            accepted: 0,
        }
    }

    /// Records accepted so far, across every probe of this run.
    pub(crate) fn accepted(&self) -> usize {
        self.accepted
    }

    /// Re-reads the shared filters and followed application if they changed.
    fn refresh(&mut self, state: &AppState) {
        let current = state.filters_version();
        if current != self.filters_version {
            if let Ok(compiled) = state.compiled_filters() {
                self.filters = compiled;
            }
            self.filters_version = current;
        }
        let current = state.app_target_version();
        if current != self.target_version {
            self.target = state.app_target();
            self.target_version = current;
        }
    }

    /// Feeds every line of `text`, then flushes.
    pub(crate) fn feed_text(&mut self, text: &str) -> usize {
        // The handle is cloned first: `state()` borrows the handle it is called
        // on, so calling it through `self.app` would freeze `self` for as long
        // as the guard lives.
        let app = self.app.clone();
        let state = app.state::<AppState>();
        self.refresh(&state);
        let mut accepted = 0;
        for line in text.lines() {
            if self.feed_line(line, &state) {
                accepted += 1;
            }
        }
        self.flush();
        accepted
    }

    /// Feeds one line; returns whether it survived the filters.
    pub(crate) fn feed_line(&mut self, line: &str, state: &AppState) -> bool {
        if line.trim().is_empty() {
            return false;
        }
        let record = crate::parser::parse_auto(self.source, line, state.next_seq());
        if !self.filters.matches(&record) || !reader::target_matches(self.target.as_ref(), &record) {
            return false;
        }

        ids::push_crashes(&record, &mut self.crashes);
        self.accepted += 1;
        self.batch.push(record);
        if self.batch.len() >= BATCH_SIZE {
            self.flush();
        }
        true
    }

    /// Publishes the pending batch: ring first, then the fast-path event.
    pub(crate) fn flush(&mut self) {
        if self.batch.is_empty() && self.crashes.is_empty() {
            return;
        }

        if !self.batch.is_empty() {
            {
                let mut ring = self.buffer.lock_ignore_poison();
                for record in self.batch.iter() {
                    ring.push(record.clone());
                }
            }
            let records = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH_SIZE));
            let _ = self.app.emit(
                reader::EVENT_RECORDS,
                RecordsEvent {
                    session_id: self.session_id.clone(),
                    records,
                },
            );
        }

        if !self.crashes.is_empty() {
            let entries = std::mem::take(&mut self.crashes);
            let _ = self.app.emit(
                EVENT_CRASH,
                CrashEvent {
                    session_id: self.session_id.clone(),
                    entries,
                },
            );
        }
    }

    /// Feeds only the lines the previous poll did not see, then remembers the
    /// tail of this one.
    ///
    /// The kernel ring buffer is append-only between polls, so comparing each
    /// line against the previous tail is enough to avoid re-emitting the whole
    /// buffer every tick — and it costs one hash set instead of a growing log.
    pub(crate) fn feed_incremental(
        &mut self,
        text: &str,
        skip: &std::collections::HashSet<u64>,
    ) -> (usize, std::collections::HashSet<u64>) {
        // See `feed_text`: the handle is cloned so the state guard does not
        // borrow `self`.
        let app = self.app.clone();
        let state = app.state::<AppState>();
        self.refresh(&state);
        let mut accepted = 0;
        for line in text.lines() {
            if skip.contains(&line_hash(line)) {
                continue;
            }
            if self.feed_line(line, &state) {
                accepted += 1;
            }
        }
        self.flush();

        let mut tail = std::collections::HashSet::with_capacity(TAIL_MEMORY);
        for line in text.lines().rev().take(TAIL_MEMORY) {
            tail.insert(line_hash(line));
        }
        (accepted, tail)
    }

    /// Publishes collection progress, which drives the boot countdown.
    pub(crate) fn set_progress(&self, label: &str, ends_at_ms: u64, done: u32, total: u32) {
        let state = self.app.state::<AppState>();
        let progress = crate::process::SessionProgress {
            label: label.to_owned(),
            ends_at_ms,
            done,
            total,
        };
        state.set_session_progress(&self.session_id, progress.clone());
        // The UI holds a session object from the moment collection started, and
        // nothing else would ever tell it the tick count changed.
        let _ = self.app.emit(
            crate::process::EVENT_SESSION_PROGRESS,
            crate::process::SessionProgressEvent {
                session_id: self.session_id.clone(),
                progress,
            },
        );
    }

    /// Publishes a terminal status for the session.
    pub(crate) fn settle(&self, status: SessionStatus) {
        let state = self.app.state::<AppState>();
        state.set_session_status(&self.session_id, status.clone());
        let _ = self.app.emit(
            reader::EVENT_SESSION_STATUS,
            SessionStatusEvent {
                session_id: self.session_id.clone(),
                status,
            },
        );
    }

    /// Stores and publishes the run's report.
    pub(crate) fn publish_report(&self, report: &CollectionReport) {
        let state = self.app.state::<AppState>();
        state.set_collect_report(report.clone());
        let _ = self.app.emit(EVENT_COLLECT_REPORT, report.clone());
    }
}

/// Crash classification of a single record, kept in one place so the streaming
/// reader and the collectors agree on what a crash is.
mod ids {
    use super::{crash, CrashEntry, LogRecord};

    /// Appends a [`CrashEntry`] when `record` looks like a failure.
    pub(super) fn push_crashes(record: &LogRecord, out: &mut Vec<CrashEntry>) {
        if !crash::is_scannable(record.level) {
            return;
        }
        if let Some(kind) = crash::classify_record(record) {
            out.push(CrashEntry {
                seq: record.seq,
                kind,
            });
        }
    }
}

/// Starts a crash-log collection and returns the session immediately.
///
/// # Errors
///
/// Returns [`DroidLogError::SessionAlreadyRunning`] on an id collision, which
/// cannot happen in practice — ids come from a monotonic counter.
pub fn start_crash(
    app: &AppHandle,
    state: &AppState,
    request: CollectRequest,
) -> Result<CaptureSession> {
    start(app, state, request, LogSourceKind::Crash, Run::Crash)
}

/// Starts a recovery-log collection.
///
/// # Errors
///
/// Same as [`start_crash`].
pub fn start_recovery(
    app: &AppHandle,
    state: &AppState,
    request: CollectRequest,
) -> Result<CaptureSession> {
    start(app, state, request, LogSourceKind::Recovery, Run::Recovery)
}

/// Starts a boot-log collection: one snapshot, then polling until boot
/// completes or the window runs out.
///
/// # Errors
///
/// Same as [`start_crash`].
pub fn start_boot(
    app: &AppHandle,
    state: &AppState,
    request: CollectRequest,
) -> Result<CaptureSession> {
    start(app, state, request, LogSourceKind::Boot, Run::Boot)
}

/// Which probe list a run uses.
#[derive(Debug, Clone, Copy)]
enum Run {
    Crash,
    Recovery,
    Boot,
}

impl Run {
    /// Every probe this run could use, before the device has been asked.
    ///
    /// Only for the session's command summary: which of them actually run is
    /// decided by [`Run::probes`] against the device profile.
    fn candidate_probes(self) -> &'static [Probe] {
        match self {
            Self::Crash => probe::CRASH_PROBES,
            Self::Recovery => probe::RECOVERY_PROBES,
            Self::Boot => probe::BOOT_PROBES,
        }
    }

    /// The probes worth running on this device.
    ///
    /// The list adapts because the device's answers differ by generation, and a
    /// probe that cannot succeed is worse than no probe: it turns into a
    /// "No such file or directory" line in the report that looks like a fault in
    /// this app rather than a fact about the phone.
    fn probes(self, profile: &DeviceProfile) -> Vec<Probe> {
        self.candidate_probes()
            .iter()
            .copied()
            .filter(|probe| keep_probe(probe.id, profile))
            .collect()
    }

    fn label(self) -> &'static str {
        match self {
            Self::Crash => "崩溃日志",
            Self::Recovery => "Recovery 日志",
            Self::Boot => "启动日志",
        }
    }
}

/// Whether a probe can answer anything on this device.
fn keep_probe(id: &str, profile: &DeviceProfile) -> bool {
    match id {
        // A ROM without the `events` buffer answers this with `logcat: Logcat read
        // failure: No such file or directory` — measured on Android 17 / 5.10.
        "crash-events" => profile.has_buffer("events"),
        // logcat's `kernel` buffer is the modern, unrooted way to the kernel ring;
        // `dmesg` is only worth trying when that buffer is absent (otherwise the
        // two would ingest the same lines twice).
        "boot-kernel" | "crash-kernel" => profile.has_buffer("kernel"),
        "boot-dmesg" => !profile.has_buffer("kernel"),
        // `/proc/last_kmsg` was removed in 3.5, the same release that brought
        // pstore in: exactly one of the two exists on any given kernel.
        "boot-last-kmsg" => profile.era.reads_last_kmsg(),
        "boot-pstore" | "recovery-pstore" => profile.era.reads_pstore(),
        _ => true,
    }
}

/// Opens the session and spawns the run behind it.
fn start(
    app: &AppHandle,
    state: &AppState,
    request: CollectRequest,
    source: LogSourceKind,
    run: Run,
) -> Result<CaptureSession> {
    let capacity = request.capacity.unwrap_or(DEFAULT_SESSION_CAPACITY);
    let probes = run.candidate_probes();
    let command = format!(
        "probe: {}",
        probes
            .iter()
            .map(|probe| probe.id)
            .collect::<Vec<_>>()
            .join(", ")
    );

    let (session, buffer) = crate::process::open_manual_session(
        state,
        &request.serial,
        request.mode,
        source,
        command,
        capacity,
    )?;

    let app_handle = app.clone();
    let session_id = session.id.clone();
    let source_for_task = source;
    let label = run.label();

    tauri::async_runtime::spawn(async move {
        run_collection(app_handle, session_id, source_for_task, label, buffer, request, run).await;
    });

    Ok(session)
}

/// The body of a collection run: discover adb, probe, report, settle.
async fn run_collection(
    app: AppHandle,
    session_id: String,
    source: LogSourceKind,
    label: &'static str,
    buffer: Arc<StdMutex<RingBuffer<LogRecord>>>,
    request: CollectRequest,
    run: Run,
) {
    let mut ingest = Ingestor::new(&app, session_id.clone(), source, buffer);

    let adb = match Adb::discover() {
        Ok(adb) => adb,
        Err(err) => {
            fail(&ingest, &session_id, source, &format!("无法定位 adb：{err}"));
            return;
        }
    };
    if let Err(err) = adb.ensure_server().await {
        fail(
            &ingest,
            &session_id,
            source,
            &format!("adb 服务不可用：{err}"),
        );
        return;
    }

    // Ask the device what it can give us before asking it for anything: which
    // kernel generation it runs (pstore vs /proc/last_kmsg) and which logcat
    // buffers the ROM actually has.
    let profile = profile::detect(&adb, request.mode, &request.serial).await;
    for note in &profile.notes {
        eprintln!("droidlog: {label}：{note}");
    }

    let outcomes = match run {
        Run::Crash | Run::Recovery => {
            let probes = run.probes(&profile);
            let mut outcomes = vec![profile_outcome(&profile)];
            outcomes.extend(
                run_probes(&adb, request.mode, &request.serial, &probes, &mut ingest).await,
            );
            outcomes
        }
        Run::Boot => {
            let mut outcomes = vec![profile_outcome(&profile)];
            outcomes.extend(
                boot::collect(&adb, request.mode, &request.serial, &mut ingest, &request, &profile)
                    .await,
            );
            outcomes
        }
    };

    ingest.flush();
    let report = CollectionReport::new(
        session_id.clone(),
        source,
        outcomes,
        crate::process::now_ms(),
    );
    eprintln!(
        "droidlog: {label}采集完成：{} 个源，{} 条记录（摄取 {} 条）",
        report.sources_found,
        report.records,
        ingest.accepted()
    );
    ingest.publish_report(&report);
    ingest.settle(settle_status(&report));
}

/// Reports a run that could not even start probing.
fn fail(ingest: &Ingestor, session_id: &str, source: LogSourceKind, message: &str) {
    eprintln!("droidlog: collect {session_id} failed: {message}");
    let report = CollectionReport {
        session_id: session_id.to_owned(),
        source,
        outcomes: Vec::new(),
        sources_found: 0,
        records: 0,
        finished_at_ms: crate::process::now_ms(),
        failure: Some(message.to_owned()),
    };
    ingest.publish_report(&report);
    ingest.settle(SessionStatus::Failed(message.to_owned()));
}

/// Terminal status of a finished run.
///
/// An empty-but-readable source is a success: a device that has not crashed
/// since boot is exactly what `logcat -b crash` returning nothing means.
fn settle_status(report: &CollectionReport) -> SessionStatus {
    match &report.failure {
        Some(message) => SessionStatus::Failed(message.clone()),
        None => SessionStatus::Stopped,
    }
}

/// The report entry that explains which paths this device was read through.
///
/// Without it the report looks like a list of failures on a modern phone: the
/// legacy paths are *supposed* to be missing there.
fn profile_outcome(profile: &DeviceProfile) -> ProbeOutcome {
    ProbeOutcome {
        id: "device-profile".to_owned(),
        label: format!("设备能力探测（内核 {}）", profile.era.label()),
        command: "uname -r; logcat -g".to_owned(),
        status: ProbeStatus::Found,
        records: 0,
        files: Vec::new(),
        detail: Some(profile.notes.join("；")),
        hint: match profile.era {
            KernelEra::Pstore => Some(
                "本机为 pstore/ramoops 世代：上次启动的内核日志在 /sys/fs/pstore（console-ramoops / console-ramoops-0 / pmsg-ramoops-0），/proc/last_kmsg 在 3.5 之后已被移除".to_owned(),
            ),
            KernelEra::LegacyLastKmsg => Some(
                "本机内核 ≤3.4：上次启动的内核日志在 /proc/last_kmsg，pstore 目录在该世代尚不存在".to_owned(),
            ),
            KernelEra::Unknown => None,
        },
    }
}

/// Runs `probes` in order, ingesting what each one produced.
pub(crate) async fn run_probes(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    probes: &[Probe],
    ingest: &mut Ingestor,
) -> Vec<ProbeOutcome> {
    let mut outcomes = Vec::with_capacity(probes.len());
    for probe in probes {
        outcomes.push(run_probe(adb, mode, serial, *probe, ingest).await);
    }
    outcomes
}

/// Runs one probe, including the files of a directory listing.
async fn run_probe(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    probe: Probe,
    ingest: &mut Ingestor,
) -> ProbeOutcome {
    let target = adb.target(mode, Some(serial));
    let output = match target.run_tolerant(probe.command).await {
        Ok(output) => output,
        Err(err) => {
            return ProbeOutcome {
                id: probe.id.to_owned(),
                label: probe.label.to_owned(),
                command: probe.command.to_owned(),
                status: ProbeStatus::Failed,
                records: 0,
                files: Vec::new(),
                detail: Some(err.to_string()),
                hint: ProbeStatus::Failed.hint().map(str::to_owned),
            };
        }
    };

    let status = probe::classify_output(&output);
    let mut detail = detail_of(&output.stderr);
    let mut records = 0;
    let mut files = Vec::new();

    if probe.ingest && status == ProbeStatus::Found {
        let (text, trimmed) = probe::tail_lines(&output.stdout, MAX_PROBE_LINES);
        records = ingest.feed_text(&text);
        if trimmed {
            detail = Some(format!(
                "输出超过 {MAX_PROBE_LINES} 行，仅保留最新部分"
            ));
        }
    }

    // A directory probe is only the index; the crash dumps themselves are the
    // evidence, so the newest few are read back one by one. When the listing
    // itself was refused, fall back to the names the kernel always uses (pstore)
    // instead of giving up on the directory entirely.
    if probe.dir.is_some() {
        let names: Vec<String> = if status == ProbeStatus::Found {
            let listed = probe::listed_files(&output.stdout, probe::MAX_FOLLOW_UP_FILES);
            if listed.is_empty() {
                detail = Some("目录为空".to_owned());
            }
            listed
        } else {
            probe::known_files(probe.id)
                .iter()
                .take(probe::MAX_FOLLOW_UP_FILES)
                .map(|name| (*name).to_owned())
                .collect()
        };
        for name in names {
            let Some(dir) = probe.dir else { break };
            let command = probe::follow_up(dir, &name);
            match target.run_tolerant(&command).await {
                Ok(file_output) => {
                    let file_status = probe::classify_output(&file_output);
                    files.push(name.clone());
                    if file_status == ProbeStatus::Found {
                        let (text, _) = probe::tail_lines(&file_output.stdout, MAX_PROBE_LINES);
                        records += ingest.feed_text(&text);
                    } else {
                        detail = Some(format!("{name}: {}", file_status.label()));
                    }
                }
                Err(err) => {
                    detail = Some(format!("{name}: {err}"));
                }
            }
        }
    }

    ProbeOutcome {
        id: probe.id.to_owned(),
        label: probe.label.to_owned(),
        command: probe.command.to_owned(),
        status,
        records,
        files,
        hint: if status == ProbeStatus::Found && records == 0 {
            None
        } else {
            status.hint().map(str::to_owned)
        },
        detail,
    }
}

/// Hashes one output line for the boot collector's tail comparison.
fn line_hash(line: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

/// The tail fingerprint of a buffer, without ingesting any of it.
///
/// Used to align the boot poller with the snapshot it follows: the snapshot
/// already emitted the whole kernel ring, and re-emitting it on the first tick
/// would look like the device repeating itself.
pub(crate) fn tail_hashes(text: &str) -> std::collections::HashSet<u64> {
    let mut tail = std::collections::HashSet::with_capacity(TAIL_MEMORY);
    for line in text.lines().rev().take(TAIL_MEMORY) {
        tail.insert(line_hash(line));
    }
    tail
}

/// First non-empty line of stderr, trimmed for display.
fn detail_of(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, status: ProbeStatus, records: usize) -> ProbeOutcome {
        ProbeOutcome {
            id: id.to_owned(),
            label: id.to_owned(),
            command: format!("cmd {id}"),
            status,
            records,
            files: Vec::new(),
            detail: None,
            hint: status.hint().map(str::to_owned),
        }
    }

    #[test]
    fn report_counts_sources_and_records() {
        let report = CollectionReport::new(
            "s1".to_owned(),
            LogSourceKind::Crash,
            vec![
                outcome("a", ProbeStatus::Found, 12),
                outcome("b", ProbeStatus::Empty, 0),
                outcome("c", ProbeStatus::Denied, 0),
            ],
            7,
        );
        assert_eq!(report.sources_found, 1);
        assert_eq!(report.records, 12);
        assert!(report.failure.is_none());
        assert_eq!(report.found_labels(), vec!["a"]);
        assert!(report.hints().iter().any(|hint| hint.contains("Root")));
    }

    #[test]
    fn a_healthy_device_is_not_a_failure() {
        // Nothing crashed since boot: the buffers exist and are empty.
        let report = CollectionReport::new(
            "s2".to_owned(),
            LogSourceKind::Crash,
            vec![
                outcome("crash-buffer", ProbeStatus::Empty, 0),
                outcome("tombstone-dir", ProbeStatus::Missing, 0),
            ],
            1,
        );
        assert!(report.failure.is_none());
        assert!(matches!(settle_status(&report), SessionStatus::Stopped));
    }

    #[test]
    fn no_readable_source_is_a_failure_with_a_reason() {
        let report = CollectionReport::new(
            "s3".to_owned(),
            LogSourceKind::Boot,
            vec![
                outcome("boot-dmesg", ProbeStatus::Restricted, 0),
                outcome("boot-last-kmsg", ProbeStatus::Missing, 0),
            ],
            1,
        );
        let failure = report.failure.clone().unwrap_or_default();
        assert!(failure.contains("没有可读取的日志源"));
        assert!(matches!(settle_status(&report), SessionStatus::Failed(_)));
    }

    #[test]
    fn crash_entries_are_only_pushed_for_scannable_levels() {
        use crate::parser::LogLevel;
        use crate::source::LogSourceKind;
        let mut record = LogRecord::raw_line(LogSourceKind::Crash, 5, "kernel panic - not syncing");
        record.level = LogLevel::Error;
        let mut out = Vec::new();
        ids::push_crashes(&record, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 5);

        // Verbose rows are skipped: the classifier would otherwise match the
        // word "panic" inside ordinary chatter.
        let mut quiet = LogRecord::raw_line(LogSourceKind::Crash, 6, "kernel panic - not syncing");
        quiet.level = LogLevel::Verbose;
        let mut out = Vec::new();
        ids::push_crashes(&quiet, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn detail_keeps_the_first_useful_line_only() {
        assert_eq!(detail_of("\n  \nls: nothing here\nmore"), Some("ls: nothing here".to_owned()));
        assert_eq!(detail_of("   "), None);
    }

    #[test]
    fn event_names_are_namespaced() {
        assert!(EVENT_CRASH.starts_with("droidlog://"));
        assert!(EVENT_COLLECT_REPORT.starts_with("droidlog://"));
        assert_ne!(EVENT_CRASH, EVENT_COLLECT_REPORT);
    }

    #[test]
    fn request_deserialises_with_and_without_boot_knobs() -> Result<()> {
        let json = r#"{"serial":"S","mode":"root"}"#;
        let request: CollectRequest = serde_json::from_str(json)?;
        assert_eq!(request.mode, ExecMode::Root);
        assert!(request.duration_ms.is_none());

        let json = r#"{"serial":"S","mode":"adb","durationMs":60000,"intervalMs":1500}"#;
        let request: CollectRequest = serde_json::from_str(json)?;
        assert_eq!(request.duration_ms, Some(60_000));
        assert_eq!(request.interval_ms, Some(1_500));
        Ok(())
    }
}
