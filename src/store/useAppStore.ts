/**
 * Application store.
 *
 * Owns everything the three-column shell renders: adb availability, the device
 * list, the selected collector and mode, the live record buffer, sessions and
 * filter rules.
 *
 * Device state has a single writer: the backend's 2-second poller. It emits
 * `droidlog://devices` on every change, and {@link AppStore.applyDevices} folds
 * that into the store. The `扫描` button does not query adb itself — it pokes the
 * poller, so a manual refresh and an automatic one can never disagree or race.
 *
 * Notes on the hot path:
 *   * records arrive in batches over a Tauri event and are appended in one
 *     `set` call per batch, not one per row;
 *   * the buffer is capped at {@link MAX_RECORDS}; the backend ring is the
 *     durable copy, the store is only the render window.
 */

import { create } from 'zustand'

import * as api from '../api/backend'
import { COLOR_THEMES, applyColorScheme } from '../theme'
import type { ColorThemeId } from '../theme'
import type {
  AdbProbe,
  AppTarget,
  BackendError,
  CaptureSession,
  CollectionReport,
  CrashEntry,
  CrashKind,
  DeviceInfo,
  DevicesEvent,
  ExecMode,
  FilterRule,
  LogLevel,
  LogRecord,
  LogRow,
  LogSourceKind,
  RunningApp,
  SessionProgress,
  SessionStatus,
  SourceAvailability,
} from '../types'

/* -------------------------------------------------------------------------- */
/* Structured filters                                                         */
/* -------------------------------------------------------------------------- */

/**
 * The filter panel's own state, compiled into the backend's generic rule list.
 *
 * Keeping the panel's fields here and the engine generic means the backend never
 * grows a concept per control, while the UI still presents one control per
 * concept instead of a raw rule table.
 */
export interface StructuredFilters {
  /** Levels to keep. Empty means all. */
  levels: LogLevel[]
  /** Substring a tag must contain. */
  tagInclude: string
  /** Substring a tag must not contain. */
  tagExclude: string
  /** Message keyword. */
  keyword: string
  /** Treat {@link keyword} as a regular expression. */
  keywordIsRegex: boolean
  /** Rolling "last N seconds" window; `null` disables it. */
  windowSeconds: number | null
}

/** Default structured filters: show everything. */
export const EMPTY_STRUCTURED_FILTERS: StructuredFilters = {
  levels: [],
  tagInclude: '',
  tagExclude: '',
  keyword: '',
  keywordIsRegex: false,
  windowSeconds: null,
}

/**
 * Rule ids reserved for the structured controls.
 *
 * The prefix keeps them out of the way of user-authored rules and lets the panel
 * round-trip its own state through the backend, which is the single source of
 * truth for what is actually being filtered.
 */
const RESERVED = {
  levels: '__levels',
  tagInclude: '__tag-include',
  tagExclude: '__tag-exclude',
  keyword: '__keyword',
  window: '__window',
} as const

/** Builds the rule list the backend should apply. */
export function compileStructuredFilters(
  structured: StructuredFilters,
): FilterRule[] {
  const rules: FilterRule[] = []

  if (structured.levels.length > 0) {
    rules.push({
      id: RESERVED.levels,
      enabled: true,
      field: 'level',
      op: 'in',
      value: structured.levels.join(','),
      caseSensitive: false,
    })
  }

  const tagInclude = structured.tagInclude.trim()
  if (tagInclude.length > 0) {
    rules.push({
      id: RESERVED.tagInclude,
      enabled: true,
      field: 'tag',
      op: 'contains',
      value: tagInclude,
      caseSensitive: false,
    })
  }

  const tagExclude = structured.tagExclude.trim()
  if (tagExclude.length > 0) {
    rules.push({
      id: RESERVED.tagExclude,
      enabled: true,
      field: 'tag',
      op: 'notContains',
      value: tagExclude,
      caseSensitive: false,
    })
  }

  const keyword = structured.keyword.trim()
  if (keyword.length > 0) {
    rules.push({
      id: RESERVED.keyword,
      enabled: true,
      field: 'message',
      op: structured.keywordIsRegex ? 'regex' : 'contains',
      value: keyword,
      caseSensitive: false,
    })
  }

  if (structured.windowSeconds !== null && structured.windowSeconds > 0) {
    rules.push({
      id: RESERVED.window,
      enabled: true,
      field: 'received',
      op: 'withinLast',
      value: String(structured.windowSeconds),
      caseSensitive: false,
    })
  }

  return rules
}

