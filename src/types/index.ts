/**
 * TypeScript mirror of the Rust IPC surface.
 *
 * This file is a literal translation of the `serde` output of the types in
 * `src-tauri/src/types.rs`: same names, camelCase, and `Option<T>` mapped to
 * `T | null` (serde writes `null`, never `undefined`).
 *
 * `any` is banned project-wide; anything genuinely unknown is modelled as
 * `unknown` and narrowed.
 */

/* -------------------------------------------------------------- primitives */

/** Transport used to reach the device shell. */
export type ExecMode = 'adb' | 'root'

/** Collector identifier. */
export type LogSourceKind =
  | 'logcat'
  | 'dmesg'
  | 'kmsg'
  | 'crash'
  | 'boot'
  | 'recovery'
  /**
   * The KernelSU boot-log module's rescued kernel evidence.
   *
   * Offered only when the device has the module installed *and* the session runs in Root mode:
   * the files live under `/data/adb`, which is `0700 root`.
   */
  | 'module'

/**
 * Which decoder a source's output needs.
 *
 * Part of the source abstraction: a source is fully described by its id, label,
 * command, root requirement and parser. A custom command reuses the parser of
 * the source it belongs to.
 */
export type ParserKind =
  | 'logcatThreadtime'
  | 'kernelDmesg'
  | 'kernelKmsg'
  /** Mixed output: try logcat, then the kernel format, then keep it raw. */
  | 'auto'

/** Normalised severity. `unknown` sorts last so it is never hidden by a min-level rule. */
export type LogLevel =
  | 'verbose'
  | 'debug'
  | 'info'
  | 'warn'
  | 'error'
  | 'fatal'
  | 'unknown'

/** Connection state reported by adb. */
export type DeviceState =
  | 'device'
  | 'offline'
  | 'unauthorized'
  | 'bootloader'
  | 'recovery'
  | 'sideload'
  | 'unknown'

/** How the adb binary was resolved. */
export type AdbSource =
  | 'envOverride'
  | 'bundledResource'
  | 'androidSdk'
  | 'pathSearch'
  | 'knownLocation'

/** logcat ring buffers, mirroring `logcat -b <name>`. */
export type LogcatBuffer =
  | 'main'
  | 'system'
  | 'crash'
  | 'events'
  | 'radio'
  | 'kernel'
  | 'security'
  | 'stats'

/* ------------------------------------------------------------------ device */

/** One attached device. */
export interface DeviceInfo {
  serial: string
  state: DeviceState
  stateRaw: string
  model: string | null
  product: string | null
  device: string | null
  transportId: string | null
  androidVersion: string | null
  sdk: number | null
  rootAvailable: boolean | null
  rootReason: string | null
  /**
   * True when the device booted into recovery or sideload.
   *
   * Recovery has no `logcat` at all, so the collector list and the toolbar badge
   * both change shape for it.
   */
  recovery: boolean
  /**
   * True when the Droidlog Boot Log KernelSU module is installed on the device.
   *
   * The device probe answers this through root (`/data/adb` is root-only), so it stays `false`
   * on a device without root even if the module were present. It gates the module collector's
   * availability and lights the toolbar badge.
   */
  ksuModule: boolean
}

/** Static build metadata. */
export interface AppInfo {
  name: string
  version: string
  platform: string
  arch: string
  debug: boolean
}

/** Result of looking for the adb binary. Never an error — `available` is the signal. */
export interface AdbProbe {
  available: boolean
  program: string | null
  source: AdbSource | null
  version: string | null
  candidates: string[]
  error: string | null
}

/* ------------------------------------------------------------------ source */

/** Static metadata describing a collector. */
export interface LogSourceSpec {
  kind: LogSourceKind
  label: string
  description: string
  requiresRoot: boolean
  defaultCommand: string
  /** Which decoder the output needs. */
  parser: ParserKind
  /** Whether a custom command is honoured for this source. */
  supportsCustomCommand: boolean
  origin: string
}

