/**
 * Presentation helpers.
 *
 * The Rust side exposes behaviour as methods (`display_name`, `android_label`),
 * which do not cross the IPC boundary — only data does. These functions rebuild
 * that presentation logic on the TypeScript side.
 */

import type { DeviceInfo, LogLevel, LogRecord } from '../types'

/** Thousands separators for counts shown in chrome. */
export function formatCount(value: number): string {
  return value.toLocaleString('en-US')
}

/** Best available device name: model, else serial. */
export function deviceDisplayName(device: DeviceInfo): string {
  return device.model !== null && device.model.length > 0
    ? device.model
    : device.serial
}

/** `Android 14 (SDK 34)` when both parts are known, else the partial form. */
export function deviceAndroidLabel(device: DeviceInfo): string | null {
  const { androidVersion, sdk } = device
  if (androidVersion !== null && sdk !== null) {
    return `Android ${androidVersion} (SDK ${sdk})`
  }
  if (androidVersion !== null) {
    return `Android ${androidVersion}`
  }
  if (sdk !== null) {
    return `SDK ${sdk}`
  }
  return null
}

/** Single-letter severity badge used by the table. */
export function levelBadge(level: LogLevel): string {
  switch (level) {
    case 'verbose':
      return 'V'
    case 'debug':
      return 'D'
    case 'info':
      return 'I'
    case 'warn':
      return 'W'
    case 'error':
      return 'E'
    case 'fatal':
      return 'F'
    default:
      return '?'
  }
}

/** Human label for a severity, used in the filter panel. */
export function levelLabel(level: LogLevel): string {
  switch (level) {
    case 'verbose':
      return 'Verbose'
    case 'debug':
      return 'Debug'
    case 'info':
      return 'Info'
    case 'warn':
      return 'Warn'
    case 'error':
      return 'Error'
    case 'fatal':
      return 'Fatal'
    default:
      return '未知'
  }
}

/** Every severity, least to most severe, for select controls. */
export const LOG_LEVELS: readonly LogLevel[] = [
  'verbose',
  'debug',
  'info',
  'warn',
  'error',
  'fatal',
]

/**
 * Timestamp column text.
 *
 * logcat supplies `MM-DD HH:MM:SS.mmm`; dmesg/kmsg supply kernel uptime instead,
 * which is rendered as `[1234.567890]` so the two are never confused.
 */
export function formatTimestamp(record: LogRecord): string {
  if (record.timestamp !== null) {
    return record.timestamp
  }
  if (record.uptimeSeconds !== null) {
    return `[${record.uptimeSeconds.toFixed(6)}]`
  }
  return '-'
}

/** Renders an optional number field, using `-` for absent values. */
export function formatOptionalNumber(value: number | null): string {
  return value === null ? '-' : value.toString()
}

/** Truncates a long message for the single-line table cell. */
export function formatMessage(message: string): string {
  return message.length === 0 ? '(空)' : message
}
