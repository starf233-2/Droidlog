/**
 * Typed wrappers around the Tauri command boundary.
 *
 * Two rules hold throughout:
 *   * every command is wrapped in exactly one function with an explicit return
 *     type, so no `invoke` string is ever written twice;
 *   * failures are normalised into {@link BackendError} before they reach the
 *     store, because `invoke` rejects with a plain serialised object, not an
 *     `Error` instance.
 */

import { invoke } from '@tauri-apps/api/core'

import type {
  AdbProbe,
  AppInfo,
  AppTarget,
  BackendError,
  CaptureRequest,
  CaptureSession,
  CollectRequest,
  CollectionReport,
  DeviceInfo,
  ExecMode,
  ExportFormat,
  ExportOutcome,
  FilterRule,
  LogRecord,
  LogSourceKind,
  RunningApp,
  SessionSummary,
  SourceAvailability,
} from '../types'

export type { BackendError }

/* ------------------------------------------------------------------ errors */

/**
 * Narrows an unknown rejection value into a {@link BackendError}.
 *
 * `invoke` rejects with whatever the command's error type serialised to, which
 * for this backend is always that payload shape — but nothing guarantees it at
 * the type level, so this narrows defensively rather than casting.
 */
export function toBackendError(error: unknown): BackendError {
  if (typeof error === 'object' && error !== null) {
    const candidate = error as Record<string, unknown>
    const kind = candidate['kind']
    const message = candidate['message']
    const detail = candidate['detail']
    return {
      kind: typeof kind === 'string' ? kind : 'unknown',
      message:
        typeof message === 'string' ? message : '命令执行失败，且未返回可读信息',
      detail: typeof detail === 'string' ? detail : null,
      environmental: candidate['environmental'] === true,
    }
  }

  return {
    kind: 'unknown',
    message: typeof error === 'string' ? error : '未知错误',
    detail: null,
    environmental: false,
  }
}

/* ---------------------------------------------------------------- commands */

/** Build metadata for the running application. */
export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>('app_info')
}

/** Locates adb and queries its version. Never rejects for a missing binary. */
export function probeAdb(): Promise<AdbProbe> {
  return invoke<AdbProbe>('probe_adb')
}

/**
 * Lists attached devices.
 *
 * @param probe also read the Android version and root capability — two extra
 *              adb round trips per device, so it is opt-in.
 */
export function listDevices(probe = false): Promise<DeviceInfo[]> {
  return invoke<DeviceInfo[]>('list_devices', { probe })
}

/**
 * Asks the backend's device poller to re-probe immediately.
 *
 * Fire-and-forget: the refreshed list arrives as a `droidlog://devices` event,
 * which keeps the poller the single writer of device state.
 */
export function refreshDevices(): Promise<boolean> {
  return invoke<boolean>('refresh_devices')
}

/**
 * Lists collectors with availability for the given privilege level.
 *
 * `recovery` is not optional: the recovery collector only exists while the
 * device is booted into recovery, and the backend rejects a call that leaves the
 * question unanswered (which is how the rail ended up permanently empty once).
 */
export function listSources(
  rootAvailable: boolean,
  recovery: boolean,
): Promise<SourceAvailability[]> {
  return invoke<SourceAvailability[]>('list_sources', {
    rootAvailable,
    recovery,
  })
}

/** Returns the active filter rules. */
export function getFilters(): Promise<FilterRule[]> {
  return invoke<FilterRule[]>('get_filters')
}

/** Replaces the active filter rules; rejects when a rule is invalid. */
export function setFilters(rules: FilterRule[]): Promise<void> {
  return invoke<void>('set_filters', { rules })
}

/** Starts a capture session. */
export function startCapture(
  request: CaptureRequest,
): Promise<CaptureSession> {
  return invoke<CaptureSession>('start_capture', { request })
}

/** Stops a capture session. */
export function stopCapture(sessionId: string): Promise<void> {
  return invoke<void>('stop_capture', { sessionId })
}

/** Stops every running session in one call; resolves to how many were stopped. */
export function stopAllCaptures(): Promise<number> {
  return invoke<number>('stop_all_captures')
}

/**
 * Starts a collection run: `crash`, `boot` or `recovery`.
 *
 * All three share one request shape and resolve as soon as the session exists —
 * the probes report through events, and the outcome arrives as
 * `droidlog://collect-report`.
 */
export function collect(
  source: Extract<LogSourceKind, 'crash' | 'boot' | 'recovery'>,
  request: CollectRequest,
): Promise<CaptureSession> {
  const command =
    source === 'crash'
      ? 'collect_crash'
      : source === 'boot'
        ? 'collect_boot'
        : 'collect_recovery'
  return invoke<CaptureSession>(command, { request })
}

/**
 * The report of a finished collection run.
 *
 * Asked for explicitly rather than only listened for, so a window that was
 * reloaded mid-collection still shows the diagnosis.
 */
export function getCollectReport(
  sessionId: string,
): Promise<CollectionReport | null> {
  return invoke<CollectionReport | null>('get_collect_report', { sessionId })
}

/**
 * Writes already-formatted rows to a file under the user's downloads folder.
 *
 * The text is built by the caller, which holds the rows and knows what is on
 * screen; the backend decides the destination, sanitises the name and adds the
 * CSV byte-order mark.
 */
export function exportRecords(payload: {
  content: string
  format: ExportFormat
  fileName: string
}): Promise<ExportOutcome> {
  return invoke<ExportOutcome>('export_records', payload)
}

/** Opens the file manager with an exported file selected. */
export function revealExport(path: string): Promise<void> {
  return invoke<void>('reveal_export', { path })
}

/** Lists all known sessions with their buffer counters. */
export function listSessions(): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>('list_sessions')
}

/** Reads buffered records, keeping the newest `limit` entries. */
export function drainRecords(
  sessionId: string,
  limit?: number,
): Promise<LogRecord[]> {
  // Send the key only when set: the Rust side models it as Option<usize>, and an
  // explicit `undefined` would be a payload detail rather than a real argument.
  return limit === undefined
    ? invoke<LogRecord[]>('drain_records', { sessionId })
    : invoke<LogRecord[]>('drain_records', { sessionId, limit })
}

/* ------------------------------------------------------- application target */

/**
 * Resolves a PID, package name or UID into a full application identity.
 *
 * `serial` and `mode` are passed explicitly: the selected device and execution
 * mode live in this UI, and the backend deliberately keeps no second copy of
 * them that could disagree.
 *
 * @param force bypass the backend's 30-second resolution cache.
 */
export function resolveApp(
  serial: string,
  mode: ExecMode,
  input: string,
  force = false,
): Promise<AppTarget> {
  return invoke<AppTarget>('resolve_app', { serial, mode, input, force })
}

/** Sets, or clears with a blank `input`, the application being followed. */
export function setAppTarget(
  serial: string,
  mode: ExecMode,
  input: string,
): Promise<AppTarget | null> {
  return invoke<AppTarget | null>('set_app_target', { serial, mode, input })
}

/** Returns the application currently being followed. */
export function getAppTarget(): Promise<AppTarget | null> {
  return invoke<AppTarget | null>('get_app_target')
}

/** Lists the running applications, for picking one without typing. */
export function listRunningApps(
  serial: string,
  mode: ExecMode,
): Promise<RunningApp[]> {
  return invoke<RunningApp[]>('list_running_apps', { serial, mode })
}
