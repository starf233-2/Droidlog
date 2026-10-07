/**
 * The grouped forensics view: who crashed, who was taken down with them, and why.
 *
 * The raw event list above this panel shows *signals*; this shows the analysis of them.
 * The difference matters where the device is terse: `isCrashing=true` on its own names no
 * process, so the raw list can only say "unknown process" — while the analysis has the
 * surrounding lines and can say which process was crashing and which were Force finished
 * with it. That is the point of the offline pipeline, and this is where a reader sees it.
 *
 * Jumping is offered only where an honest coordinate exists: resource anomalies carry the
 * line they were matched on, so they translate through `seqAt` and jump. Causal links do
 * not — their evidence is the analyser's own record, and the view does not render it,
 * because using a list position as a line number would jump to an unrelated row.
 */

import type { JSX } from 'react'
import { useState } from 'react'

import type { CausalLink, CrashStory, ForensicsView, IntegrityCheck, KnownNote, ResourceAnomaly } from '../types'
import { Collapse } from './Collapse'

/** Translates an analyser line index into a capture sequence number. */
function seqOf(view: ForensicsView, lineIndex: number): number | null {
  const seq = view.seqAt[lineIndex]
  return seq === undefined ? null : seq
}

/** Stories worth showing: those that crashed or lost a process. */
function crashedStories(view: ForensicsView): CrashStory[] {
  return view.analysis.correlation.stories
    .filter((story) => story.crashes.length > 0 || story.victims.length > 0)
    .sort((left, right) => right.crashes.length - left.crashes.length)
}

/** `source → victim`, with the analyser's reason. */
function linkLabel(link: CausalLink): string {
  const from = link.sourcePid === null ? link.source : `${link.source}(${link.sourcePid})`
  const to = link.victimPid === null ? link.victim : `${link.victim}(${link.victimPid})`
  return `${from} → ${to} · ${link.reason}`
}

/** `process · kind（严重）` for a resource anomaly. */
function anomalyLabel(anomaly: ResourceAnomaly): string {
  // `pid 4` in a logcat-sourced line is a kernel thread, not an application: calling it
  // "pid 4" invites the reader to look for process 4 in the app list and find nothing.
  const who =
    anomaly.process ??
    (anomaly.pid === null ? '' : anomaly.pid <= 4 ? '内核' : `pid ${anomaly.pid}`)
  const severity = anomaly.severity === 'critical' ? '（严重）' : ''
  return `${who} · ${anomaly.kind}${severity}`
}

/** One integrity finding, shared by the first page and the expanded remainder. */
function CheckRow({ check }: { check: IntegrityCheck }): JSX.Element {
  return (
    <li>
      <div className="dl-apps__item">
        <span className="dl-apps__name">{check.label}</span>
        <span className="dl-apps__package">{check.detail}</span>
      </div>
    </li>
  )
}

