/**
 * Serialising the render buffer into the three export formats.
 *
 * Kept out of the store so the formats can be unit-tested on plain data, and out
 * of Rust so there is exactly one definition of what an exported line looks like
 * (`write_export` only decides where the bytes land and whether they need a BOM).
 */

import type { ExportFormat, LogLevel, LogRow } from '../types'

/** logcat's own level letters, plus `?` for rows the parser could not classify. */
const LEVEL_LETTER: Record<LogLevel, string> = {
  verbose: 'V',
  debug: 'D',
  info: 'I',
  warn: 'W',
  error: 'E',
  fatal: 'F',
  unknown: '?',
}

/** Column order of the CSV/JSON exports, so both agree with each other. */
const FIELDS = [
  'seq',
  'timestamp',
  'level',
  'pid',
  'tid',
  'uid',
  'tag',
  'package',
  'source',
  'message',
] as const

/**
 * One row as a logcat-ish line.
 *
 * Unparsed rows are written verbatim: they are frequently the only copy of a
 * vendor-specific format, and reflowing them through empty fields would destroy
 * the very thing worth keeping.
 */
function logLine(row: LogRow): string {
  if (!row.parsed) {
    return row.raw.length > 0 ? row.raw : row.message
  }
  const time = row.timestamp ?? ''.padEnd(18, ' ')
  const pid = String(row.pid ?? '').padStart(5, ' ')
  const tid = String(row.tid ?? '').padStart(5, ' ')
  const letter = LEVEL_LETTER[row.level]
  const tag = row.tag ?? ''
  return `${time} ${pid} ${tid} ${letter} ${tag}: ${row.message}`
}

/** The whole buffer as plain text, oldest first. */
export function recordsToLog(rows: readonly LogRow[]): string {
  return rows.map(logLine).join('\n') + (rows.length > 0 ? '\n' : '')
}

/** RFC 4180 quoting: only what needs it, with quotes doubled. */
function csvField(value: string): string {
  const needsQuotes = /[",\r\n]/.test(value) || value !== value.trim()
  return needsQuotes ? `"${value.replace(/"/g, '""')}"` : value
}

function csvValue(row: LogRow, field: (typeof FIELDS)[number]): string {
  switch (field) {
    case 'seq':
      return String(row.seq)
    case 'pid':
      return row.pid === null ? '' : String(row.pid)
    case 'tid':
      return row.tid === null ? '' : String(row.tid)
    case 'uid':
      return row.uid === null ? '' : String(row.uid)
    case 'timestamp':
      return row.timestamp ?? ''
    case 'tag':
      return row.tag ?? ''
    case 'package':
      return row.package ?? ''
    case 'level':
      return row.level
    case 'source':
      return row.source
    case 'message':
      return row.parsed ? row.message : row.raw || row.message
    default:
      return ''
  }
}

/**
 * The buffer as CSV.
 *
 * No BOM here: the BOM is added by the Rust side when it writes the file, so a
 * string handed to something else never carries an invisible character.
 */
export function recordsToCsv(rows: readonly LogRow[]): string {
  const header = FIELDS.join(',')
  const lines = rows.map((row) => FIELDS.map((field) => csvField(csvValue(row, field))).join(','))
  return [header, ...lines].join('\n') + '\n'
}

/** One JSON object per line inside an array: valid JSON, and still greppable. */
export function recordsToJson(rows: readonly LogRow[]): string {
  const objects = rows.map((row) => {
    const entry: Record<string, unknown> = {}
    for (const field of FIELDS) {
      entry[field] = csvValue(row, field)
    }
    entry['receivedAtMs'] = row.receivedAtMs
    entry['sessionId'] = row.sessionId
    return JSON.stringify(entry)
  })
  return `[\n${objects.join(',\n')}\n]\n`
}

/** Serialises `rows` in `format`. */
export function serialize(rows: readonly LogRow[], format: ExportFormat): string {
  switch (format) {
    case 'csv':
      return recordsToCsv(rows)
    case 'json':
      return recordsToJson(rows)
    case 'log':
    default:
      return recordsToLog(rows)
  }
}

/**
 * A local-time, sortable file name: `droidlog-20260925-190432.log`.
 *
 * Built in the frontend because `Date` already knows the local time zone, which
 * is what the user will match the file against; Rust only sanitises whatever
 * arrives and forces the extension.
 */
export function exportFileName(format: ExportFormat, now: Date = new Date()): string {
  const pad = (value: number): string => String(value).padStart(2, '0')
  const stamp =
    `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}` +
    `-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`
  return `droidlog-${stamp}.${format}`
}