/**
 * Rows kept in the render window. Matches the backend ring's default capacity,
 * so the table can hold everything a session retains.
 *
 * This is only affordable because the table virtualises: rendering every row
 * here would pin the WebView renderer (measured at 1.87 GB and minutes of CPU
 * with a 5 000-row buffer), which is what made buttons appear dead.
 */
export const MAX_RECORDS = 100_000

/** Colour scheme choice. `system` follows the OS preference. */
export type ThemeChoice = 'light' | 'dark' | 'system'

/** Storage keys for the two independent theme choices. */
const THEME_KEY = 'droidlog.theme'
const COLOR_THEME_KEY = 'droidlog.color-theme'

/** A stored value, or null when there is nothing usable to restore. */
function readStored(key: string): string | null {
  try {
    return window.localStorage.getItem(key)
  } catch {
    // A webview with storage disabled must not take the app down with it.
    return null
  }
}

function writeStored(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value)
  } catch {
    // Persistence is a convenience; failing to write is not worth reporting.
  }
}

/** The theme choice to start from: the stored one, else follow the system. */
function initialThemeChoice(): ThemeChoice {
  const stored = readStored(THEME_KEY)
  return stored === 'light' || stored === 'dark' || stored === 'system'
    ? stored
    : 'system'
}

/** The colour theme to start from, validated against the built-in list. */
function initialColorTheme(): ColorThemeId {
  const stored = readStored(COLOR_THEME_KEY)
  return COLOR_THEMES.some((theme) => theme.id === stored)
    ? (stored as ColorThemeId)
    : 'slate'
}

/* -------------------------------------------------------------------------- */
/* Frame-coalesced record intake                                              */
/* -------------------------------------------------------------------------- */

/**
 * Batches waiting to be merged into the store.
 *
 * Records are appended at most once per animation frame rather than once per
 * incoming event. A log storm can deliver several batches per frame; folding
 * them together keeps re-renders bounded by the display's refresh rate instead
 * of by the device's log rate.
 */
const pendingRows: LogRow[] = []
let frameScheduled = false

function flushPendingRows(): void {
  frameScheduled = false
  if (pendingRows.length === 0) {
    return
  }
  const incoming = pendingRows.splice(0, pendingRows.length)
  useAppStore.setState((state) => {
    const combined = state.records.concat(incoming)
    const overflow = Math.max(0, combined.length - MAX_RECORDS)
    return {
      records: overflow === 0 ? combined : combined.slice(overflow),
      recordCount: state.recordCount + incoming.length,
      droppedCount: state.droppedCount + overflow,
    }
  })
}

function enqueueRows(rows: LogRow[]): void {
  if (rows.length === 0) {
    return
  }
  for (const row of rows) {
    pendingRows.push(row)
  }
  if (!frameScheduled) {
    frameScheduled = true
    // `requestAnimationFrame` is not available in a worker; the renderer always
    // has it, and a missing one would only cost latency, not correctness.
    if (typeof requestAnimationFrame === 'function') {
      requestAnimationFrame(flushPendingRows)
    } else {
      setTimeout(flushPendingRows, 16)
    }
  }
}

/** Drops anything queued but not yet rendered (used by clear). */
function discardPendingRows(): void {
  pendingRows.length = 0
}

interface AppStore {
  /* ------------------------------------------------------------- chrome */
  theme: ThemeChoice
  setTheme: (theme: ThemeChoice) => void
  /** Which colour theme the M3 tokens are generated from. */
  colorTheme: ColorThemeId
  setColorTheme: (theme: ColorThemeId) => void

