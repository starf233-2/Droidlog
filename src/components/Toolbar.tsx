/**
 * Top app bar: the capture control surface.
 *
 * Left to right: what is selected (device + collector), how it runs (Adb /
 * Root), then the session actions and the two theme controls. Status text sits
 * on the right so the actions never shift position as state changes.
 *
 * Several sessions can run at once, so `开始采集` *adds* one rather than
 * replacing; each running session is listed with its own stop control in the log
 * pane's footer, and `全部停止` is the single-call escape hatch.
 *
 * Built from the Material component library: `Toolbar` for the bar itself,
 * segmented button sets for the two mutually exclusive choices (exec mode, and
 * the light/dark/system preference), and a second segmented set for the colour
 * theme. Segmented sets are the M3 control for "pick exactly one of N", which is
 * what all three of those are — the previous hand-rolled buttons were doing the
 * same job with `aria-pressed` and no keyboard semantics.
 */

import type { JSX } from 'react'
// Imported from the package root rather than a group entry point: the library's
// `exports` map declares only *some* groups (there is no `./button-group` or
// `./snackbar` subpath), so the root is the only specifier that is guaranteed to
// resolve for every component.
import {
  FilledButton,
  OutlinedButton,
  OutlinedSegmentedButton,
  OutlinedSegmentedButtonSet,
  TextButton,
  Toolbar as MaterialToolbar,
} from 'material-expressive-react'

import { COLOR_THEMES } from '../theme'
import { useAppStore, type ThemeChoice } from '../store/useAppStore'
import { deviceAndroidLabel, deviceDisplayName, formatCount } from '../lib/format'
import type { ExecMode } from '../types'

const MODES: readonly { value: ExecMode; label: string }[] = [
  { value: 'adb', label: 'ADB' },
  { value: 'root', label: 'Root' },
]

const THEME_MODES: readonly { value: ThemeChoice; label: string }[] = [
  { value: 'light', label: '浅色' },
  { value: 'dark', label: '深色' },
  { value: 'system', label: '跟随系统' },
]