export function CrashForensics({
  view,
  onJump,
}: {
  view: ForensicsView | null
  onJump: (seq: number) => void
}): JSX.Element | null {
  // Group caps keep a big capture from mounting hundreds of rows at once; the totals stay
  // visible and this toggle opens the rest. Hooks come first: the early return below must
  // not sit between them.
  const [expanded, setExpanded] = useState(false)
  if (view === null) {
    return null
  }
  const warnings = view.analysis.checks.filter((check) => check.status === 'warning')
  const notice = view.notice
  // One note per *kind* of known crash, with a count: ten identical Dirac failures are one
  // thing to fix, and ten rows saying so is noise.
  const knownByTitle = new Map<string, { note: KnownNote; count: number }>()
  for (const note of view.analysis.known) {
    const seen = knownByTitle.get(note.title)
    if (seen === undefined) {
      knownByTitle.set(note.title, { note, count: 1 })
    } else {
      seen.count += 1
    }
  }
  const known = [...knownByTitle.values()]
  const stories = crashedStories(view)
  const links = view.analysis.links
  const anomalies = view.analysis.anomalies
  const { unlinkedEvents, unlinkedLinks } = view.analysis.correlation

  const hidden = warnings.length + stories.length + links.length + anomalies.length
  const shown =
    Math.min(warnings.length, 12) +
    Math.min(stories.length, 40) +
    Math.min(links.length, 40) +
    Math.min(anomalies.length, 60)
  const truncated = !expanded && hidden > shown
  const nothing =
    stories.length === 0 && links.length === 0 && anomalies.length === 0 && warnings.length === 0

  // One renderer per row kind, used by both the first page and the expanded remainder: the
  // collapse mechanism is shared, so the rows must not be written out twice either.
  const renderStory = (story: CrashStory): JSX.Element => (
    <li key={story.identity}>
      <div className="dl-apps__item">
        <span className="dl-apps__name">
          {story.identity}
          {` · ${story.crashes.length} 次崩溃`}
          {story.victims.length > 0 ? ` · 连带 ${story.victims.length} 个进程` : ''}
        </span>
        <span className="dl-apps__package">
          {story.victims.length > 0
            ? story.victims.map((victim) => victim.victim).join('、')
            : '无连带受害者'}
        </span>
      </div>
    </li>
  )

  const renderLink = (link: CausalLink, index: number): JSX.Element => {
    // `evidence` holds the analyser's line numbers, so it must go through `seqAt` before it
    // means anything to the table. Jumping to the first evidence line is the honest choice: it
    // is the line that made the analyser connect the two.
    const first = link.evidence[0]
    const seq = first === undefined ? null : seqOf(view, first)
    return (
      <li key={`${link.source}->${link.victim}-${index}`}>
        <button
          type="button"
          className="dl-apps__item"
          disabled={seq === null}
          onClick={() => {
            if (seq !== null) {
              onJump(seq)
            }
          }}
          title={seq === null ? '这条关系没有可跳转的证据行' : `跳到证据记录 ${seq}`}
        >
          <span className="dl-apps__name">{linkLabel(link)}</span>
          <span className="dl-apps__package dl-mono">
            {`置信度 ${Math.round(link.confidence * 100)}%`}
            {seq === null ? '' : ` · 证据 ${seq}`}
          </span>
        </button>
      </li>
    )
  }

  const renderAnomaly = (anomaly: ResourceAnomaly, index: number): JSX.Element => {
    const seq = seqOf(view, anomaly.lineIndex)
    return (
      <li key={`${anomaly.kind}-${anomaly.lineIndex}-${index}`}>
        <button
          type="button"
          className="dl-apps__item"
          disabled={seq === null}
          onClick={() => {
            if (seq !== null) {
              onJump(seq)
            }
          }}
          title={seq === null ? '这条异常没有可跳转的日志行' : `跳回日志记录 ${seq}`}
        >
          <span className="dl-apps__name">{anomalyLabel(anomaly)}</span>
          <span className="dl-apps__package">
            {anomaly.detail}
            {seq === null ? '' : ` · 记录 ${seq}`}
          </span>
        </button>
      </li>
    )
  }

  return (
    <div className="dl-forensics">
      {nothing ? (
        <p className="dl-panel__hint">未发现崩溃、连带死亡或资源异常。</p>
      ) : null}

      {notice !== null ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">采集说明</span>
          <p className="dl-panel__hint">{notice}</p>
        </section>
      ) : null}

      {known.length > 0 ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">已知崩溃模式</span>
          <ul className="dl-apps" aria-label="已知崩溃模式">
            {known.map((entry) => (
              <li key={entry.note.title}>
                <div className="dl-apps__item">
                  <span className="dl-apps__name">
                    {entry.count > 1 ? `${entry.note.title} · 重复 ${entry.count} 次` : entry.note.title}
                  </span>
                  <span className="dl-apps__package">{`${entry.note.cause} ${entry.note.action}`}</span>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {warnings.length > 0 ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">完整性告警</span>
          <ul className="dl-apps" aria-label="完整性告警">
            {warnings.slice(0, 12).map((check) => (
              <CheckRow key={check.id} check={check} />
            ))}
          </ul>
          {/* The remainder arrives through the one collapse mechanism. It is a second list
              rather than a wrapper inside the first one: a `<div>` may not be a child of a
              `<ul>`, and the animation belongs to the *extra* rows, not to the first page. */}
          <Collapse open={expanded}>
            <ul className="dl-apps" aria-label="完整性告警（其余）">
              {warnings.slice(12).map((check) => (
                <CheckRow key={check.id} check={check} />
              ))}
            </ul>
          </Collapse>
        </section>
      ) : null}

      {stories.length > 0 ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">崩溃源与受害者</span>
          <ul className="dl-apps" aria-label="崩溃源与受害者">
            {stories.slice(0, 40).map(renderStory)}
          </ul>
          <Collapse open={expanded}>
            <ul className="dl-apps" aria-label="崩溃源与受害者（其余）">
              {stories.slice(40).map(renderStory)}
            </ul>
          </Collapse>
        </section>
      ) : null}

      {links.length > 0 ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">连带关系（源 → 受害者）</span>
          <ul className="dl-apps" aria-label="连带关系">
            {links.slice(0, 40).map(renderLink)}
          </ul>
          <Collapse open={expanded}>
            <ul className="dl-apps" aria-label="连带关系（其余）">
              {links.slice(40).map(renderLink)}
            </ul>
          </Collapse>
        </section>
      ) : null}

      {anomalies.length > 0 ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">资源异常</span>
          <ul className="dl-apps" aria-label="资源异常">
            {anomalies.slice(0, 60).map(renderAnomaly)}
          </ul>
          <Collapse open={expanded}>
            <ul className="dl-apps" aria-label="资源异常（其余）">
              {anomalies.slice(60).map(renderAnomaly)}
            </ul>
          </Collapse>
        </section>
      ) : null}

      {unlinkedEvents > 0 || unlinkedLinks > 0 ? (
        <section className="dl-forensics__group">
          <span className="dl-forensics__title">未归属</span>
          <p className="dl-panel__hint">
            {`${unlinkedEvents} 条事件、${unlinkedLinks} 条关系无法归属到具体进程。`}
          </p>
        </section>
      ) : null}
      {truncated ? (
        <button type="button" className="dl-panel__action" onClick={() => setExpanded(true)}>
          {`展开全部（还有 ${hidden - shown} 条）`}
        </button>
      ) : null}
    </div>
  )
}