/** A spec plus whether the selected device can run it. */
export interface SourceAvailability {
  spec: LogSourceSpec
  available: boolean
  unavailableReason: string | null
}

/** Per-session knobs. All fields are optional on the wire. */
export interface SourceOptions {
  pids?: number[]
  /** Restrict to a uid where the source supports it (logcat `--uid=`). */
  uid?: number | null
  tags?: string[]
  buffers?: LogcatBuffer[]
  /** Run this device-side command instead of the source's built-in one. */
  customCommand?: string
  /**
   * Add the events buffer when the device has one.
   *
   * The events buffer is where ActivityManager announces m_crash, m_proc_died and
   * m_kill — the cheapest way to tell a crash source from the processes torn down
   * around it. The backend drops the request on a ROM without that buffer.
   */
  includeEvents?: boolean
}

/* ------------------------------------------------------- application target */

/** What the user typed into the target field. */
export type AppTargetKind = 'pid' | 'uid' | 'package'

/** How a capture session narrowed the stream on the device. */
export type Prefilter = 'uid' | 'pid' | 'none'

/**
 * A resolved application.
 *
 * `pids` is a snapshot: Android hands an app new pids after a restart, so the
 * backend re-resolves it every 30 seconds and emits `droidlog://app-target` when
 * the identity changes.
 */
export interface AppTarget {
  input: string
  kind: AppTargetKind
  serial: string
  mode: ExecMode
  package: string | null
  uid: number | null
  pids: number[]
  /** False when the app is not installed or not running. */
  found: boolean
  /** Why nothing was found, shown verbatim in the panel. */
  reason: string | null
  resolvedAtMs: number
  prefilter: Prefilter
}

/** One entry of the running-applications picker. */
export interface RunningApp {
  package: string
  uid: number | null
  pids: number[]
}

/**
 * One entry of the installed-applications picker.
 *
 * `label` is the display name the *device* resolved (`设置`, `酷玩`). It is empty
 * when the device could not resolve one — `dumpsys package` only exposes a
 * `labelRes` in that case — so the UI falls back to a name derived from the
 * package. `installedAt` is the device's own local wall clock (`YYYY-MM-DD
 * HH:MM:SS`) for `lastUpdateTime`, falling back to `firstInstallTime`; it is
 * passed through as text because the backend has no time-zone database and
 * converting it there would be off by the device's offset.
 */
export interface InstalledApp {
  package: string
  label: string
  uid: number | null
  /** True for a system (pre-installed) application. */
  system: boolean
  versionName: string | null
  installedAt: string | null
  targetSdk: number | null
  enabled: boolean | null
}

/* ------------------------------------------------------------------ record */

/** One decoded log line. */
export interface LogRecord {
  seq: number
  source: LogSourceKind
  level: LogLevel
  timestamp: string | null
  uptimeSeconds: number | null
  pid: number | null
  tid: number | null
  uid: number | null
  tag: string | null
  package: string | null
  message: string
  raw: string
  /** Host wall-clock time this record was decoded (ms since the epoch). */
  receivedAtMs: number
  /**
   * False when the line did not match the parser's grammar and was kept
   * verbatim. Unparsed lines are never dropped.
   */
  parsed: boolean
}

/**
 * A record as held by the render buffer: the decoded line plus the session it
 * arrived from, so the table can attribute rows when several sessions run at
 * once.
 */
export interface LogRow extends LogRecord {
  sessionId: string
}

/* ------------------------------------------------------------------ filter */

/** Which part of a record a rule inspects. */
export type FilterField =
  | 'tag'
  | 'message'
  | 'pid'
  | 'tid'
  | 'uid'
  | 'package'
  | 'level'
  | 'received'
  | 'source'

/** How a rule compares a field against its value. */
export type FilterOp =
  | 'contains'
  | 'notContains'
  | 'equals'
  | 'notEquals'
  | 'regex'
  | 'minLevel'
  | 'in'
  | 'withinLast'

