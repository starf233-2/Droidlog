/**
 * Centre pane: the live log table.
 *
 * **Virtualised.** Only the rows inside the viewport (plus a small overscan) are
 * mounted. This is not a micro-optimisation: rendering the whole buffer was
 * measured driving the WebView renderer to 1.87 GB and minutes of CPU, at which
 * point clicks were delivered but their handlers could not run — which is what
 * made the stop button look dead. Windowing keeps the mounted row count
 * proportional to the window height (tens of rows) regardless of how many
 * records are retained, so a 100 000-row buffer costs the same as a 100-row one.
 *
 * The column template is defined once in {@link COLUMNS} and published to CSS as
 * `--dl-log-columns`, so the header and every row cannot drift apart.
 *
 * **Smooth follow.** Records arrive in batches (every 100 ms, or 200 records),
 * and setting `scrollTop` per batch made the view advance in visible steps. The
 * table now glides: one rAF loop closes a fixed fraction of the remaining
 * distance per frame ({@link FOLLOW_TAU_MS}), and a new batch only moves the
 * target, so the loop never restarts and the motion never stutters at a batch
 * boundary. Following stops the moment the user scrolls away and resumes when
 * they come back, and `prefers-reduced-motion` turns the glide back into a step.
 */

import type { JSX } from 'react'
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { TextButton } from 'material-expressive-react'

import { useAppStore } from '../store/useAppStore'
import {
  formatMessage,
  formatOptionalNumber,
  formatTimestamp,
  levelBadge,
} from '../lib/format'
import {
  crashKindBadge,
  crashKindLabel,
  formatRemaining,
} from '../lib/crash-labels'
import type { CrashKind, LogRow } from '../types'

/** Horizontal alignment of a column's content. */
type Align = 'left' | 'center' | 'right'

/**
 * Column definitions: header label, grid width and alignment.
 *
 * Alignment lives here and is applied to *both* the header cell and the body
 * cells via {@link alignClass}, because the two used to disagree — the PID/TID
 * headers sat left while their numbers were right-aligned, so they never lined
 * up.
 *
 * Widths are `minmax(min, ideal)` rather than fixed for every column but the
 * level badge, for two reasons:
 *
 * * with fixed widths a narrow window (the layout tightens below 1200 CSS px)
 *   squeezed the message column to about 50px and ellipsised it to nothing;
 * * right-aligned numbers sit flush against their column's right edge, so the
 *   gap to the next column is exactly the grid gap — with the old 8px gap the
 *   TID number all but touched the tag text. {@link COLUMN_GAP} widens it, and
 *   the tracks shrink toward their minimums instead of pushing the message
 *   column out when space runs short.
 */
const COLUMNS: readonly {
  key: string
  label: string
  width: string
  align: Align
}[] = [
  // The crash marker column: an empty header, and a badge only on rows the
  // classifier matched. It comes first so the marker is the first thing the eye
  // lands on when scanning for the failure among thousands of lines.
  { key: 'crash', label: '', width: '22px', align: 'center' },
  { key: 'level', label: '级别', width: '40px', align: 'center' },
  { key: 'timestamp', label: '时间', width: 'minmax(80px, 130px)', align: 'left' },
  { key: 'pid', label: 'PID', width: 'minmax(44px, 58px)', align: 'right' },
  { key: 'tid', label: 'TID', width: 'minmax(44px, 58px)', align: 'right' },
  { key: 'tag', label: 'TAG', width: 'minmax(60px, 130px)', align: 'left' },
  { key: 'source', label: '来源', width: 'minmax(52px, 72px)', align: 'left' },
  {
    key: 'message',
    label: '消息',
    width: 'minmax(112px, 1fr)',
    align: 'left',
  },
]

/**
 * Gutter between columns, in CSS pixels. Published to the stylesheet alongside
 * the template so the header and the rows can never disagree.
 */
const COLUMN_GAP = 16

/** Maps an alignment to the shared utility class used by header and body. */
function alignClass(align: Align): string {
  return `dl-align--${align}`
}

/**
 * Row height in CSS pixels. Must match `--dl-log-row-height` in the stylesheet;
 * a mismatch shows up as scrollbar drift, so the stylesheet derives the row's
 * height from the same number.
 */