  /* --------------------------------------------------------------- adb */
  adb: AdbProbe | null
  adbLoading: boolean

  /* ------------------------------------------------------------ devices */
  devices: DeviceInfo[]
  devicesLoading: boolean
  /** Set when the poller could not reach adb, so the rail can explain why. */
  devicesError: string | null
  selectedSerial: string | null
  selectedDevice: () => DeviceInfo | null

  /* ------------------------------------------------------------ capture */
  mode: ExecMode
  sources: SourceAvailability[]
  selectedSource: LogSourceKind
  /** Device-side command override for the next session; empty means built-in. */
  customCommand: string
  setCustomCommand: (command: string) => void
  /** Every running session, so several devices/sources can run in parallel. */
  sessions: CaptureSession[]
  runningCount: () => number
  records: LogRow[]
  recordCount: number
  droppedCount: number

  /**
   * Every crash the backend recognised, oldest first.
   *
   * Kept beside the records rather than inside them: `LogRecord` is the
   * decoder's output and stays exactly as the device wrote it, so classification
   * cannot corrupt what gets exported.
   */
  crashes: CrashEntry[]
  crashCount: () => number
  /** The crash families of the rows currently shown, keyed by `seq`. */
  crashKinds: () => Map<number, CrashKind>
  applyCrashes: (sessionId: string, entries: CrashEntry[]) => void
  /** Show only rows that are part of a recognised crash. */
  crashOnly: boolean
  setCrashOnly: (value: boolean) => void
  /** The `seq` the table should scroll to, or `null`. */
  jumpToSeq: number | null
  requestJump: (seq: number) => void
  clearJump: () => void
  /** Moves to the next crash after the given row, wrapping around. */
  jumpToNextCrash: (from: number) => void

  /** Collection reports, keyed by session id; they outlive their session. */
  reports: CollectionReport[]
  applyCollectReport: (report: CollectionReport) => void
  /** The report the analysis panel is showing. */
  activeReportId: string | null
  selectReport: (sessionId: string | null) => void
  activeReport: () => CollectionReport | null
  /**
   * Runs a crash, boot or recovery collection.
   *
   * Boot collection is bounded by the backend: it polls until the device
   * finishes booting or the window ends, so the UI needs no stopwatch of its
   * own.
   */
  collect: (source: 'crash' | 'boot' | 'recovery') => Promise<void>

  /* ------------------------------------------------------------ filters */
  filters: FilterRule[]
  filtersError: BackendError | null
  /** The filter panel's structured controls. */
  structured: StructuredFilters
  /** Updates structured controls and pushes the recompiled rules. */
  setStructured: (patch: Partial<StructuredFilters>) => Promise<void>
  /** Rules the user authored directly, i.e. everything not reserved. */
  advancedFilters: () => FilterRule[]

  /* --------------------------------------------------------- app target */
  /** The application being followed, when one is set. */
  appTarget: AppTarget | null
  /** Raw text in the target box. */
  appTargetInput: string
  setAppTargetInput: (value: string) => void
  /** Resolves the input and starts following it. */
  resolveAppTarget: () => Promise<void>
  /** Stops following the current application. */
  clearAppTarget: () => Promise<void>
  /** Applies a target pushed by the backend's 30-second refresh. */
  applyAppTarget: (target: AppTarget | null) => void
  /** Running applications, for the picker. */
  runningApps: RunningApp[]
  refreshRunningApps: () => Promise<void>

  /* -------------------------------------------------------------- status */
  error: BackendError | null
  notice: string | null