/** A user-authored filter rule. */
export interface FilterRule {
  id: string
  enabled: boolean
  field: FilterField
  op: FilterOp
  value: string
  caseSensitive: boolean
}

/* ----------------------------------------------------------------- process */

/** Lifecycle state of a session. Adjacently tagged on the wire. */
export type SessionStatus =
  | { state: 'starting' }
  | { state: 'running' }
  | { state: 'stopped' }
  | { state: 'failed'; message: string }

/** Immutable description of a session. */
export interface CaptureSession {
  id: string
  serial: string
  mode: ExecMode
  source: LogSourceKind
  command: string
  capacity: number
  startedAtMs: number
  status: SessionStatus
  /** Progress of a bounded collection; `null` for a streaming session. */
  progress: SessionProgress | null
}

/**
 * Progress of a collection that ends by itself.
 *
 * `endsAtMs` is a wall-clock deadline rather than a duration, so the countdown
 * stays correct no matter when the UI re-renders.
 */
export interface SessionProgress {
  label: string
  endsAtMs: number
  done: number
  total: number
}

/** Ring occupancy and lifetime counters. */
export interface RingStats {
  len: number
  capacity: number
  totalPushed: number
  totalDropped: number
}

/** A session plus its live buffer counters. */
export interface SessionSummary {
  session: CaptureSession
  stats: RingStats
}

/** What the frontend asks for when starting a capture. */
export interface CaptureRequest {
  serial: string
  mode: ExecMode
  source: LogSourceKind
  options?: SourceOptions
  capacity?: number
}

/* -------------------------------------------------------- crash collection */

/**
 * Failure families the backend recognises.
 *
 * Classification happens in Rust, on the raw line, and only the family crosses
 * the IPC boundary — the record itself is left untouched.
 */
export type CrashKind =
  | 'kernelPanic'
  | 'oops'
  | 'kernelBug'
  | 'watchdog'
  | 'lowMemory'
  | 'anr'
  | 'systemServer'
  | 'nativeCrash'

/** One recognised crash: which row, and which family. */
export interface CrashEntry {
  seq: number
  kind: CrashKind
}

/** Payload of the `droidlog://crash` event. */
export interface CrashEvent {
  sessionId: string
  entries: CrashEntry[]
}

/** Diagnosis of one collection probe. */
export type ProbeStatus =
  | 'found'
  | 'empty'
  | 'missing'
  | 'denied'
  | 'restricted'
  | 'failed'

/** Result of one probe, as shown in the collection report. */
export interface ProbeOutcome {
  id: string
  label: string
  command: string
  status: ProbeStatus
  records: number
  /** Files read back after a directory probe (tombstones, pstore entries). */
  files: string[]
  detail: string | null
  /** What the user can do about a non-`found` status. */
  hint: string | null
}

/** What a collection run found, probe by probe. */
export interface CollectionReport {
  sessionId: string
  source: LogSourceKind
  outcomes: ProbeOutcome[]
  sourcesFound: number
  records: number
  finishedAtMs: number
  /** Set when nothing at all could be read. */
  failure: string | null
}

/** What the frontend asks for when starting a collection run. */
export interface CollectRequest {
  serial: string
  mode: ExecMode
  capacity?: number
  /** Boot collection: total polling window in milliseconds. */
  durationMs?: number
  /** Boot collection: poll interval in milliseconds. */
  intervalMs?: number
}

/* ------------------------------------------------------------------ export */

/** File formats the export control offers. The extension is fixed by the backend. */
export type ExportFormat = 'log' | 'csv' | 'json'

/** What an export wrote, for the confirmation notice. */
export interface ExportOutcome {
  /** Absolute path of the written file. */
  path: string
  /** Bytes written, including the CSV byte-order mark. */
  bytes: number
  /** Format actually used. */
  format: ExportFormat
}

/* ------------------------------------------------------------------ events */

/** Payload of the `droidlog://records` event. */
export interface RecordsEvent {
  sessionId: string
  records: LogRecord[]
}

