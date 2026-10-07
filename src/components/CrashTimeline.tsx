/**
 * Crash timeline.
 *
 * The point of the whole crash-forensics work, in one view: instead of "the settings
 * process was Force finished", it shows the crash that caused it, the processes torn
 * down with it, the AMS events in between and the resource pressure that preceded it —
 * in capture order, each row carrying the line it came from so clicking jumps to it in
 * the log table.
 *
 * It sits under the log table rather than in a fourth column: the table stays the main
 * surface, and the timeline is a lens on the same capture. Rows reuse the log list's
 * own classes, so nothing here introduces new visual language (no gradients, one
 * radius scale, text selection still limited to the logs).
 */

import type { JSX } from 'react'
import { useEffect, useRef, useState } from 'react'

import { TextButton } from 'material-expressive-react'

import { CrashForensics } from './CrashForensics'

import { Collapse, CollapseArrow } from './Collapse'
import { useAppStore } from '../store/useAppStore'
import { useCrashTimeline } from '../store/useCrashTimeline'
import type { AmsSignal, CrashEvent, LiveEvent, ResourceAnomaly } from '../types'

/** The most recent session, or none. */
function latestSessionId(sessions: readonly { id: string }[]): string | null {
  if (sessions.length === 0) {
    return null
  }
  return sessions[sessions.length - 1]?.id ?? null
}

/** One line for a crash: what broke, where, and how much stack there is. */
function crashLine(crash: CrashEvent): string {
  // "未知进程" was a lie dressed as information: the badge on the row already says this
  // is a crash, and the line itself often carries no process (a bare `FATAL EXCEPTION`
  // block has none). Show what is known — the pid when there is one — and nothing else.
  const subject = crash.process ?? (crash.pid === null ? '' : `pid ${crash.pid}`)
  const what = crash.exception ?? crash.message ?? '崩溃'
  const frames = crash.frames.length === 0 ? '无堆栈' : `${crash.frames.length} 帧`
  return [subject, what, frames].filter((part) => part !== '').join(' · ')
}

/** One line for an AMS signal. */
function amsLine(signal: AmsSignal): string {
  const subject = signal.process ?? (signal.pid === null ? '未知进程' : `pid ${signal.pid}`)
  const reason = signal.reason === null ? '' : ` · ${signal.reason}`
  return `${subject} · ${signal.kind}${reason}`
}

/** One line for a resource anomaly. */
function anomalyLine(anomaly: ResourceAnomaly): string {
  const subject = anomaly.process ?? (anomaly.pid === null ? '' : `pid ${anomaly.pid}`)
  return `${subject === '' ? '' : `${subject} · `}${anomaly.kind} · ${anomaly.detail}`
}

/** Which source a timeline row came from, or which non-crash kind it is. */
type TimelineTab =
  | 'all'
  | 'crashBuffer'
  | 'dropbox'
  | 'tombstone'
  | 'anrTrace'
  | 'kernel'
  | 'ams'
  | 'anomaly'

/**
 * The tabs, in the order a crash is worth reading.
 *
 * The sources stay apart on purpose: a tombstone is a native crash with an abort message, an
 * ANR trace is a stuck process, and a dropbox entry is the system's own record of either —
 * merged into one stream, the reader has to sort that out. "全部" stays the default so
 * nothing is hidden, but the isolation is one click away.
 */
const TABS: { id: TimelineTab; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'crashBuffer', label: '崩溃缓冲' },
  { id: 'dropbox', label: 'Dropbox' },
  { id: 'tombstone', label: '墓碑' },
  { id: 'anrTrace', label: 'ANR' },
  { id: 'kernel', label: '内核' },
  { id: 'ams', label: 'AMS' },
  { id: 'anomaly', label: '资源异常' },
]

/** The tab one event belongs to. */
function tabOf(event: LiveEvent): TimelineTab {
  if (event.payload.type === 'crash') {
    return event.payload.origin
  }
  return event.payload.type === 'ams' ? 'ams' : 'anomaly'
}

const SEVERITY_BADGE: Record<ResourceAnomaly['severity'], string> = {
  critical: '严重',
  warning: '提示',
}

