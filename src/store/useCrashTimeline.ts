/**
 * Crash timeline state.
 *
 * Deliberately a separate store rather than more fields in `useAppStore`: the main store
 * is the capture's hot path (it batches ten thousand records a second and is read by the
 * table on every frame), while this holds cold data that only changes when a report is
 * opened. Keeping them apart means the timeline cannot cost the table a re-render.
 *
 * Three sources, all snapshots (the backend clones rather than drains, so opening,
 * closing and reopening the view shows the same story):
 *
 * * `events` — what the live watch noticed while capturing (crash blocks, AMS signals,
 *   resource anomalies), each carrying the capture sequence number it came from;
 * * `integrity` — the six checks, for the summary line and the warnings;
 * * `view` — the full analysis (`getForensics`): causal links, per-identity stories and
 *   anomalies attached to them, plus `seqAt`, the map from the analysers' own line
 *   numbering to capture sequence numbers.
 */

import { create } from 'zustand'

import * as api from '../api/backend'
import type { ForensicsView, IntegrityCheck, LiveEvent } from '../types'

interface CrashTimelineState {
  /** Live events (crash blocks, AMS signals, resource anomalies), in capture order. */
  events: LiveEvent[]
  /** Integrity findings for the same session, worst first. */
  integrity: IntegrityCheck[]
  /** The full analysis, or `null` before the first load. */
  view: ForensicsView | null
  /**
   * True when the data came from the on-disk snapshot rather than a live session.
   *
   * The log rows are not persisted, so the table cannot scroll to them: the view uses this to
   * stop offering jumps instead of offering ones that do nothing.
   */
  fromSnapshot: boolean
  /** True while the requests are in flight. */
  loading: boolean
  /** Why the load failed, if it did. */
  error: string | null
  /** Loads everything the view needs for one session, then remembers it. */
  load: (sessionId: string) => Promise<void>
  /** Restores the last remembered capture, when nothing live is selected. */
  hydrate: () => Promise<void>
  /** Drops the current data, when a different session is selected. */
  clear: () => void
}

export const useCrashTimeline = create<CrashTimelineState>((set, get) => ({
  events: [],
  integrity: [],
  view: null,
  fromSnapshot: false,
  loading: false,
  error: null,

  clear: () =>
    set({ events: [], integrity: [], view: null, fromSnapshot: false, error: null }),

  hydrate: async () => {
    // Already showing something: never overwrite live data with a stale file.
    if (get().view !== null) {
      return
    }
    try {
      const snapshot = await api.loadTimelineSnapshot()
      if (snapshot === null || get().view !== null) {
        return
      }
      set({
        events: snapshot.events,
        integrity: snapshot.view.analysis.checks,
        view: snapshot.view,
        fromSnapshot: true,
        error: null,
      })
    } catch (error) {
      // A missing or unreadable snapshot is the normal case on a first run.
      set({ error: error instanceof Error ? error.message : '读取上次时间线失败' })
    }
  },

  load: async (sessionId) => {
    set({ loading: true })
    try {
      // One round trip each, in parallel: the two reports are independent, and the
      // analysis is the expensive one (it scans the capture's rows), so it must not
      // serialise behind the cheap ones.
      const [events, integrity, view] = await Promise.all([
        api.getCrashEvents(sessionId),
        api.getIntegrity(sessionId),
        api.getForensics(sessionId),
      ])
      set({ events, integrity, view, fromSnapshot: false, loading: false, error: null })
      // Remember it, so a restart can show the same story. A failure here is not the user's
      // problem: the timeline is already on screen, and the console has the reason.
      void api.saveTimelineSnapshot(sessionId).catch(() => undefined)
    } catch (error) {
      set({
        loading: false,
        // Shown verbatim: a failed read is the user's problem to act on, and swallowing
        // it into an empty list would look like "nothing happened".
        error: error instanceof Error ? error.message : '读取崩溃时间线失败',
      })
    }
  },
}))