/** Payload of the `droidlog://session-status` event. */
export interface SessionStatusEvent {
  sessionId: string
  status: SessionStatus
}

/** Payload of the `droidlog://session-progress` event. */
export interface SessionProgressEvent {
  sessionId: string
  progress: SessionProgress
}

/**
 * Payload of the `droidlog://devices` event.
 *
 * The backend polls `adb devices -l` every 2 seconds and emits this only when
 * something changed, so it is the authoritative device list once the app is up.
 */
export interface DevicesEvent {
  adbAvailable: boolean
  adbProgram: string | null
  adbSource: AdbSource | null
  adbVersion: string | null
  devices: DeviceInfo[]
  error: string | null
}

/** Event names emitted by the backend. */
export const BACKEND_EVENTS = {
  records: 'droidlog://records',
  sessionStatus: 'droidlog://session-status',
  sessionProgress: 'droidlog://session-progress',
  devices: 'droidlog://devices',
  appTarget: 'droidlog://app-target',
  crash: 'droidlog://crash',
  collectReport: 'droidlog://collect-report',
} as const

/* ------------------------------------------------------------------- error */

/** The structured error every command rejects with. */
export interface BackendError {
  kind: string
  message: string
  detail: string | null
  /**
   * True for environmental failures (no adb, no device, root refused) rather
   * than bugs — the UI shows a hint instead of a failure banner.
   */
  environmental: boolean
}

/* ------------------------------------------------------- crash forensics */

/** Where a structured crash came from (mirrors `crash::structured::CrashOrigin`). */
export type CrashOrigin = 'crashBuffer' | 'dropbox' | 'tombstone' | 'anrTrace' | 'kernel'

/** One crash, as far as it could be understood (mirrors `CrashEvent`). */
export interface CrashEvent {
  id: string
  origin: CrashOrigin
  kind: CrashKind | null
  process: string | null
  pid: number | null
  tid: number | null
  thread: string | null
  exception: string | null
  message: string | null
  /** Stack frames, verbatim, in device order. */
  frames: string[]
  /** `Caused by:` lines — the root cause of a Java crash. */
  causedBy: string[]
  timestamp: string | null
  /** The source lines, verbatim. Never empty. */
  raw: string[]
}

/** What an ActivityManager signal says happened (mirrors `AmsSignalKind`). */
export type AmsSignalKind =
  | 'amCrash'
  | 'amAnr'
  | 'amProcDied'
  | 'amKill'
  | 'amProcStart'
  | 'forceFinish'
  | 'forceStop'
  | 'killing'
  | 'processDied'
  | 'isCrashing'
  | 'anrIn'
  | 'restart'
  | 'processRecord'

/** One parsed ActivityManager signal. */
export interface AmsSignal {
  kind: AmsSignalKind
  process: string | null
  pid: number | null
  uid: number | null
  reason: string | null
  crashing: boolean | null
  detail: string | null
  /** Index of the source line, so the timeline can jump to the row. */
  lineIndex: number
  raw: string
}

/** Which resource ran out (mirrors `ResourceKind`). */
export type ResourceKind =
  | 'lowMemoryKill'
  | 'outOfMemory'
  | 'fdExhaustion'
  | 'threadExhaustion'
  | 'binderFailure'

/** One recognised resource anomaly. */
export interface ResourceAnomaly {
  kind: ResourceKind
  severity: 'warning' | 'critical'
  process: string | null
  pid: number | null
  detail: string
  lineIndex: number
  raw: string
}

/**
 * Something the live watch decided was worth reporting.
 *
 * The payload is internally tagged (`type`) to match the Rust enum's serde shape, so a
 * consumer switches on `payload.type` and the remaining fields belong to that event.
 */
export type LiveEvent = {
  /** Stable id: crash blocks keep it while they grow, so a consumer can replace. */
  id: string
  /** False while a crash block is still receiving lines. */
  complete: boolean
  /** Capture-local sequence number of the line this came from. */
  lineIndex: number
} & (
  | { payload: { type: 'crash' } & CrashEvent }
  | { payload: { type: 'ams' } & AmsSignal }
  | { payload: { type: 'anomaly' } & ResourceAnomaly }
)

