/**
 * Application shell: three columns plus the toolbar.
 *
 * ```
 * +-------------------------------------------------------------+
 * | toolbar                                                     |
 * +-----------+---------------------------------+---------------+
 * | devices / | log table                       | filter rules  |
 * | sources   |                                 |               |
 * +-----------+---------------------------------+---------------+
 * ```
 *
 * This component also owns the two backend event subscriptions, so they are
 * registered exactly once and torn down on unmount.
 */

import type { JSX } from 'react'
import { useEffect } from 'react'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Snackbar, SnackbarDuration } from 'material-expressive-react'

import { Toolbar } from './components/Toolbar'
import { DevicePanel } from './components/DevicePanel'
import { LogTable } from './components/LogTable'
import { FilterPanel } from './components/FilterPanel'
import { CollectPanel } from './components/CollectPanel'
import { resolveTheme, useAppStore } from './store/useAppStore'
import { windowBackground } from './theme'
import { BACKEND_EVENTS } from './types'
import type {
  AppTarget,
  CollectionReport,
  CrashEvent,
  DevicesEvent,
  RecordsEvent,
  SessionProgressEvent,
  SessionStatusEvent,
} from './types'

/**
 * Guards against React StrictMode's double effect invocation.
 *
 * In development StrictMode mounts, unmounts and remounts, so an unguarded
 * effect body runs twice. `bootstrap` resolves adb and queries devices, and
 * `adb start-server` is **not atomic**: two concurrent cold-start queries race
 * for port 5037, exactly one wins, and the losers die with
 * `could not read ok from ADB Server` — which the user then sees as a failure
 * banner. Running bootstrap once removes the race the app was creating for
 * itself. (The backend also retries, for races caused by other tools.)
 */
let bootstrapRequested = false

/**
 * Reveals the window once, after React's first commit.
 *
 * The window is created hidden (`visible: false`) so it cannot appear before
 * there is something correct to show — which is what removed the white flash on
 * startup.
 *
 * Deliberately **not** deferred with `requestAnimationFrame`: rAF does not run
 * while the page is hidden, and a hidden window is precisely the state being
 * escaped, so the reveal would wait forever for the visibility it is trying to
 * create. (Measured: it did exactly that, and the Rust-side fallback had to fire.)
 * `useEffect` already runs after the DOM is committed but before the first paint,
 * so showing the window here paints the real UI as its first visible frame.
 */
let windowRevealRequested = false

function revealWindow(): void {
  if (windowRevealRequested) {
    return
  }
  windowRevealRequested = true
  void getCurrentWindow()
    .show()
    .catch((cause: unknown) => {
      // Not fatal: the backend shows the window itself if it stays hidden.
      console.error('droidlog: could not reveal the window', cause)
    })
}

