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

/**
 * Chinese labels for the system packages whose last name segment is not a word.
 *
 * The device cannot hand over an application's display name: `dumpsys package`
 * only exposes `labelRes` (a resource id), and the text lives in the APK's
 * `resources.arsc`. Until that is parsed on the host, the name shown for a
 * running app is either this table (for the system apps everyone recognises) or a
 * name derived from the package — and the package itself is always displayed
 * underneath, so nothing is hidden by the guess.
 */
const SYSTEM_APP_NAMES: Record<string, string> = {
  'com.android.settings': '设置',
  'com.android.phone': '电话',
  'com.android.dialer': '拨号',
  'com.android.contacts': '联系人',
  'com.android.mms': '信息',
  'com.android.messaging': '信息',
  'com.android.camera': '相机',
  'com.android.camera2': '相机',
  'com.android.gallery3d': '相册',
  'com.android.documentsui': '文件',
  'com.android.settings.intelligence': '设置建议',
  'com.android.systemui': '系统界面',
  'com.android.launcher': '桌面',
  'com.android.launcher3': '桌面',
  'com.android.bluetooth': '蓝牙',
  'com.android.shell': 'Shell',
  'com.android.providers.settings': '设置存储',
  'com.android.vending': '应用商店',
  'com.google.android.gms': 'Google 服务',
  'com.google.android.gsf': 'Google 服务框架',
  'com.android.chrome': 'Chrome',
  'com.android.calendar': '日历',
  'com.android.deskclock': '时钟',
  'com.android.music': '音乐',
  'com.android.email': '电子邮件',
  'com.miui.home': '桌面',
  'com.miui.securitycenter': '安全中心',
  'com.miui.gallery': '相册',
  'com.miui.calculator': '计算器',
  'com.miui.weather2': '天气',
}

/**
 * A readable name for a package.
 *
 * `com.android.settings` → `设置`, `tv.danmaku.bili` → `Bili`. Never a substitute
 * for the real label: it is a display convenience, which is why the callers show
 * the package next to it.
 */
export function appDisplayName(pkg: string): string {
  const known = SYSTEM_APP_NAMES[pkg]
  if (known !== undefined) {
    return known
  }
  const segments = pkg.split('.').filter((segment) => segment.length > 0)
  const last = segments.length > 0 ? (segments[segments.length - 1] ?? pkg) : pkg
  // `bilibili` and `bili` should not read as `Bilibili`/`Bili` alike; keeping the
  // segment as-is (only capitalising) at least matches the store listing.
  return last.charAt(0).toUpperCase() + last.slice(1)
}