  /* ------------------------------------------------------------- actions */
  bootstrap: () => Promise<void>
  refreshAdb: () => Promise<void>
  loadDevicesOnce: () => Promise<void>
  /** Pokes the backend poller; the result arrives via {@link applyDevices}. */
  refreshDevices: () => Promise<void>
  /** Folds a `droidlog://devices` event into the store. */
  applyDevices: (event: DevicesEvent) => void
  selectDevice: (serial: string | null) => void
  setMode: (mode: ExecMode) => void
  selectSource: (kind: LogSourceKind) => void
  startCapture: () => Promise<void>
  stopSession: (sessionId: string) => Promise<void>
  stopAllSessions: () => Promise<void>
  clearRecords: () => void
  appendRecords: (sessionId: string, rows: LogRecord[]) => void
  applySessionStatus: (sessionId: string, status: SessionStatus) => void
  /** Folds a progress tick in, which is what drives the countdown. */
  applySessionProgress: (
    sessionId: string,
    progress: SessionProgress,
  ) => void
  addFilter: (rule: FilterRule) => Promise<void>
  updateFilter: (id: string, patch: Partial<FilterRule>) => Promise<void>
  removeFilter: (id: string) => Promise<void>
  clearFilters: () => Promise<void>
  dismissError: () => void
  dismissNotice: () => void
}

/** Resolves the effective colour scheme for a choice. */
export function resolveTheme(choice: ThemeChoice): 'light' | 'dark' {
  if (choice !== 'system') {
    return choice
  }
  return window.matchMedia('(prefers-color-scheme: dark)').matches
    ? 'dark'
    : 'light'
}

/** Applies a theme choice plus the active colour theme to the document. */
function applyTheme(choice: ThemeChoice, colorTheme: ColorThemeId): void {
  applyColorScheme(colorTheme, resolveTheme(choice))
}

/**
 * Applies the stored theme to the document, returning the resolved scheme.
 *
 * Exported so `main.tsx` can paint the correct surface *before* React's first
 * render — otherwise the first frame is the webview default.
 */
export function applyStoredTheme(): 'light' | 'dark' {
  const { theme, colorTheme } = useAppStore.getState()
  const resolved = resolveTheme(theme)
  applyColorScheme(colorTheme, resolved)
  return resolved
}

const messagesOf = (error: unknown): BackendError => api.toBackendError(error)