export function Toolbar(): JSX.Element {
  const selectedDevice = useAppStore((state) => state.selectedDevice())
  const mode = useAppStore((state) => state.mode)
  const setMode = useAppStore((state) => state.setMode)
  const selectedSource = useAppStore((state) => state.selectedSource)
  const sources = useAppStore((state) => state.sources)
  const sessions = useAppStore((state) => state.sessions)
  const startCapture = useAppStore((state) => state.startCapture)
  const stopAllSessions = useAppStore((state) => state.stopAllSessions)
  const clearRecords = useAppStore((state) => state.clearRecords)
  const recordCount = useAppStore((state) => state.recordCount)
  const droppedCount = useAppStore((state) => state.droppedCount)
  const customCommand = useAppStore((state) => state.customCommand)
  const crashes = useAppStore((state) => state.crashes)
  const theme = useAppStore((state) => state.theme)
  const setTheme = useAppStore((state) => state.setTheme)
  const colorTheme = useAppStore((state) => state.colorTheme)
  const setColorTheme = useAppStore((state) => state.setColorTheme)

  const running = sessions.filter((session) => session.status.state === 'running')
  const selectedEntry =
    sources.find((entry) => entry.spec.kind === selectedSource) ?? null
  // Root mode is only offered when the device can escalate.
  const rootPossible = selectedDevice?.rootAvailable !== false
  const hasCustomCommand = customCommand.trim().length > 0

  return (
    <MaterialToolbar
      className="dl-toolbar"
      variant="Docked"
      dockPosition="Top"
      size="Small"
    >
      <div className="dl-toolbar__left">
        <span className="dl-toolbar__brand">Droidlog</span>

        <div className="dl-toolbar__context">
          {selectedDevice === null ? (
            <span className="dl-toolbar__hint">未选择设备</span>
          ) : (
            <>
              <span className="dl-toolbar__device">
                {deviceDisplayName(selectedDevice)}
              </span>
              <span className="dl-toolbar__serial dl-mono">
                {selectedDevice.serial}
              </span>
              {deviceAndroidLabel(selectedDevice) !== null ? (
                <span className="dl-chip">
                  {deviceAndroidLabel(selectedDevice)}
                </span>
              ) : null}
              {selectedDevice.recovery ? (
                <span
                  className="dl-chip dl-chip--recovery"
                  title="设备处于 Recovery／Sideload 模式：没有 logcat，可采集 Recovery 日志"
                >
                  Recovery 模式
                </span>
              ) : null}
            </>
          )}
        </div>

        <OutlinedSegmentedButtonSet
          className="dl-segmented"
          selectType="single"
          size="xsmall"
          // A check glyph rather than the default Material icon name: the icon
          // font is not bundled (the app must work offline), and the wrapper
          // renders whatever string it is given inside an <md-icon>.
          selectedIcon="✓"
          value={mode}
          onChange={(value) => {
            const next = Array.isArray(value) ? value[0] : value
            if (next === 'adb' || next === 'root') {
              setMode(next)
            }
          }}
          aria-label="执行模式"
        >
          {MODES.map((option) => (
            <OutlinedSegmentedButton
              key={option.value}
              value={option.value}
              label={option.label}
              disabled={option.value === 'root' && !rootPossible}
            />
          ))}
        </OutlinedSegmentedButtonSet>

        <span className="dl-chip dl-chip--accent">
          {selectedEntry?.spec.label ?? selectedSource}
        </span>

        {hasCustomCommand ? (
          <span className="dl-chip dl-chip--warn" title={customCommand}>
            自定义命令
          </span>
        ) : null}

        {crashes.length > 0 ? (
          <span
            className="dl-chip dl-chip--crash"
            title="已识别的崩溃日志条数：可在日志面板「仅看崩溃」中查看"
          >
            崩溃 {crashes.length}
          </span>
        ) : null}
      </div>

      <div className="dl-toolbar__right">
        <span className="dl-toolbar__stats dl-mono">
          {running.length > 0 ? `采集中 ${running.length} · ` : ''}
          行 {formatCount(recordCount)}
          {droppedCount > 0 ? ` · 已丢弃 ${formatCount(droppedCount)}` : ''}
        </span>

        <TextButton onClick={clearRecords} disabled={recordCount === 0}>
          清空
        </TextButton>

        <div className="dl-toolbar__themes">
          <OutlinedSegmentedButtonSet
            className="dl-segmented"
            selectType="single"
            size="xsmall"
            selectedIcon="✓"
            value={theme}
            onChange={(value) => {
              const next = Array.isArray(value) ? value[0] : value
              if (next === 'light' || next === 'dark' || next === 'system') {
                setTheme(next)
              }
            }}
            aria-label="明暗模式"
          >
            {THEME_MODES.map((option) => (
              <OutlinedSegmentedButton
                key={option.value}
                value={option.value}
                label={option.label}
              />
            ))}
          </OutlinedSegmentedButtonSet>

          <OutlinedSegmentedButtonSet
            className="dl-segmented"
            selectType="single"
            size="xsmall"
            selectedIcon="✓"
            value={colorTheme}
            onChange={(value) => {
              const next = Array.isArray(value) ? value[0] : value
              const match = COLOR_THEMES.find((entry) => entry.id === next)
              if (match !== undefined) {
                setColorTheme(match.id)
              }
            }}
            aria-label="色彩主题"
          >
            {COLOR_THEMES.map((entry) => (
              <OutlinedSegmentedButton
                key={entry.id}
                value={entry.id}
                label={entry.label}
              />
            ))}
          </OutlinedSegmentedButtonSet>
        </div>

        {running.length > 0 ? (
          <OutlinedButton onClick={() => void stopAllSessions()}>
            全部停止
          </OutlinedButton>
        ) : null}

        <FilledButton
          onClick={() => void startCapture()}
          disabled={selectedDevice === null || selectedEntry?.available === false}
          title="可以同时开启多个设备／采集源"
        >
          开始采集
        </FilledButton>
      </div>
    </MaterialToolbar>
  )
}