/** One integrity finding (mirrors `IntegrityCheck`). */
export interface IntegrityCheck {
  id: string
  label: string
  status: 'ok' | 'notice' | 'warning'
  detail: string
}
/* ----------------------------------------------- crash forensics analysis */

/**
 * A `source → victim` link: the process whose crash is believed to have taken the
 * victim down with it (mirrors `crash::ams::CausalLink`).
 *
 * `evidence` is the analyser's own record of where the link came from: a list of **line
 * indices into the text it was handed**, verified against `CausalLink.evidence: Vec<usize>`
 * in `crash/ams.rs`. Those are not capture sequence numbers — translate through
 * `ForensicsView.seqAt` before jumping, or the row will be wrong.
 */
export interface CausalLink {
  source: string
  sourcePid: number | null
  victim: string
  victimPid: number | null
  /** Why the analyser linked them, in words — this is what the view shows. */
  reason: string
  /** Line indices of the lines that produced the link, in the analyser's numbering. */
  evidence: number[]
  confidence: number
}

/**
 * One process's story across pid changes (the fields the view uses).
 *
 * `crashes` is `unknown[]` rather than `CrashEvent[]`: the view only counts them, and the
 * structured crashes the timeline renders come from the live watch, which is typed above.
 */
export interface CrashStory {
  identity: string
  uid: number | null
  crashes: unknown[]
  /**
   * The links that took other processes down around this identity.
   *
   * These are `CausalLink`s, not names — verified against `correlate::CrashStory.victims`,
   * which a test caught when an assertion compared them to strings. Rendering them as names
   * printed `[object Object]`; the name lives in `victim.victim`, and the link additionally
   * carries the reason and the evidence.
   */
  victims: CausalLink[]
}

/** The analysis the forensics view renders. */
export interface Forensics {
  links: CausalLink[]
  anomalies: ResourceAnomaly[]
  correlation: {
    stories: CrashStory[]
    unlinkedEvents: number
    unlinkedLinks: number
  }
  checks: IntegrityCheck[]
  /** Crashes that matched a known signature, with a cause instead of a stack. */
  known: KnownNote[]
}

/** A crash that matched a known signature (mirrors `crash::known::KnownNote`). */
export interface KnownNote {
  /** The crash this describes — the same id the timeline uses. */
  eventId: string
  /** What it is, in a few words. */
  title: string
  /** The root cause. */
  cause: string
  /** What to do about it. */
  action: string
}

/**
 * [`Forensics`] plus the map from analyser line index to capture sequence number.
 *
 * The analysers number the lines they were handed from zero; the table jumps by capture
 * sequence. Every jump from the forensics view goes through `seqAt`, otherwise it lands
 * on the wrong row.
 */
export interface ForensicsView {
  analysis: Forensics
  seqAt: number[]
  /**
   * A capture-level remark, or `null`.
   *
   * Today: whether the rows carry more than one date. The crash buffer is a ring, so a
   * capture can legitimately include older crashes — which changes what the timeline is
   * describing, and must be said rather than left for the reader to notice.
   */
  notice: string | null
}

/**
 * The remembered timeline of the latest capture (mirrors `crash::store::TimelineSnapshot`).
 *
 * Written by `save_timeline_snapshot` when the view loads a capture, and read on startup so
 * reopening the app does not throw away what the user was just looking at. Only the *latest*
 * capture is kept, in one file, on purpose: an archive would need retention rules nobody asked
 * for. The log rows themselves are not stored — they live in the ring buffer.
 */
export interface TimelineSnapshot {
  /** Schema version; a file from another version is ignored rather than half-read. */
  version: number
  sessionId: string
  /** Host wall-clock time it was written, ms since the Unix epoch. */
  savedAtMs: number
  events: LiveEvent[]
  view: ForensicsView
}