export function App(): JSX.Element {
  const bootstrap = useAppStore((state) => state.bootstrap)
  const appendRecords = useAppStore((state) => state.appendRecords)
  const applySessionStatus = useAppStore((state) => state.applySessionStatus)
  const applyDevices = useAppStore((state) => state.applyDevices)
  const applyAppTarget = useAppStore((state) => state.applyAppTarget)
  const applyCrashes = useAppStore((state) => state.applyCrashes)
  const applyCollectReport = useAppStore((state) => state.applyCollectReport)
  const applySessionProgress = useAppStore((state) => state.applySessionProgress)
  const refreshDevices = useAppStore((state) => state.refreshDevices)
  const theme = useAppStore((state) => state.theme)
  const colorTheme = useAppStore((state) => state.colorTheme)
  const error = useAppStore((state) => state.error)
  const dismissError = useAppStore((state) => state.dismissError)
  const notice = useAppStore((state) => state.notice)
  const dismissNotice = useAppStore((state) => state.dismissNotice)

  useEffect(() => {
    if (bootstrapRequested) {
      return
    }
    bootstrapRequested = true
    void bootstrap()
    revealWindow()
  }, [bootstrap])

  // Keep the native window surface in step with the theme, so resizing and the
  // close animation never expose the platform default colour.
  useEffect(() => {
    const colour = windowBackground(colorTheme, resolveTheme(theme))
    void getCurrentWindow()
      .setBackgroundColor(colour)
      .catch(() => undefined)
  }, [theme, colorTheme])

  useEffect(() => {
    // `listen` resolves to an unlisten function; the cleanup awaits the promise
    // so a fast unmount cannot leak a listener.
    const records = listen<RecordsEvent>(BACKEND_EVENTS.records, (event) => {
      appendRecords(event.payload.sessionId, event.payload.records)
    })

    const status = listen<SessionStatusEvent>(
      BACKEND_EVENTS.sessionStatus,
      (event) => {
        applySessionStatus(event.payload.sessionId, event.payload.status)
      },
    )

    // The 2-second device poller is the single writer of device state.
    const devices = listen<DevicesEvent>(BACKEND_EVENTS.devices, (event) => {
      applyDevices(event.payload)
    })

    // The same poller re-resolves the followed application every 30 s; this is
    // how an app restart (and its new pid) reaches the chip without user action.
    const target = listen<AppTarget | null>(
      BACKEND_EVENTS.appTarget,
      (event) => {
        applyAppTarget(event.payload)
      },
    )

    // Crash classification arrives separately from the record it describes, so
    // the table marks rows by `seq` and simply leaves unmarked whatever it has
    // not received.
    const crash = listen<CrashEvent>(BACKEND_EVENTS.crash, (event) => {
      applyCrashes(event.payload.sessionId, event.payload.entries)
    })

    // Progress of a bounded collection: one event per poll tick, which is what
    // keeps the countdown moving after the session object was first created.
    const progress = listen<SessionProgressEvent>(
      BACKEND_EVENTS.sessionProgress,
      (event) => {
        applySessionProgress(event.payload.sessionId, event.payload.progress)
      },
    )

    // One per finished collection run: the panel keeps it after the session is
    // retired, which is the whole point — the diagnosis outlives the capture.
    const report = listen<CollectionReport>(
      BACKEND_EVENTS.collectReport,
      (event) => {
        applyCollectReport(event.payload)
      },
    )

    // The poller's first tick usually lands before this component mounts, and it
    // only speaks on change — so ask it to re-send. Chained off `devices` so the
    // listener is definitely registered before the payload can be emitted.
    void devices.then(() => refreshDevices())

    return () => {
      void records.then((unlisten) => unlisten())
      void status.then((unlisten) => unlisten())
      void devices.then((unlisten) => unlisten())
      void target.then((unlisten) => unlisten())
      void crash.then((unlisten) => unlisten())
      void progress.then((unlisten) => unlisten())
      void report.then((unlisten) => unlisten())
    }
  }, [
    appendRecords,
    applySessionStatus,
    applyDevices,
    applyAppTarget,
    applyCrashes,
    applyCollectReport,
    applySessionProgress,
    refreshDevices,
  ])

  return (
    <div className="dl-app">
      <Toolbar />

      <div className="dl-app__body">
        <DevicePanel />
        {/*
          The report is docked in the log column rather than floated over it: it
          describes the rows above it, so it must not cover them.
        */}
        <div className="dl-logs-column">
          <LogTable />
          <CollectPanel />
        </div>
        <FilterPanel />
      </div>

      {error !== null ? (
        <Snackbar
          className="dl-snackbar"
          // Errors stay until dismissed: a failure that vanishes after four
          // seconds is a failure the user never got to read.
          duration={SnackbarDuration.INDEFINITE}
          open
          multiLine
          closeButton
          supportingText={
            error.detail === null ? error.message : `${error.message}：${error.detail}`
          }
          onClose={dismissError}
          onDismiss={dismissError}
        />
      ) : notice !== null ? (
        /*
          Notices get a bar of their own. They used to render only inside the log
          pane's empty state, so anything the app said while rows were on screen —
          "已开始采集", "已导出 100,000 行 → C:\…" — was invisible. AUTO rather than
          INDEFINITE: a confirmation must not need dismissing, but ten seconds is
          long enough to read a path.
        */
        <Snackbar
          className="dl-snackbar"
          duration={SnackbarDuration.LONG}
          open
          multiLine
          supportingText={notice}
          onClose={dismissNotice}
          onDismiss={dismissNotice}
        />
      ) : null}
    </div>
  )
}
