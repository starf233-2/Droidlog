/**
 * Collection report: what a crash, boot or recovery run actually found.
 *
 * The panel exists because an empty log table is ambiguous. A device that has
 * not crashed since boot, a tombstone directory that needs root, and a kernel
 * ring restricted by `dmesg_restrict` all look identical — no rows — but call
 * for three different actions. So every probe reports its own diagnosis, and the
 * remedies are listed verbatim from the backend.
 *
 * It is a side panel rather than a dialog: a report is something to read while
 * looking at the logs it describes, not a modal that blocks them.
 */

import type { JSX } from 'react'
import { TextButton } from 'material-expressive-react'

import { useAppStore } from '../store/useAppStore'
import {
  crashKindLabel,
  probeStatusLabel,
  sourceLabel,
} from '../lib/crash-labels'
import type { ProbeOutcome, ProbeStatus } from '../types'

/** Status classes, so the stylesheet can colour the chip by severity alone. */
const STATUS_TONE: Record<ProbeStatus, string> = {
  found: 'dl-probe--ok',
  empty: 'dl-probe--idle',
  missing: 'dl-probe--idle',
  denied: 'dl-probe--warn',
  restricted: 'dl-probe--warn',
  failed: 'dl-probe--bad',
}

function ProbeRow({ outcome }: { outcome: ProbeOutcome }): JSX.Element {
  return (
    <li className={`dl-probe ${STATUS_TONE[outcome.status]}`}>
      <div className="dl-probe__head">
        <span className="dl-probe__label">{outcome.label}</span>
        <span className="dl-probe__status">{probeStatusLabel(outcome.status)}</span>
      </div>
      <div className="dl-probe__meta">
        <span className="dl-mono">{outcome.records} 条</span>
        {outcome.files.length > 0 ? (
          <span className="dl-mono" title={outcome.files.join(', ')}>
            · 文件 {outcome.files.length}
          </span>
        ) : null}
      </div>
      {outcome.detail !== null ? (
        <p className="dl-probe__detail">{outcome.detail}</p>
      ) : null}
      {outcome.hint !== null ? (
        <p className="dl-probe__hint">{outcome.hint}</p>
      ) : null}
      <code className="dl-probe__command dl-mono" title={outcome.command}>
        {outcome.command}
      </code>
    </li>
  )
}

export function CollectPanel(): JSX.Element | null {
  const report = useAppStore((state) => state.activeReport())
  const selectReport = useAppStore((state) => state.selectReport)
  const crashes = useAppStore((state) => state.crashes)
  const requestJump = useAppStore((state) => state.requestJump)

  if (report === null) {
    return null
  }

  return (
    <aside className="dl-report" aria-label="采集报告">
      <header className="dl-report__head">
        <span className="dl-report__title">
          {sourceLabel(report.source)}采集报告
        </span>
        <TextButton
          className="dl-report__close"
          aria-label="关闭采集报告"
          onClick={() => selectReport(null)}
        >
          ✕
        </TextButton>
      </header>

      <p className="dl-report__summary">
        {report.failure !== null
          ? report.failure
          : `找到 ${report.sourcesFound} 个日志源，共采集 ${report.records} 条记录`}
      </p>

      <ul className="dl-report__probes">
        {report.outcomes.map((outcome) => (
          <ProbeRow key={outcome.id} outcome={outcome} />
        ))}
      </ul>

      {crashes.length > 0 ? (
        <section className="dl-report__crashes">
          <span className="dl-report__subtitle">
            识别到的崩溃（{crashes.length}）
          </span>
          <ul className="dl-report__crash-list">
            {crashes.map((entry) => (
              <li key={entry.seq} className="dl-report__crash-item">
                <span className="dl-report__crash-kind">
                  {crashKindLabel(entry.kind)}
                </span>
                <span className="dl-mono dl-report__crash-seq">#{entry.seq}</span>
                <TextButton
                  className="dl-report__crash-jump"
                  onClick={() => requestJump(entry.seq)}
                >
                  定位
                </TextButton>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </aside>
  )
}
