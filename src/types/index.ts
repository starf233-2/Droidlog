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
