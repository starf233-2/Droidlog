/**
 * Left rail: device selection and collector selection.
 *
 * Two stacked sections, no tabs: the user picks a device, then what to capture
 * from it. Root-gated collectors stay visible when unavailable, with the reason
 * shown inline, so the fix (switch to Root mode) is discoverable.
 *
 * The device and collector rows stay hand-built markup rather than becoming an
 * M3 list item: both are *composite* rows (a status dot, a title, a serial, and
 * a row of capability chips), and the library's `IconListItem` accepts only a
 * headline plus a support line — it cannot carry the chips, and its leading and
 * trailing icons are Material Symbols ligature names that would render as
 * literal words here, because this app deliberately bundles no icon font.
 * The controls around the rows do come from the library.
 */

import type { JSX } from 'react'
import { TextButton, Textfield } from 'material-expressive-react'
import type { MdOutlinedTextField } from '@material/web/textfield/outlined-text-field.js'

import type { DeviceInfo, SourceAvailability } from '../types'
import { useAppStore } from '../store/useAppStore'
import { deviceAndroidLabel, deviceDisplayName, formatCount } from '../lib/format'

/** Colour token name per device state. */
function deviceStateTone(state: DeviceInfo['state']): string {
  switch (state) {
    case 'device':
      return 'var(--dl-level-info)'
    case 'offline':
      return 'var(--dl-level-warn)'
    case 'unauthorized':
      return 'var(--dl-level-error)'
    default:
      return 'var(--dl-level-unknown)'
  }
}

/** Human label for a device state. */
function deviceStateLabel(device: DeviceInfo): string {
  switch (device.state) {
    case 'device':
      return '已连接'
    case 'offline':
      return '离线'
    case 'unauthorized':
      return '未授权'
    case 'bootloader':
      return 'bootloader'
    case 'recovery':
      return 'recovery'
    case 'sideload':
      return 'sideload'
    default:
      return device.stateRaw
  }
}

function DeviceRow({ device }: { device: DeviceInfo }): JSX.Element {
  const selectedSerial = useAppStore((state) => state.selectedSerial)
  const selectDevice = useAppStore((state) => state.selectDevice)
  const selected = device.serial === selectedSerial
  const androidLabel = deviceAndroidLabel(device)

  return (
    <button
      type="button"
      className="dl-device"
      aria-pressed={selected}
      onClick={() => selectDevice(device.serial)}
    >
      <span
        className="dl-device__dot"
        style={{ backgroundColor: deviceStateTone(device.state) }}
        aria-hidden="true"
      />
      <span className="dl-device__body">
        <span className="dl-device__name">{deviceDisplayName(device)}</span>
        <span className="dl-device__meta dl-mono">{device.serial}</span>
        <span className="dl-device__tags">
          <span className="dl-chip">{deviceStateLabel(device)}</span>
          {androidLabel !== null ? (
            <span className="dl-chip">{androidLabel}</span>
          ) : null}
          {device.rootAvailable === true ? (
            <span className="dl-chip dl-chip--accent">root</span>
          ) : null}
          {device.rootAvailable === false ? (
            <span className="dl-chip dl-chip--warn">无 root</span>
          ) : null}
        </span>
      </span>
    </button>
  )
}

function SourceRow({ entry }: { entry: SourceAvailability }): JSX.Element {
  const selectedSource = useAppStore((state) => state.selectedSource)
  const selectSource = useAppStore((state) => state.selectSource)
  const selected = entry.spec.kind === selectedSource

  return (
    <button
      type="button"
      className="dl-source"
      aria-pressed={selected}
      // Root-gated collectors are refused until Root mode is on: the row greys
      // out and the click does nothing, with the reason spelled out below.
      disabled={!entry.available}
      onClick={() => selectSource(entry.spec.kind)}
    >
      <span className="dl-source__head">
        <span className="dl-source__label">{entry.spec.label}</span>
        {entry.spec.requiresRoot ? (
          <span className="dl-chip dl-chip--warn">root</span>
        ) : null}
        <span className="dl-chip" title="解析器类型">
          {entry.spec.parser}
        </span>
      </span>
      <span className="dl-source__desc">{entry.spec.description}</span>
      <span className="dl-source__cmd dl-mono">{entry.spec.defaultCommand}</span>
      {entry.available ? null : (
        <span className="dl-source__blocked">{entry.unavailableReason}</span>
      )}
    </button>
  )
}