export function CrashTimeline(): JSX.Element {
  const sessions = useAppStore((state) => state.sessions)
  const requestJump = useAppStore((state) => state.requestJump)
  const events = useCrashTimeline((state) => state.events)
  const integrity = useCrashTimeline((state) => state.integrity)
  const loading = useCrashTimeline((state) => state.loading)
  const error = useCrashTimeline((state) => state.error)
  const load = useCrashTimeline((state) => state.load)
  const view = useCrashTimeline((state) => state.view)
  const fromSnapshot = useCrashTimeline((state) => state.fromSnapshot)
  const hydrate = useCrashTimeline((state) => state.hydrate)
  const clear = useCrashTimeline((state) => state.clear)

  const [open, setOpen] = useState(false)
  const [tab, setTab] = useState<TimelineTab>('all')
  // The raw event list caps at 200 rows for the same reason the groups do; this is its way
  // back out, so the cap is a default rather than a limit.
  const [expanded, setExpanded] = useState(false)
  // A capture that produced crash material must not need a second click to be seen.
  // Opening happens once, when the first event arrives: closing the panel afterwards
  // means the user wants it closed, so it must not spring back open.
  const autoOpened = useRef(false)
  useEffect(() => {
    if (!autoOpened.current && events.length > 0) {
      autoOpened.current = true
      setOpen(true)
    }
  }, [events])
  const sessionId = latestSessionId(sessions)

  // Reload whenever the view is open and the session changes: the snapshot belongs to
  // one capture, and showing the previous one's story would be worse than showing none.
  useEffect(() => {
    if (!open) {
      return
    }
    if (sessionId === null) {
      // Nothing live to analyse: show the last capture we remembered rather than an empty
      // panel. `hydrate` refuses to overwrite live data, so this cannot race the load below.
      void hydrate()
      return
    }
    // A live session supersedes the remembered one.
    clear()
    void load(sessionId)
  }, [open, sessionId, load, clear, hydrate])

  const warnings = integrity.filter((check) => check.status === 'warning')
  // Capture order: the line index is the same coordinate the log table uses.
  const ordered = [...events].sort((left, right) => left.lineIndex - right.lineIndex)
  // Identical crashes repeat: the crash buffer keeps every occurrence, and ten copies of one
  // stack push everything else off the screen. They collapse into a single row that says how
  // many times it happened and still jumps to the first one — the count is the information,
  // the repetition is not.
  const grouped = (() => {
    const byKey = new Map<string, { event: (typeof ordered)[number]; count: number }>()
    for (const event of ordered) {
      const key =
        event.payload.type === 'crash'
          ? `crash|${event.payload.exception ?? ''}|${event.payload.frames.join('\n')}`
          : event.id
      const seen = byKey.get(key)
      if (seen === undefined) {
        byKey.set(key, { event, count: 1 })
      } else {
        seen.count += 1
      }
    }
    return [...byKey.values()]
  })()

  const counts = new Map<TimelineTab, number>()
  for (const entry of grouped) {
    const id = tabOf(entry.event)
    counts.set(id, (counts.get(id) ?? 0) + 1)
  }
  const visible = grouped.filter((entry) => tab === 'all' || tabOf(entry.event) === tab)

  return (
    <section className="dl-panel__section dl-timeline" aria-label="崩溃时间线">
      <div className="dl-target__row">
        <TextButton
          className="dl-panel__action"
          onClick={() => setOpen((current) => !current)}
        >
          {open ? '收起崩溃时间线' : '崩溃时间线'}
          {/* Rotates with the box, on the same tokens: one gesture, not two. */}
          <CollapseArrow open={open} />
        </TextButton>
        {open && fromSnapshot ? (
          <span className="dl-panel__hint">上次采集的快照：日志行未保存，跳转已停用</span>
        ) : null}
        {open && ordered.length > 0 ? (
          <span className="dl-panel__hint">
            {ordered.length} 条事件
            {warnings.length > 0 ? ` · ${warnings.length} 项完整性告警` : ''}
          </span>
        ) : null}
      </div>

      <Collapse open={open}>
          {ordered.length > 0 ? (
            <div className="dl-timeline__tabs" role="tablist" aria-label="崩溃来源">
              {TABS.map((entry) => (
                <TextButton
                  key={entry.id}
                  className={entry.id === tab ? 'dl-panel__action dl-tab--on' : 'dl-panel__action'}
                  onClick={() => setTab(entry.id)}
                >
                  {entry.id === 'all'
                    ? `${entry.label}（${ordered.length}）`
                    : `${entry.label}（${counts.get(entry.id) ?? 0}）`}
                </TextButton>
              ))}
            </div>
          ) : null}

          {sessionId === null ? (
            <p className="dl-panel__hint">先开始一次采集。</p>
          ) : null}
          {loading ? <p className="dl-panel__hint">读取中…</p> : null}
          {error !== null ? <p className="dl-panel__hint">{error}</p> : null}



          {ordered.length === 0 && !loading && sessionId !== null ? (
            <p className="dl-panel__hint">
              未发现崩溃或异常事件。
            </p>
          ) : null}

          <CrashForensics view={view} onJump={requestJump} />

          {ordered.length > 0 ? (
            <section className="dl-forensics__group">
              <span className="dl-forensics__title">
                {tab === 'all' ? '崩溃事件' : `崩溃事件 · ${TABS.find((t) => t.id === tab)?.label ?? ''}`}
              </span>
              <ul className="dl-apps" aria-label="崩溃时间线事件">
              {(expanded ? visible : visible.slice(0, 200)).map(({ event, count }) => {
                // Narrowed by the switch, not by a boolean: the payload is an
                // internally tagged union, and only the discriminant narrows it.
                let badge = '资源'
                let title = ''
                // A probe event has no row of its own: its text came from a file on the
                // device (`/data/system/dropbox`, `/data/anr/traces.txt`, a tombstone),
                // not from the log stream, so line 0 is the "no line" marker rather than
                // row zero — saying "第 0 行" would send the reader to the wrong place.
                const fromProbe = event.lineIndex === 0
                let detail = fromProbe ? '探针读取' : `记录 ${event.lineIndex}`
                switch (event.payload.type) {
                  case 'crash': {
                    badge = '崩溃'
                    title = crashLine(event.payload)
                    const frame = event.payload.frames[0]
                    if (frame !== undefined) {
                      detail = `${detail} · ${frame}`
                    }
                    break
                  }
                  case 'ams': {
                    badge = 'AMS'
                    title = amsLine(event.payload)
                    break
                  }
                  case 'anomaly': {
                    badge = `资源（${SEVERITY_BADGE[event.payload.severity]}）`
                    title = anomalyLine(event.payload)
                    break
                  }
                }
                if (count > 1) {
                  detail = `重复 ${count} 次 · ${detail}`
                }
                return (
                  <li key={event.id}>
                    <button
                      type="button"
                      className="dl-apps__item"
                      // Jumping needs the row to still exist. A hydrated timeline comes from a
                      // snapshot and has no ring buffer behind it, so the button stays put
                      // instead of scrolling the table somewhere arbitrary.
                      disabled={fromSnapshot || fromProbe}
                      onClick={() => {
                        if (!fromProbe && !fromSnapshot) {
                          requestJump(event.lineIndex)
                        }
                      }}
                      title={
                        fromSnapshot
                          ? '来自上次采集的快照，日志行未保存'
                          : fromProbe
                            ? '来自探针读取的设备文件'
                            : `跳到记录 ${event.lineIndex}`
                      }
                    >
                      <span className="dl-apps__name">
                        {badge}
                        {event.complete ? '' : ' · 增长中'}
                        {` · ${title}`}
                      </span>
                      <span className="dl-apps__package dl-mono">{detail}</span>
                    </button>
                  </li>
                )
              })}
              </ul>
            </section>
          ) : null}

          {visible.length > 200 ? (
            <button
              type="button"
              className="dl-panel__action"
              onClick={() => setExpanded(!expanded)}
            >
              {expanded
                ? `收起（只显示前 200 条 / 共 ${visible.length} 条）`
                : `展开全部（还有 ${visible.length - 200} 条）`}
            </button>
          ) : null}
      </Collapse>
    </section>
  )
}