export const useAppStore = create<AppStore>((set, get) => ({
  /* ------------------------------------------------------------- chrome */
  theme: initialThemeChoice(),
  colorTheme: initialColorTheme(),
  setTheme: (theme) => {
    applyTheme(theme, get().colorTheme)
    writeStored(THEME_KEY, theme)
    set({ theme })
  },
  setColorTheme: (colorTheme) => {
    applyTheme(get().theme, colorTheme)
    writeStored(COLOR_THEME_KEY, colorTheme)
    set({ colorTheme })
  },

  /* --------------------------------------------------------------- adb */
  adb: null,
  adbLoading: false,

  /* ------------------------------------------------------------ devices */
  devices: [],
  devicesLoading: false,
  devicesError: null,
  selectedSerial: null,
  selectedDevice: () => {
    const { devices, selectedSerial } = get()
    return devices.find((device) => device.serial === selectedSerial) ?? null
  },

  /* ------------------------------------------------------------ capture */
  mode: 'adb',
  sources: [],
  selectedSource: 'logcat',
  customCommand: '',
  setCustomCommand: (command) => set({ customCommand: command }),
  sessions: [],
  runningCount: () =>
    get().sessions.filter((session) => session.status.state === 'running').length,
  records: [],
  recordCount: 0,
  droppedCount: 0,
  crashes: [],
  crashOnly: false,
  jumpToSeq: null,
  reports: [],
  activeReportId: null,

  /* ------------------------------------------------------------ filters */
  filters: [],
  filtersError: null,
  structured: EMPTY_STRUCTURED_FILTERS,
  advancedFilters: () => {
    const reserved: string[] = Object.values(RESERVED)
    return get().filters.filter((rule) => !reserved.includes(rule.id))
  },

  /* --------------------------------------------------------- app target */
  appTarget: null,
  appTargetInput: '',
  runningApps: [],

  /* -------------------------------------------------------------- status */
  error: null,
  notice: null,

  /* ------------------------------------------------------------- actions */

  /** Loads adb status, an initial device list, and the saved filters. */
  bootstrap: async () => {
    applyTheme(get().theme, get().colorTheme)
    await get().refreshAdb()
    await get().loadDevicesOnce()
    try {
      const filters = await api.getFilters()
      set({ filters })
    } catch (error) {
      set({ filtersError: messagesOf(error) })
    }
  },

  refreshAdb: async () => {
    set({ adbLoading: true })
    try {
      const adb = await api.probeAdb()
      set({ adb, adbLoading: false })
    } catch (error) {
      // `probe_adb` reports absence in its payload rather than rejecting, so a
      // rejection here is a genuine transport problem.
      set({ adbLoading: false, error: messagesOf(error) })
    }
  },

  /**
   * One unprompted fetch so the rail is populated immediately, without waiting
   * up to two seconds for the poller's first change event.
   */
  loadDevicesOnce: async () => {
    set({ devicesLoading: true })
    try {
      const devices = await api.listDevices(false)
      set({ devicesLoading: false })
      get().applyDevices({
        adbAvailable: true,
        adbProgram: null,
        adbSource: null,
        adbVersion: null,
        devices,
        error: null,
      })
    } catch (error) {
      const backendError = messagesOf(error)
      set({
        devicesLoading: false,
        devices: [],
        selectedSerial: null,
        // A missing adb is an empty state, not a failure banner.
        error: backendError.environmental ? null : backendError,
        notice: backendError.environmental ? backendError.message : null,
      })
      set({ sources: [] })
    }
  },

  refreshDevices: async () => {
    set({ devicesLoading: true })
    try {
      await api.refreshDevices()
    } catch (error) {
      set({ devicesLoading: false, error: messagesOf(error) })
    }
    // `devicesLoading` is cleared by the resulting `droidlog://devices` event.
  },

  applyDevices: (event) => {
    const { selectedSerial, mode, selectedSource } = get()
    const devices = event.devices

    const stillPresent = devices.some(
      (device) => device.serial === selectedSerial,
    )
    // Fall back to the first *usable* device: an `offline` or `unauthorized`
    // entry cannot be captured from.
    const nextSerial = stillPresent
      ? selectedSerial
      : (devices.find((device) => device.state === 'device')?.serial ?? null)
    const selected = devices.find((device) => device.serial === nextSerial) ?? null

    // Root availability decides whether dmesg/kmsg are offered and whether Root
    // mode is selectable, so a device swap can force the mode back to Adb.
    const nextMode: ExecMode =
      mode === 'root' && selected?.rootAvailable === false ? 'adb' : mode

    set({
      devices,
      devicesLoading: false,
      devicesError: event.error,
      selectedSerial: nextSerial,
      mode: nextMode,
    })

    // If adb availability changed (installed, removed, or first seen), re-read
    // the detailed probe so the rail stops showing a stale "未找到 adb" hint.
    const currentAdb = get().adb
    if (currentAdb === null || currentAdb.available !== event.adbAvailable) {
      void get().refreshAdb()
    }

    void refreshSources(set, nextMode, selectedSource, selected?.recovery === true)
  },

  selectDevice: (serial) => {
    set({ selectedSerial: serial })
    void refreshSources(
      set,
      get().mode,
      get().selectedSource,
      get().selectedDevice()?.recovery === true,
    )
  },

  setMode: (mode) => {
    set({ mode })
    void refreshSources(
      set,
      mode,
      get().selectedSource,
      get().selectedDevice()?.recovery === true,
    )
  },

  /**
   * Selects a collector.
   *
   * Root-gated collectors are only offered in Root mode: the rail greys them out
   * and refuses the click otherwise, so there is nothing to guard against here.
   */
  selectSource: (kind) => set({ selectedSource: kind }),

  /**
   * Starts a session for the current device + collector + mode.
   *
   * Several sessions may run at once (multiple devices and/or several collectors
   * on one device), so this does **not** clear the record buffer: stopping one
   * source must not wipe what another is still producing. Duplicates of an
   * already-running combination are refused instead of silently doubled.
   */
  startCapture: async () => {
    const {
      selectedSerial,
      selectedSource,
      mode,
      sessions,
      customCommand,
    } = get()
    if (selectedSerial === null) {
      set({ notice: '请先选择一个设备' })
      return
    }

    const duplicate = sessions.some(
      (session) =>
        session.status.state === 'running' &&
        session.serial === selectedSerial &&
        session.source === selectedSource &&
        session.mode === mode,
    )
    if (duplicate) {
      set({ notice: '该组合已在采集中（同一设备 + 同一采集源 + 同一模式）' })
      return
    }

    const trimmed = customCommand.trim()
    try {
      const started = await api.startCapture({
        serial: selectedSerial,
        mode,
        source: selectedSource,
        ...(trimmed.length > 0 ? { options: { customCommand: trimmed } } : {}),
      })
      set({
        sessions: get().sessions.concat(started),
        error: null,
        notice: `已开始采集：${started.command}`,
      })
    } catch (error) {
      const backendError = messagesOf(error)
      set({
        error: backendError.environmental ? null : backendError,
        notice: backendError.environmental ? backendError.message : null,
      })
    }
  },

  /** Stops one session, leaving every other session running. */
  stopSession: async (sessionId) => {
    try {
      await api.stopCapture(sessionId)
    } catch (error) {
      const backendError = messagesOf(error)
      // Losing the race with a natural exit is not worth reporting.
      if (backendError.kind !== 'sessionNotFound') {
        set({ error: backendError })
      }
    } finally {
      // Remove locally as well: the backend's terminal status event may be
      // coalesced or arrive after the UI has already moved on.
      set({
        sessions: get().sessions.filter((session) => session.id !== sessionId),
      })
    }
  },

  /** Stops everything in one backend call. */
  stopAllSessions: async () => {
    if (get().sessions.length === 0) {
      return
    }
    try {
      await api.stopAllCaptures()
    } catch (error) {
      set({ error: messagesOf(error) })
    } finally {
      set({ sessions: [] })
    }
  },

  clearRecords: () => {
    discardPendingRows()
    set({
      records: [],
      recordCount: 0,
      droppedCount: 0,
      crashes: [],
      jumpToSeq: null,
    })
  },

  crashCount: () => get().crashes.length,

  crashKinds: () => {
    const kinds = new Map<number, CrashKind>()
    for (const entry of get().crashes) {
      kinds.set(entry.seq, entry.kind)
    }
    return kinds
  },

  /**
   * Folds a batch of classified crashes in.
   *
   * A crash is announced before its row is necessarily rendered — the record and
   * the classification travel in separate events — so the table looks the family
   * up by `seq` and simply shows nothing for rows it has not received yet.
   */
  applyCrashes: (_sessionId, entries) => {
    if (entries.length === 0) {
      return
    }
    const known = new Set(get().crashes.map((entry) => entry.seq))
    const fresh = entries.filter((entry) => !known.has(entry.seq))
    if (fresh.length === 0) {
      return
    }
    set({ crashes: get().crashes.concat(fresh) })
  },

  setCrashOnly: (crashOnly) => set({ crashOnly }),

  requestJump: (seq) => set({ jumpToSeq: seq }),

  clearJump: () => set({ jumpToSeq: null }),

  jumpToNextCrash: (from) => {
    const seqs = get()
      .crashes.map((entry) => entry.seq)
      .sort((left, right) => left - right)
    if (seqs.length === 0) {
      set({ notice: '还没有识别到崩溃日志' })
      return
    }
    const next = seqs.find((seq) => seq > from) ?? seqs[0]
    if (next !== undefined) {
      set({ jumpToSeq: next, crashOnly: false })
    }
  },

  applyCollectReport: (report) => {
    const reports = get().reports.filter(
      (existing) => existing.sessionId !== report.sessionId,
    )
    reports.push(report)
    const failure = report.failure
    set({
      reports,
      activeReportId: report.sessionId,
      notice:
        failure !== null
          ? failure
          : `找到 ${report.sourcesFound} 个日志源，共 ${report.records} 条记录`,
    })
  },

  selectReport: (activeReportId) => set({ activeReportId }),

  activeReport: () => {
    const { reports, activeReportId } = get()
    if (activeReportId === null) {
      return null
    }
    return reports.find((report) => report.sessionId === activeReportId) ?? null
  },

  collect: async (source) => {
    const { selectedSerial, mode } = get()
    if (selectedSerial === null) {
      set({ notice: '请先选择一个设备' })
      return
    }

    try {
      const started = await api.collect(source, {
        serial: selectedSerial,
        mode,
      })
      set({
        sessions: get().sessions.concat(started),
        error: null,
        notice: `已开始采集：${started.command}`,
      })
    } catch (error) {
      const backendError = messagesOf(error)
      set({
        error: backendError.environmental ? null : backendError,
        notice: backendError.environmental ? backendError.message : null,
      })
    }
  },

  /**
   * Queues a batch for the next animation frame.
   *
   * The per-frame coalescing is what keeps a log storm from turning into a
   * per-event re-render storm.
   */
  appendRecords: (sessionId, rows) => {
    if (rows.length === 0) {
      return
    }
    // `raw` is dropped at the boundary: for parsed rows it is a second copy of
    // the line, and for unparsed rows it is identical to `message`. With a
    // 100 000-row buffer that duplication is the single biggest string cost, and
    // nothing in the table reads it — the backend ring keeps the original.
    enqueueRows(rows.map((row) => ({ ...row, sessionId, raw: '' })))
  },

  /** Folds a progress tick in, which is what drives the countdown. */
  applySessionProgress: (sessionId, progress) => {
    const sessions = get().sessions
    if (!sessions.some((session) => session.id === sessionId)) {
      // The collection already ended (or was stopped) — nothing to update.
      return
    }
    set({
      sessions: sessions.map((session) =>
        session.id === sessionId ? { ...session, progress } : session,
      ),
    })
  },

  /** Folds a session status event in, and retires sessions that have ended. */
  applySessionStatus: (sessionId, status) => {
    const sessions = get().sessions
    const target = sessions.find((session) => session.id === sessionId)
    if (target === undefined) {
      return
    }

    if (status.state === 'failed') {
      set({
        sessions: sessions.filter((session) => session.id !== sessionId),
        notice: status.message,
      })
      return
    }

    if (status.state === 'stopped') {
      // Natural exit (device unplugged, command finished): drop it so the UI
      // does not keep showing a session that can no longer produce records.
      set({ sessions: sessions.filter((session) => session.id !== sessionId) })
      return
    }

    set({
      sessions: sessions.map((session) =>
        session.id === sessionId ? { ...session, status } : session,
      ),
    })
  },

  setStructured: async (patch) => {
    const structured = { ...get().structured, ...patch }
    set({ structured })
    await pushFilters(set, get)
  },

  setAppTargetInput: (value) => set({ appTargetInput: value }),

  /**
   * Resolves the typed value and starts following it.
   *
   * The backend works out whether the input is a PID, a UID or a package name,
   * and returns every live pid including `pkg:remote` children. A result with
   * `found === false` is not an error: it means the app is not running, and the
   * panel says so instead of the filter silently matching nothing.
   */
  resolveAppTarget: async () => {
    const { appTargetInput, selectedSerial, mode } = get()
    const input = appTargetInput.trim()
    if (input.length === 0) {
      await get().clearAppTarget()
      return
    }
    if (selectedSerial === null) {
      set({ notice: '请先选择一个设备' })
      return
    }
    try {
      const target = await api.setAppTarget(selectedSerial, mode, input)
      set({ appTarget: target, error: null })
    } catch (error) {
      const backendError = messagesOf(error)
      set({
        error: backendError.environmental ? null : backendError,
        notice: backendError.message,
      })
    }
  },

  clearAppTarget: async () => {
    set({ appTargetInput: '' })
    try {
      await api.setAppTarget('', 'adb', '')
    } catch {
      // Clearing is best-effort: local state is the visible outcome.
    }
    set({ appTarget: null })
  },

  applyAppTarget: (target) => {
    // Keep the typed text in step with what the backend actually resolved, so
    // the box and the chip never disagree after an automatic refresh.
    set(target === null ? { appTarget: null } : { appTarget: target })
  },

  refreshRunningApps: async () => {
    const { selectedSerial, mode } = get()
    if (selectedSerial === null) {
      set({ runningApps: [] })
      return
    }
    try {
      const runningApps = await api.listRunningApps(selectedSerial, mode)
      set({ runningApps })
    } catch {
      // The picker is a convenience; a failure must not blank the panel.
      set({ runningApps: [] })
    }
  },

  addFilter: async (rule) => {
    const next = get().filters.concat(rule)
    try {
      await api.setFilters(next)
      set({ filters: next, filtersError: null })
    } catch (error) {
      set({ filtersError: messagesOf(error) })
    }
  },

  updateFilter: async (id, patch) => {
    const next = get().filters.map((rule) =>
      rule.id === id ? { ...rule, ...patch } : rule,
    )
    try {
      await api.setFilters(next)
      set({ filters: next, filtersError: null })
    } catch (error) {
      set({ filtersError: messagesOf(error) })
    }
  },

  removeFilter: async (id) => {
    const next = get().filters.filter((rule) => rule.id !== id)
    try {
      await api.setFilters(next)
      set({ filters: next, filtersError: null })
    } catch (error) {
      set({ filtersError: messagesOf(error) })
    }
  },

  clearFilters: async () => {
    try {
      await api.setFilters([])
      set({
        filters: [],
        filtersError: null,
        structured: EMPTY_STRUCTURED_FILTERS,
      })
    } catch (error) {
      set({ filtersError: messagesOf(error) })
    }
  },

  dismissError: () => set({ error: null }),
  dismissNotice: () => set({ notice: null }),
}))