/**
 * Device-side command override.
 *
 * The parser stays the selected source's own, so a custom command only makes
 * sense when its output matches that grammar — the hint says so rather than
 * pretending any command can be decoded.
 */
function CustomCommandField(): JSX.Element | null {
  const selectedSource = useAppStore((state) => state.selectedSource)
  const sources = useAppStore((state) => state.sources)
  const customCommand = useAppStore((state) => state.customCommand)
  const setCustomCommand = useAppStore((state) => state.setCustomCommand)

  const entry = sources.find((item) => item.spec.kind === selectedSource) ?? null
  if (entry === null || !entry.spec.supportsCustomCommand) {
    return null
  }

  return (
    <div className="dl-customcmd">
      <Textfield
        id="dl-custom-command"
        className="dl-customcmd__input"
        variant="outlined"
        label="自定义命令（留空则用内置）"
        value={customCommand}
        placeholder={entry.spec.defaultCommand}
        onChange={(event: Event) =>
          setCustomCommand((event.target as MdOutlinedTextField).value)
        }
      />
      <span className="dl-customcmd__hint">
        仍按「{entry.spec.parser}」解析；不匹配的行会作为原始行保留。
      </span>
    </div>
  )
}

export function DevicePanel(): JSX.Element {
  const devices = useAppStore((state) => state.devices)
  const devicesLoading = useAppStore((state) => state.devicesLoading)
  const devicesError = useAppStore((state) => state.devicesError)
  const sources = useAppStore((state) => state.sources)
  const refreshDevices = useAppStore((state) => state.refreshDevices)
  const adb = useAppStore((state) => state.adb)
  const adbLoading = useAppStore((state) => state.adbLoading)

  const unauthorized = devices.filter(
    (device) => device.state === 'unauthorized',
  ).length
  const adbMissing = adb !== null && !adb.available

  return (
    <aside className="dl-rail" aria-label="设备与采集源">
      {/* ------------------------------------------------------------ devices */}
      <section className="dl-rail__section">
        <header className="dl-rail__header">
          <h2 className="dl-rail__title">设备</h2>
          <TextButton
            className="dl-rail__action"
            onClick={() => void refreshDevices()}
            disabled={devicesLoading || adbMissing}
            title="自动每 2 秒刷新；点击可立即重新探测版本与 Root 状态"
          >
            {devicesLoading ? '扫描中' : '重新探测'}
          </TextButton>
        </header>

        <div className="dl-rail__body dl-scroll">
          {adbMissing ? (
            <div className="dl-empty">
              <span className="dl-empty__title">未找到 adb</span>
              <span className="dl-empty__hint">
                请安装 Android platform-tools，或设置环境变量 DROIDLOG_ADB 指向 adb
                可执行文件。
              </span>
              {adb.error !== null ? (
                <span className="dl-empty__hint dl-mono">{adb.error}</span>
              ) : null}
            </div>
          ) : devices.length === 0 ? (
            <div className="dl-empty">
              <span className="dl-empty__title">
                {adbLoading || devicesLoading ? '正在扫描设备' : '没有可用设备'}
              </span>
              <span className="dl-empty__hint">
                用 USB 连接手机并开启「USB 调试」，设备会自动出现。
              </span>
              {devicesError !== null ? (
                <span className="dl-empty__hint dl-mono">{devicesError}</span>
              ) : null}
            </div>
          ) : (
            <div className="dl-list">
              {devices.map((device) => (
                <DeviceRow key={device.serial} device={device} />
              ))}
            </div>
          )}
        </div>

        {unauthorized > 0 ? (
          <p className="dl-rail__note">
            {formatCount(unauthorized)} 台设备未授权：请在手机上确认 USB 调试授权弹窗。
          </p>
        ) : null}
      </section>

      <div className="dl-divider dl-divider--horizontal" />

      {/* ------------------------------------------------------------ sources */}
      <section className="dl-rail__section dl-rail__section--grow">
        <header className="dl-rail__header">
          <h2 className="dl-rail__title">采集源</h2>
        </header>
        <div className="dl-rail__body dl-scroll">
          {sources.length === 0 ? (
            <div className="dl-empty">
              <span className="dl-empty__title">尚未解析采集源</span>
              <span className="dl-empty__hint">先选择一个设备。</span>
            </div>
          ) : (
            <div className="dl-list">
              {sources.map((entry) => (
                <SourceRow key={entry.spec.kind} entry={entry} />
              ))}
            </div>
          )}
          <CustomCommandField />
        </div>
      </section>
    </aside>
  )
}
