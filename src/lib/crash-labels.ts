/**
 * Human labels for the crash classifier and the collection report.
 *
 * The backend sends stable identifiers (`systemServer`, `denied`, …) and never
 * display text, so the mapping lives here — one place to change a word, and no
 * chance of a backend string leaking into the UI.
 */

import type { CrashKind, LogSourceKind, ProbeStatus } from '../types'

/** Chinese label per failure family. */
const CRASH_LABELS: Record<CrashKind, string> = {
  kernelPanic: '内核恐慌',
  oops: '内核异常',
  kernelBug: '内核断言',
  watchdog: '看门狗',
  lowMemory: '内存不足',
  anr: 'ANR 无响应',
  systemServer: '系统服务',
  nativeCrash: 'Native 崩溃',
}

/**
 * Two-character badge per family, for the row marker.
 *
 * The badge sits in a 22 px row, so a full word would either wrap or force the
 * row height up; the title attribute carries the readable name.
 */
const CRASH_BADGES: Record<CrashKind, string> = {
  kernelPanic: 'KP',
  oops: 'OP',
  kernelBug: 'BG',
  watchdog: 'WD',
  lowMemory: 'LM',
  anr: 'A',
  systemServer: 'SS',
  nativeCrash: 'NC',
}

/** Chinese label per probe status. */
const PROBE_LABELS: Record<ProbeStatus, string> = {
  found: '已找到',
  empty: '为空',
  missing: '不存在',
  denied: '无权限',
  restricted: '内核受限',
  failed: '失败',
}

/** Chinese label per collector, used in report and session summaries. */
const SOURCE_LABELS: Record<LogSourceKind, string> = {
  logcat: 'logcat',
  dmesg: 'dmesg',
  kmsg: 'kmsg',
  crash: '崩溃日志',
  boot: '启动日志',
  recovery: 'Recovery 日志',
}

/** Readable name of a failure family. */
export function crashKindLabel(kind: CrashKind): string {
  return CRASH_LABELS[kind]
}

/** Short badge for the row marker. */
export function crashKindBadge(kind: CrashKind): string {
  return CRASH_BADGES[kind]
}

/** Readable name of a probe status. */
export function probeStatusLabel(status: ProbeStatus): string {
  return PROBE_LABELS[status]
}

/** Readable name of a collector. */
export function sourceLabel(source: LogSourceKind): string {
  return SOURCE_LABELS[source]
}

/**
 * Formats the time left on a bounded collection.
 *
 * Returns `null` once the deadline has passed, so the caller can drop the
 * countdown instead of showing `0 秒` forever — the status event that ends the
 * session may arrive a moment later.
 */
export function formatRemaining(endsAtMs: number, nowMs: number): string | null {
  const remaining = endsAtMs - nowMs
  if (remaining <= 0) {
    return null
  }
  const seconds = Math.ceil(remaining / 1000)
  if (seconds < 60) {
    return `${seconds} 秒`
  }
  const minutes = Math.floor(seconds / 60)
  return `${minutes} 分 ${String(seconds % 60).padStart(2, '0')} 秒`
}