/**
 * Recompiles and pushes the filter rules.
 *
 * The structured controls are compiled into the same generic rule list the
 * backend already understands, and the user's own rules are appended after them.
 * `set_filters` validates before storing, so an invalid value (a bad regex, say)
 * leaves the previous rules running and surfaces an error instead of silently
 * disabling filtering.
 */
async function pushFilters(
  set: (partial: Partial<AppStore>) => void,
  get: () => AppStore,
): Promise<void> {
  const next = [
    ...compileStructuredFilters(get().structured),
    ...get().advancedFilters(),
  ]
  try {
    await api.setFilters(next)
    set({ filters: next, filtersError: null })
  } catch (error) {
    set({ filtersError: api.toBackendError(error) })
  }
}

/**
 * Refreshes the collector list for the current execution mode.
 *
 * Availability is decided by the **mode**, not by whether the attached device
 * happens to have root: in ADB mode the kernel collectors are offered as
 * disabled with the reason shown, which is what makes "switch to Root first"
 * discoverable. The current selection is preserved when it is still usable and
 * falls back to the first available source otherwise, so leaving Root mode can
 * never strand the UI on a collector that cannot run.
 */
async function refreshSources(
  set: (partial: Partial<AppStore>) => void,
  mode: ExecMode,
  selectedSource: LogSourceKind,
  recovery: boolean,
): Promise<void> {
  try {
    // `mode === 'root'` doubles as the privilege signal: Root mode is only
    // reachable when the device can escalate, and that is exactly what decides
    // whether dmesg/kmsg are offered. `recovery` decides the recovery collector.
    const sources = await api.listSources(mode === 'root', recovery)
    const current = sources.find((entry) => entry.spec.kind === selectedSource)
    const next =
      current?.available === true
        ? selectedSource
        : (sources.find((entry) => entry.available)?.spec.kind ?? selectedSource)
    set({ sources, selectedSource: next })
  } catch (error) {
    // Keep whatever list was already shown and say so. Blanking the rail here is
    // how a `list_sources` signature change went unnoticed: the command was
    // rejected for a missing argument, the catch swallowed it, and the rail just
    // read "尚未解析采集源" as though no device had been chosen.
    const backendError = messagesOf(error)
    set({
      error: backendError.environmental ? null : backendError,
      notice: backendError.environmental ? backendError.message : null,
    })
  }
}