const ROW_HEIGHT = 22
/** Rows rendered beyond the viewport on each side, to cover fast scrolling. */
const OVERSCAN = 8

/**
 * Time constant of the follow glide, in milliseconds.
 *
 * Each frame closes `1 - e^(-dt/TAU)` of the distance still to travel, which
 * makes the glide frame-rate independent: a 120 Hz window and a 60 Hz one cover
 * the same path in the same wall-clock time.
 *
 * The lag this would normally cost is paid back by the feed-forward term in the
 * loop, so TAU is free to be tuned for how the motion *looks*: 140 ms spreads a
 * 100 ms batch over 5–8 frames, which reads as continuous movement instead of a
 * step, and still settles within about a third of a second when a stream stops.
 */
const FOLLOW_TAU_MS = 140

/**
 * Beyond this many viewport heights of lag the table stops gliding and jumps.
 *
 * A first-order glide closes a fixed fraction of a *distance*, so chasing a
 * target that is moving away faster than the loop can close the gap would leave
 * the view reading ever-older lines. Past this threshold the honest answer is
 * "catch up now": that is what a first paint over a full buffer, or a return
 * after the window was minimised, actually needs.
 */
const SNAP_VIEWPORTS = 6

/**
 * Smallest movement worth recomputing the row window for, in CSS pixels.
 *
 * The window derives from React state. Committing every animation frame would
 * mean 60 state updates per second, each re-rendering ~40 rows — precisely the
 * kind of work that once made this table feel dead. Half a row of slack is
 * invisible because {@link OVERSCAN} already renders 8 rows past the viewport.
 */
const SCROLL_COMMIT_PX = ROW_HEIGHT / 2

/**
 * How long after the last wheel/key/touch event the scrollport keeps counting as
 * "driven by hand", in milliseconds.
 *
 * One gesture produces a burst of scroll events; the burst, not the individual
 * event, is the intent, so the flag is cleared on a timer rather than per event.
 */
const USER_INPUT_MS = 300

/**
 * Whether the user asked the platform to minimise motion.
 *
 * Under `prefers-reduced-motion: reduce` the table still follows the tail, it
 * just does so in a single step. A glide is decoration, and someone who turned
 * motion off should not be given an animation they cannot interrupt.
 */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false)
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') {
      return
    }
    const query = window.matchMedia('(prefers-reduced-motion: reduce)')
    setReduced(query.matches)
    const onChange = (event: MediaQueryListEvent): void => {
      setReduced(event.matches)
    }
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [])
  return reduced
}

function LogRowView({
  row,
  crash,
}: {
  row: LogRow
  // Explicitly `| undefined`: the project runs with
  // `exactOptionalPropertyTypes`, under which an absent property and one set to
  // `undefined` are different types.
  crash: CrashKind | undefined
}): JSX.Element {
  // Unparsed lines are dimmed and marked, but they are *never* hidden: they are
  // frequently the only record of a vendor-specific format.
  const raw = row.parsed ? '' : ' dl-log-row--raw'
  // A crash marker is a left edge plus a badge, not a filled row: the family has
  // to be readable at 22 px, and a colour wash would fight the level colours
  // that the table already spends its saturation budget on.
  const marked = crash === undefined ? '' : ' dl-log-row--crash'
  return (
    <div
      className={`dl-log-row${raw}${marked}`}
      style={{ height: `${ROW_HEIGHT}px` }}
    >
      {crash === undefined ? (
        <span className="dl-log-row__crash" aria-hidden="true" />
      ) : (
        <span
          className="dl-log-row__crash dl-log-row__crash--marked"
          title={crashKindLabel(crash)}
        >
          {crashKindBadge(crash)}
        </span>
      )}
      <span className={`dl-log-row__level dl-level--${row.level} ${alignClass('center')}`}>
        {row.parsed ? levelBadge(row.level) : '·'}
      </span>
      <span className={`dl-log-row__time dl-mono ${alignClass('left')}`}>
        {formatTimestamp(row)}
      </span>
      <span className={`dl-log-row__num dl-mono ${alignClass('right')}`}>
        {formatOptionalNumber(row.pid)}
      </span>
      <span className={`dl-log-row__num dl-mono ${alignClass('right')}`}>
        {formatOptionalNumber(row.tid)}
      </span>
      <span className={`dl-log-row__tag dl-mono ${alignClass('left')}`} title={row.tag ?? undefined}>
        {row.tag ?? '-'}
      </span>
      <span className={`dl-log-row__session dl-mono ${alignClass('left')}`} title={row.sessionId}>
        {row.source}
      </span>
      <span className={`dl-log-row__message dl-mono ${alignClass('left')}`} title={row.message}>
        {formatMessage(row.message)}
      </span>
    </div>
  )
}

export function LogTable(): JSX.Element {
  const records = useAppStore((state) => state.records)
  const sessions = useAppStore((state) => state.sessions)
  const stopSession = useAppStore((state) => state.stopSession)
  const stopAllSessions = useAppStore((state) => state.stopAllSessions)
  const notice = useAppStore((state) => state.notice)
  const crashes = useAppStore((state) => state.crashes)
  const crashOnly = useAppStore((state) => state.crashOnly)
  const setCrashOnly = useAppStore((state) => state.setCrashOnly)
  const jumpToSeq = useAppStore((state) => state.jumpToSeq)
  const clearJump = useAppStore((state) => state.clearJump)
  const jumpToNextCrash = useAppStore((state) => state.jumpToNextCrash)
  const collect = useAppStore((state) => state.collect)
  const hasDevice = useAppStore((state) => state.selectedDevice() !== null)
  const recovery = useAppStore(
    (state) => state.selectedDevice()?.recovery === true,
  )

  /** `seq` to family, for the row markers. */
  const crashKinds = useMemo(
    () => new Map(crashes.map((entry) => [entry.seq, entry.kind])),
    [crashes],
  )

  /**
   * The rows the table actually pages through.
   *
   * Filtering here rather than in the backend keeps the ring untouched: the crash
   * view is a lens on the buffer, and switching it off must not require
   * recollecting anything.
   */
  const shown = useMemo(
    () => (crashOnly ? records.filter((row) => crashKinds.has(row.seq)) : records),
    [crashOnly, records, crashKinds],
  )

  /** Clock for the collection countdowns; ticks only while one is running. */
  const [nowMs, setNowMs] = useState(() => Date.now())
  const counting = sessions.some(
    (session) => session.status.state === 'running' && session.progress !== null,
  )
  useEffect(() => {
    if (!counting) {
      return
    }
    const handle = window.setInterval(() => setNowMs(Date.now()), 1000)
    return () => window.clearInterval(handle)
  }, [counting])

  const rootRef = useRef<HTMLElement | null>(null)
  const scrollerRef = useRef<HTMLDivElement | null>(null)
  const [follow, setFollow] = useState(true)
  const [scrollTop, setScrollTop] = useState(0)
  const [viewport, setViewport] = useState(0)
  const reducedMotion = usePrefersReducedMotion()

  /** Offset the row window was last computed from. */
  const committedRef = useRef(0)
  /** Offset the glide last wrote; a scroll landing here is our own, not the user's. */
  const programmaticRef = useRef(-1)
  /** Bottom offset worth gliding toward, refreshed once per rendered batch. */
  const targetRef = useRef(0)
  /** When {@link targetRef} was last refreshed, for the velocity estimate. */
  const targetTimeRef = useRef(0)
  /** How fast the bottom is moving away, in pixels per millisecond. */
  const velocityRef = useRef(0)
  /** Handle of the running glide, or 0 when the table is at rest. */
  const glideRef = useRef(0)
  /** True while a wheel, key or touch gesture owns the scrollport. */
  const userInputRef = useRef(false)
  const userInputTimerRef = useRef(0)

  /** Publishes a scroll offset to the row window, at most once per half row. */
  const commit = useCallback((next: number) => {
    if (Math.abs(next - committedRef.current) < SCROLL_COMMIT_PX) {
      return
    }
    committedRef.current = next
    setScrollTop(next)
  }, [])

  /** Moves the scrollport and records the move as ours rather than the user's. */
  const write = useCallback(
    (next: number) => {
      const node = scrollerRef.current
      if (node === null) {
        return
      }
      programmaticRef.current = next
      node.scrollTop = next
      commit(next)
    },
    [commit],
  )

  const stopGlide = useCallback(() => {
    if (glideRef.current !== 0) {
      cancelAnimationFrame(glideRef.current)
      glideRef.current = 0
    }
  }, [])

  /** Marks the scrollport as user-driven for the duration of the gesture. */
  const markUserInput = useCallback(() => {
    userInputRef.current = true
    window.clearTimeout(userInputTimerRef.current)
    userInputTimerRef.current = window.setTimeout(() => {
      userInputRef.current = false
    }, USER_INPUT_MS)
  }, [])

  const gridTemplate = useMemo(
    () => COLUMNS.map((column) => column.width).join(' '),
    [],
  )

  // Publish the template and the gutter as custom properties instead of
  // repeating an inline grid on every row: `setProperty` is typed, so no cast is
  // needed, and the header and the rows read the same two values.
  useEffect(() => {
    const root = rootRef.current
    if (root === null) {
      return
    }
    root.style.setProperty('--dl-log-columns', gridTemplate)
    root.style.setProperty('--dl-log-column-gap', `${COLUMN_GAP}px`)
  }, [gridTemplate])

  // Track the scrollport height so the window size follows window resizes.
  useLayoutEffect(() => {
    const node = scrollerRef.current
    if (node === null) {
      return
    }
    setViewport(node.clientHeight)
    const observer = new ResizeObserver(() => setViewport(node.clientHeight))
    observer.observe(node)
    return () => observer.disconnect()
  }, [])

  // Where the bottom is, how fast it is moving away, and the offset the window
  // was built from — all read here, after React has rendered the new spacers,
  // and never from inside the animation loop, where `scrollHeight` would force a
  // layout read on every frame.
  useLayoutEffect(() => {
    const node = scrollerRef.current
    if (node === null) {
      return
    }
    const next = node.scrollHeight - node.clientHeight
    const now = performance.now()
    const elapsed = now - targetTimeRef.current
    if (next > targetRef.current && elapsed > 0) {
      // Smoothed so one unusually large batch cannot spike the feed-forward.
      const instant = (next - targetRef.current) / elapsed
      velocityRef.current = velocityRef.current * 0.7 + instant * 0.3
    } else if (next <= targetRef.current) {
      // The buffer was trimmed or cleared: nothing is running away any more.
      velocityRef.current = 0
    }
    targetRef.current = next
    targetTimeRef.current = now
    // Clearing or trimming the buffer clamps the offset without a scroll event
    // this component can attribute, so resync the window from the real value.
    if (Math.abs(node.scrollTop - committedRef.current) >= 1) {
      committedRef.current = node.scrollTop
      setScrollTop(node.scrollTop)
    }
  }, [records, viewport])

  // Follow the tail: one rAF loop that outlives individual batches. A new batch
  // only moves `targetRef`, and the loop that is already chasing it keeps going,
  // which is what removes the stutter at batch boundaries.
  useEffect(() => {
    if (!follow) {
      stopGlide()
      return
    }
    const node = scrollerRef.current
    if (node === null) {
      return
    }
    if (reducedMotion) {
      write(targetRef.current)
      return
    }
    if (glideRef.current !== 0) {
      return
    }
    let last = performance.now()
    const tick = (now: number): void => {
      const scroller = scrollerRef.current
      if (scroller === null) {
        glideRef.current = 0
        return
      }
      const target = targetRef.current
      const top = scroller.scrollTop
      const gap = target - top
      if (gap <= 0.5) {
        // Land exactly: a glide that stops half a pixel short leaves the
        // "following" threshold permanently unhappy.
        write(target)
        glideRef.current = 0
        return
      }
      const dt = Math.min(64, Math.max(1, now - last))
      last = now
      if (gap > scroller.clientHeight * SNAP_VIEWPORTS) {
        write(target)
        glideRef.current = 0
        return
      }
      // Two terms, because one is not enough: the exponential closes the gap
      // that exists now, and the feed-forward carries the view along with a
      // bottom that is itself moving. Without the second term a fast stream
      // leaves the table a fixed distance behind the newest line, forever.
      const step = Math.max(0.5, gap * (1 - Math.exp(-dt / FOLLOW_TAU_MS)))
      const next = top + step + velocityRef.current * dt
      write(next >= target ? target : next)
      glideRef.current = requestAnimationFrame(tick)
    }
    glideRef.current = requestAnimationFrame(tick)
  }, [records, follow, reducedMotion, stopGlide, write])

  // A glide must not outlive the table it is scrolling, and neither must the
  // gesture timer that gates it.
  useEffect(
    () => () => {
      stopGlide()
      window.clearTimeout(userInputTimerRef.current)
    },
    [stopGlide],
  )

  /**
   * Scrolls to a requested row, then clears the request.
   *
   * The store holds a `seq` rather than an offset because the row may not be in
   * the visible window — or even in the buffer yet — when the request is made,
   * and because the crash panel and the table must not agree on pixel maths.
   */
  useEffect(() => {
    if (jumpToSeq === null) {
      return
    }
    const scroller = scrollerRef.current
    const index = shown.findIndex((row) => row.seq === jumpToSeq)
    if (scroller === null || index < 0) {
      clearJump()
      return
    }
    const offset = Math.max(
      0,
      index * ROW_HEIGHT - Math.max(0, viewport / 2 - ROW_HEIGHT),
    )
    // Landing on a crash is the opposite of following the tail.
    setFollow(false)
    stopGlide()
    committedRef.current = offset
    programmaticRef.current = offset
    scroller.scrollTop = offset
    setScrollTop(offset)
    clearJump()
  }, [jumpToSeq, shown, viewport, clearJump, stopGlide])

  const onScroll = useCallback(
    (event: React.UIEvent<HTMLDivElement>) => {
      const node = event.currentTarget
      const next = node.scrollTop
      // A scroll this component caused is not user intent. Without this check
      // every frame of the glide would look like the user dragging the
      // scrollbar, and following would switch itself off mid-batch.
      if (Math.abs(next - programmaticRef.current) < 1) {
        return
      }
      // The row window still has to hear about it — the browser clamps the
      // offset when the buffer shrinks, and anchoring nudges it — so commit
      // first and decide about intent second.
      committedRef.current = next
      setScrollTop(next)
      // Anything the browser did on its own is not the user leaving the tail.
      // Only a wheel, key or touch gesture may switch following off; inferring
      // it from scroll events made Chromium's scroll anchoring stop the follow
      // while the table was still pinned to the bottom.
      if (!userInputRef.current) {
        return
      }
      stopGlide()
      const distance = node.scrollHeight - node.scrollTop - node.clientHeight
      setFollow(distance < ROW_HEIGHT * 2)
    },
    [stopGlide],
  )

  // Window bounds. Clamped so the last page still renders when the buffer is
  // shorter than the viewport.
  const total = shown.length
  const visibleRows = Math.max(1, Math.ceil((viewport || 600) / ROW_HEIGHT))
  const maxStart = Math.max(0, total - visibleRows)
  const start = Math.min(
    maxStart,
    Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN),
  )
  const end = Math.min(total, start + visibleRows + OVERSCAN * 2)
  const windowRows = shown.slice(start, end)
  const topPad = start * ROW_HEIGHT
  const bottomPad = Math.max(0, (total - end) * ROW_HEIGHT)

  const running = sessions.filter((session) => session.status.state === 'running')

  /**
   * Bounded collections currently reporting progress.
   *
   * Only the collectors that end by themselves publish progress, so this is
   * exactly the set of sessions that deserve a countdown.
   */
  const countdowns = running.filter((session) => session.progress !== null)

  return (
    <section className="dl-logs" aria-label="日志" ref={rootRef}>
      <div className="dl-logs__actions">
        <TextButton
          className={`dl-toggle${crashOnly ? ' dl-toggle--on' : ''}`}
          // A string, not a boolean: the library drops a bare boolean attribute,
          // and `aria-pressed=""` tells a screen reader nothing.
          aria-pressed={crashOnly ? 'true' : 'false'}
          onClick={() => setCrashOnly(!crashOnly)}
          disabled={!crashOnly && crashes.length === 0}
          title="只显示被判定为崩溃的日志行"
        >
          {crashes.length === 0 ? '仅看崩溃' : `仅看崩溃 (${crashes.length})`}
        </TextButton>
        <TextButton
          onClick={() => jumpToNextCrash(shown[start]?.seq ?? 0)}
          disabled={crashes.length === 0}
          title="滚动到下一条崩溃日志"
        >
          下一个崩溃
        </TextButton>

        {/*
          The one-shot collectors live here rather than in the toolbar. They fill
          this pane, so the action belongs next to its output — and three more
          buttons in the top bar pushed its two clusters into each other at
          1440px, which is the width this window actually runs at.
        */}
        <div className="dl-logs__collect">
          <TextButton
            onClick={() => void collect('crash')}
            disabled={hasDevice === false}
            title="采集崩溃日志：logcat -b crash、事件缓冲、tombstone、dropbox 与 ANR"
          >
            崩溃日志
          </TextButton>
          <TextButton
            onClick={() => void collect('boot')}
            disabled={hasDevice === false}
            title="采集启动日志：内核缓冲快照，然后轮询到设备启动完成"
          >
            启动日志
          </TextButton>
          <TextButton
            onClick={() => void collect('recovery')}
            disabled={hasDevice === false || recovery === false}
            title={
              recovery
                ? '采集 Recovery 日志：/tmp/recovery.log、/cache/recovery/last_log、内核缓冲与 pstore'
                : '仅当设备处于 Recovery／Sideload 模式时可用'
            }
          >
            Recovery
          </TextButton>
        </div>

        {countdowns.map((session) => {
          const progress = session.progress
          if (progress === null) {
            return null
          }
          const remaining = formatRemaining(progress.endsAtMs, nowMs)
          return (
            <span key={session.id} className="dl-countdown" title={session.command}>
              <span className="dl-countdown__label">{progress.label}</span>
              <span className="dl-countdown__value dl-mono">
                {progress.done}/{progress.total}
                {remaining === null ? ' · 即将结束' : ` · 剩余 ${remaining}`}
              </span>
            </span>
          )
        })}
      </div>

      <div className="dl-logs__head">
        {COLUMNS.map((column) => (
          <span
            key={column.key}
            className={`dl-logs__head-cell ${alignClass(column.align)}`}
          >
            {column.label}
          </span>
        ))}
      </div>

      <div
        className="dl-logs__scroller dl-scroll"
        ref={scrollerRef}
        onScroll={onScroll}
        onWheel={markUserInput}
        onPointerDown={markUserInput}
        onTouchStart={markUserInput}
        onKeyDown={markUserInput}
      >
        {total === 0 ? (
          <div className="dl-empty dl-logs__placeholder">
            <span className="dl-empty__title">
              {running.length > 0
                ? '采集中，等待日志…'
                : crashOnly && records.length > 0
                  ? '没有识别到崩溃日志'
                  : '暂无日志'}
            </span>
            <span className="dl-empty__hint">
              {crashOnly && records.length > 0
                ? '当前缓冲没有崩溃行：关闭「仅看崩溃」可以查看全部日志，或采集一次崩溃日志。'
                : '在左侧选择设备与采集源，然后点击「开始采集」。可同时采集多个设备／采集源。'}
            </span>
            {notice !== null ? (
              <span className="dl-empty__hint dl-mono">{notice}</span>
            ) : null}
          </div>
        ) : (
          <>
            {/* Spacers keep the scrollbar honest while only the window is mounted. */}
            <div style={{ height: `${topPad}px` }} aria-hidden="true" />
            {windowRows.map((row) => (
              <LogRowView key={row.seq} row={row} crash={crashKinds.get(row.seq)} />
            ))}
            <div style={{ height: `${bottomPad}px` }} aria-hidden="true" />
          </>
        )}
      </div>

      <footer className="dl-logs__foot">
        {running.length === 0 ? (
          <span className="dl-logs__foot-item">未在采集</span>
        ) : (
          <>
            {running.map((session) => (
              <span key={session.id} className="dl-session" title={session.command}>
                <span className="dl-session__label">
                  {session.source} · {session.serial}
                </span>
                <TextButton
                  className="dl-session__stop"
                  aria-label={`停止 ${session.source} @ ${session.serial}`}
                  onClick={() => void stopSession(session.id)}
                >
                  ✕
                </TextButton>
              </span>
            ))}
            {running.length > 1 ? (
              <TextButton
                className="dl-logs__follow"
                onClick={() => void stopAllSessions()}
              >
                全部停止
              </TextButton>
            ) : null}
          </>
        )}

        {follow ? null : (
          <TextButton className="dl-logs__follow" onClick={() => setFollow(true)}>
            回到底部
          </TextButton>
        )}
      </footer>
    </section>
  )
}
