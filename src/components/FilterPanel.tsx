/**
 * Right pane: what gets captured, expressed as controls rather than as a table
 * of rules.
 *
 * Two layers, deliberately:
 *
 * * the **target application** is a backend concept (`set_app_target`), not a
 *   rule — the backend owns resolving it and re-resolving it every 30 seconds,
 *   because a pid list goes stale the moment the app restarts;
 * * **level / tag / keyword / window** are structured controls that compile into
 *   the same generic rule list the engine already evaluates (see
 *   `compileStructuredFilters`), so the backend never grows one concept per
 *   control while the UI still presents one control per concept.
 *
 * Controls come from the Material component library: outlined text fields for
 * text, filter chips for the level set, a switch for the regex toggle, outlined
 * selects for the enumerations, and a segmented set for the time-window presets.
 * Everything stays M3 *outlined* — transparent surface, one hairline outline, no
 * fills, no gradients.
 *
 * Values are read from the elements' own properties, which is what the wrappers
 * assign: `event.target` is the custom element, not an `<input>`.
 */

import type { JSX } from 'react'
import { useEffect, useMemo, useState } from 'react'
import {
  FilterChip,
  OutlinedButton,
  OutlinedSelect,
  SelectOption,
  Switch,
  TextButton,
  Textfield,
  OutlinedSegmentedButton,
  OutlinedSegmentedButtonSet,
} from 'material-expressive-react'
import type { MdOutlinedSelect } from '@material/web/select/outlined-select.js'
import type { MdOutlinedTextField } from '@material/web/textfield/outlined-text-field.js'
import type { MdSwitch } from '@material/web/switch/switch.js'

import { useAppStore } from '../store/useAppStore'
import { reserveMenuScrollbar } from '../lib/shadow-fixes'
import { LOG_LEVELS, appDisplayName, levelLabel } from '../lib/format'
import type { FilterField, FilterOp, FilterRule, InstalledApp, LogLevel } from '../types'

const FIELDS: readonly { value: FilterField; label: string }[] = [
  { value: 'tag', label: 'TAG' },
  { value: 'message', label: '消息' },
  { value: 'pid', label: 'PID' },
  { value: 'tid', label: 'TID' },
  { value: 'uid', label: 'UID' },
  { value: 'package', label: '包名' },
  { value: 'level', label: '级别' },
  { value: 'received', label: '接收时间' },
  { value: 'source', label: '采集源' },
]

const OPS: readonly { value: FilterOp; label: string }[] = [
  { value: 'contains', label: '包含' },
  { value: 'notContains', label: '不包含' },
  { value: 'equals', label: '等于' },
  { value: 'notEquals', label: '不等于' },
  { value: 'regex', label: '正则' },
  { value: 'minLevel', label: '不低于' },
  { value: 'in', label: '属于（逗号分隔）' },
  { value: 'withinLast', label: '最近 N 秒' },
]

/** Time-window presets. `none` is the "no window" choice. */
const WINDOW_PRESETS: readonly { value: string; label: string }[] = [
  { value: 'none', label: '不限' },
  { value: '10', label: '10s' },
  { value: '30', label: '30s' },
  { value: '60', label: '60s' },
]

/** True when the operator compares against a log level rather than free text. */
function isLevelOp(op: FilterOp): boolean {
  return op === 'minLevel'
}

/** True when the operator takes a number of seconds. */
function isWindowOp(op: FilterOp): boolean {
  return op === 'withinLast'
}

let ruleCounter = 0
function nextRuleId(): string {
  ruleCounter += 1
  return `rule-${Date.now().toString(36)}-${ruleCounter}`
}

/* -------------------------------------------------------------------------- */
/* Target application                                                         */
/* -------------------------------------------------------------------------- */

function TargetSection(): JSX.Element {
  /*
    Whether the running-apps list is expanded.

    Local to this card rather than a store field: only this card renders the list,
    and it is the one part of the card that grows without bound (one row per process
    on the device), so folding it away has to be possible. Defaults to expanded —
    the behaviour before there was a control for it.
  */
  const [runningAppsOpen, setRunningAppsOpen] = useState(true)

  const input = useAppStore((state) => state.appTargetInput)
  const setInput = useAppStore((state) => state.setAppTargetInput)
  const target = useAppStore((state) => state.appTarget)
  const resolve = useAppStore((state) => state.resolveAppTarget)
  const clear = useAppStore((state) => state.clearAppTarget)
  const runningApps = useAppStore((state) => state.runningApps)
  const refreshRunning = useAppStore((state) => state.refreshRunningApps)
  const selectedSerial = useAppStore((state) => state.selectedSerial)
  const watching = useAppStore((state) => state.watching)
  const setWatching = useAppStore((state) => state.setWatching)
  const installedApps = useAppStore((state) => state.installedApps)
  const installedAppsLoading = useAppStore((state) => state.installedAppsLoading)
  const installedAppsError = useAppStore((state) => state.installedAppsError)
  const installedAppsOpen = useAppStore((state) => state.installedAppsOpen)
  const setInstalledAppsOpen = useAppStore((state) => state.setInstalledAppsOpen)
  const installedAppsQuery = useAppStore((state) => state.installedAppsQuery)
  const setInstalledAppsQuery = useAppStore(
    (state) => state.setInstalledAppsQuery,
  )
  const showSystemApps = useAppStore((state) => state.showSystemApps)
  const setShowSystemApps = useAppStore((state) => state.setShowSystemApps)
  const loadInstalledApps = useAppStore((state) => state.loadInstalledApps)

  /*
    Device-resolved names for the *running* list, keyed by package.

    The installed-app dump is the only place a real display name exists, and it is
    read only when the picker has been used — hence the package-derived fallback.
    A map rather than a lookup per row: the running list is re-rendered whenever a
    record arrives, and scanning the whole installed list each time would be O(rows
    × installed).
  */
  const installedLabels = useMemo(() => {
    const labels = new Map<string, string>()
    for (const app of installedApps) {
      if (app.label.length > 0) {
        labels.set(app.package, app.label)
      }
    }
    return labels
  }, [installedApps])

  // The system filter is applied here too, not only on the device: the rows from
  // the previous read stay on screen while a fresh one is in flight, and they must
  // not contradict the toggle the user just flipped.
  const query = installedAppsQuery.trim().toLowerCase()
  const visibleInstalledApps = installedApps.filter((app) => {
    if (!showSystemApps && app.system) {
      return false
    }
    if (query.length === 0) {
      return true
    }
    return (
      app.label.toLowerCase().includes(query) ||
      app.package.toLowerCase().includes(query)
    )
  })

  const submit = (): void => void resolve()

  const toggleInstalledApps = (): void => {
    const open = !installedAppsOpen
    setInstalledAppsOpen(open)
    if (open) {
      // Re-read on every open: the list belongs to one device, and the backend
      // answers from its host-side cache unless the device changed.
      void loadInstalledApps()
    }
  }

  return (
    <section className="dl-panel__section">
      <h3 className="dl-panel__label">目标应用</h3>

      <div className="dl-target__row">
        <Textfield
          className="dl-target__input"
          variant="outlined"
          label="输入 PID / 包名 / UID"
          value={input}
          disabled={selectedSerial === null}
          onChange={(event: Event) =>
            setInput((event.target as MdOutlinedTextField).value)
          }
          onKeyDown={(event: React.KeyboardEvent) => {
            if (event.key === 'Enter') {
              submit()
            }
          }}
        />
        <TextButton
          onClick={submit}
          disabled={selectedSerial === null || input.trim().length === 0}
        >
          解析
        </TextButton>
      </div>

      {/*
        The second way to fill the box above: pick from what is installed. Left
        enabled with no device on purpose — the store answers with 请先选择一个设备,
        which explains more than a greyed-out button with no reason.
      */}
      <TextButton className="dl-panel__action" onClick={toggleInstalledApps}>
        {installedAppsOpen ? '收起应用列表' : '从已安装应用中选择'}
      </TextButton>

      {installedAppsOpen ? (
        <>
          {/*
            The 32px control height this panel uses for a single-line field (see
            `.dl-target__input`). The existing row class is what makes that field's
            `flex: 1` mean "full width": as a direct child of the section's *column*
            flex it would mean "grow in height" instead.
          */}
          <div className="dl-target__row">
            <Textfield
              className="dl-target__input"
              variant="outlined"
              label="搜索应用名 / 包名"
              value={installedAppsQuery}
              onChange={(event: Event) =>
                setInstalledAppsQuery((event.target as MdOutlinedTextField).value)
              }
            />
          </div>

          {/* The device filters system packages, so flipping this re-reads the list. */}
          <OutlinedSegmentedButtonSet
            className="dl-segmented"
            selectType="single"
            size="xsmall"
            selectedIcon="✓"
            value={showSystemApps ? 'system' : 'user'}
            onChange={(value) => {
              const next = Array.isArray(value) ? value[0] : value
              if (next === undefined) {
                return
              }
              setShowSystemApps(next === 'system')
            }}
            aria-label="是否包含系统应用"
          >
            <OutlinedSegmentedButton value="user" label="仅用户" />
            <OutlinedSegmentedButton value="system" label="含系统" />
          </OutlinedSegmentedButtonSet>

          {installedAppsLoading ? (
            <p className="dl-panel__hint">正在读取已安装应用…</p>
          ) : null}

          {installedAppsError !== null ? (
            <p className="dl-panel__hint">{installedAppsError.message}</p>
          ) : null}

          <h4 className="dl-panel__label">已安装应用</h4>
          {visibleInstalledApps.length > 0 ? (
            <ul className="dl-apps" aria-label="已安装应用">              {visibleInstalledApps.map((app) => (
                <li key={app.package}>
                  <button
                    type="button"
                    className="dl-apps__item"
                    onClick={() => {
                      setInput(app.package)
                      void resolve()
                    }}
                    title={installedAppTitle(app)}
                  >
                    {/*
                      The device's own name wins. `appDisplayName` is the same
                      last-resort guess the running list used before, and the package
                      is always printed underneath, so nothing is hidden by it.
                    */}
                    <span className="dl-apps__name">
                      {app.label.length > 0
                        ? app.label
                        : appDisplayName(app.package)}
                    </span>
                    <span className="dl-apps__package dl-mono">{app.package}</span>
                  </button>
                </li>
              ))}
            </ul>
          ) : installedAppsLoading ? null : (
            <p className="dl-panel__hint">
              {installedApps.length === 0
                ? '尚未读取到已安装应用。'
                : '没有匹配的应用。'}
            </p>
          )}
        </>
      ) : null}

      {runningApps.length > 0 ? (
        <TextButton
          className="dl-panel__action"
          onClick={() => setRunningAppsOpen((open) => !open)}
        >
          {runningAppsOpen ? '收起运行列表' : '展开运行列表'}
        </TextButton>
      ) : null}

      {runningApps.length > 0 && runningAppsOpen ? (
        <h4 className="dl-panel__label">运行中的应用</h4>
      ) : null}

      {runningApps.length > 0 && runningAppsOpen ? (
        /*
          A list rather than a select: every entry has to carry three things — the
          name (top), the package (below, grey, monospace) and pid/uid (on hover,
          where there is no room to print them without truncating the name).
        */
        <ul className="dl-apps" aria-label="运行中的应用">
          {runningApps
            // The list comes from `ps`, so it also contains process names that are
            // not packages (`.dataservices`, `kworker`). Those have no label and no
            // meaning in the target field, and showing them as "Dataservices" reads
            // like an application name.
            .filter((app) => /^[A-Za-z][\w]*(\.[\w]+)+$/.test(app.package))
            .map((app) => (
            <li key={app.package}>
              <button
                type="button"
                className="dl-apps__item"
                onClick={() => {
                  setInput(app.package)
                  void resolve()
                }}
                title={
                  `PID ${app.pids.length > 0 ? app.pids.join(', ') : '—'}` +
                  ` · UID ${app.uid ?? '—'}\n${app.package}`
                }
              >
                <span className="dl-apps__name">
                  {installedLabels.get(app.package) ?? appDisplayName(app.package)}
                </span>
                <span className="dl-apps__package dl-mono">{app.package}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      <TextButton
        className="dl-panel__action"
        onClick={() => void refreshRunning()}
        disabled={selectedSerial === null}
      >
        刷新运行列表
      </TextButton>

      {/*
        Launch watcher: the answer to "it crashes before I can start a capture".
      */}
      <TextButton
        className={`dl-panel__action${watching ? ' dl-panel__action--on' : ''}`}
        onClick={() => void setWatching(!watching)}
        disabled={selectedSerial === null || input.trim().length === 0}
        title="每秒检查一次；应用一旦启动就立刻开始采集"
      >
        {watching ? '停止监听' : '监听启动'}
      </TextButton>

      {watching ? (
        <p className="dl-panel__hint">
          监听中：<span className="dl-mono">{input.trim()}</span>{' '}
          启动后立即自动采集（每秒检查一次；应用消失后会重新武装）。
        </p>
      ) : null}

      {target === null ? (
        <p className="dl-panel__hint">
          留空表示不过滤应用；日志表格会显示所选采集源的全部输出。
        </p>
      ) : target.found ? (
        <div className="dl-target__result">
          <span className="dl-chip dl-chip--accent dl-target__chip" title={target.input}>
            {chipText(target)}
          </span>
          <span className="dl-panel__hint">
            每 30 秒自动重新解析；应用重启换了 PID 会自动跟随。
          </span>
        </div>
      ) : (
        <div className="dl-target__result">
          <span className="dl-target__missing">应用未开启或不存在</span>
          {target.reason !== null ? (
            <span className="dl-panel__hint">{target.reason}</span>
          ) : null}
        </div>
      )}

      {target !== null ? (
        <TextButton className="dl-panel__action" onClick={() => void clear()}>
          清除目标
        </TextButton>
      ) : null}
    </section>
  )
}

/**
 * The hover tooltip of an installed-app row.
 *
 * Follows the running list's shape — the facts on the first line, the package on
 * its own line below, where there is no room to print it without truncating the
 * name — and omits every part the device did not report.
 */
function installedAppTitle(app: InstalledApp): string {
  const parts: string[] = []
  if (app.uid !== null) {
    parts.push(`UID ${app.uid}`)
  }
  if (app.versionName !== null && app.versionName.length > 0) {
    parts.push(app.versionName)
  }
  if (app.installedAt !== null) {
    parts.push(formatInstalledAt(app.installedAt))
  }
  const meta = parts.join(' · ')
  return meta.length > 0 ? `${meta}\n${app.package}` : app.package
}

/**
 * The device's install/update wall clock, rendered as local time.
 *
 * The device prints `YYYY-MM-DD HH:MM:SS` with no zone, so parsing it as *this
 * host's* local time is what the device meant — the backend deliberately passes
 * the text through instead of converting it. Text that does not parse is shown
 * verbatim rather than as `Invalid Date`.
 */
function formatInstalledAt(raw: string): string {
  const date = new Date(raw.replace(' ', 'T'))
  return Number.isNaN(date.getTime()) ? raw : date.toLocaleString('zh-CN')
}

/** Renders the resolved identity the way the spec describes it. */
function chipText(target: {
  package: string | null
  uid: number | null
  pids: number[]
  input: string
}): string {
  const parts: string[] = []
  if (target.package !== null) {
    parts.push(target.package)
  }
  if (target.uid !== null) {
    parts.push(`UID ${target.uid}`)
  }
  if (target.pids.length > 0) {
    parts.push(`PIDs [${target.pids.join(',')}]`)
  }
  return parts.length > 0 ? parts.join(' · ') : target.input
}

/* -------------------------------------------------------------------------- */
/* Advanced rules                                                             */
/* -------------------------------------------------------------------------- */

function RuleRow({ rule }: { rule: FilterRule }): JSX.Element {
  const updateFilter = useAppStore((state) => state.updateFilter)
  const removeFilter = useAppStore((state) => state.removeFilter)

  return (
    <div className="dl-rule">
      {/*
        The rule is laid out over two rows on purpose. The panel is ~227px of
        content, and `开关 + 字段 + 比较 + 删除` on one line left the two selects
        39px and 51px wide — "包含" wrapped onto two lines and its arrow rendered
        outside the outlined box. Row one carries the controls that act on the
        rule (enable, delete); row two carries the ones that describe it, which
        each get about the same width as the "new rule" selects above.
      */}
      <div className="dl-rule__head">
        <Switch
          selected={rule.enabled}
          aria-label="启用该规则"
          onChange={(event: Event) =>
            void updateFilter(rule.id, {
              enabled: (event.target as MdSwitch).selected,
            })
          }
        />

        <TextButton
          className="dl-rule__remove"
          aria-label="删除规则"
          onClick={() => void removeFilter(rule.id)}
        >
          ✕
        </TextButton>
      </div>

      <div className="dl-rule__row">
        <OutlinedSelect
          menuPositioning="fixed"
          onOpening={(event: Event) => reserveMenuScrollbar(event.target as Element)}
          className="dl-rule__field"
          value={rule.field}
          aria-label="字段"
          onChange={(event: Event) =>
            void updateFilter(rule.id, {
              field: (event.target as MdOutlinedSelect).value as FilterField,
            })
          }
        >
          {FIELDS.map((field) => (
            <SelectOption key={field.value} value={field.value}>
              <div slot="headline">{field.label}</div>
            </SelectOption>
          ))}
        </OutlinedSelect>

        <OutlinedSelect
          menuPositioning="fixed"
          onOpening={(event: Event) => reserveMenuScrollbar(event.target as Element)}
          className="dl-rule__op"
          value={rule.op}
          aria-label="比较方式"
          onChange={(event: Event) =>
            void updateFilter(rule.id, {
              op: (event.target as MdOutlinedSelect).value as FilterOp,
            })
          }
        >
          {OPS.map((op) => (
            <SelectOption key={op.value} value={op.value}>
              <div slot="headline">{op.label}</div>
            </SelectOption>
          ))}
        </OutlinedSelect>
      </div>

      <div className="dl-rule__value">
        {isLevelOp(rule.op) ? (
          <OutlinedSelect
            menuPositioning="fixed"
            onOpening={(event: Event) => reserveMenuScrollbar(event.target as Element)}
            className="dl-rule__input"
            value={rule.value}
            aria-label="级别"
            onChange={(event: Event) =>
              void updateFilter(rule.id, {
                value: (event.target as MdOutlinedSelect).value,
              })
            }
          >
            {LOG_LEVELS.map((level: LogLevel) => (
              <SelectOption key={level} value={level}>
                <div slot="headline">{levelLabel(level)}</div>
              </SelectOption>
            ))}
          </OutlinedSelect>
        ) : (
          <Textfield
            className="dl-rule__input"
            variant="outlined"
            value={rule.value}
            placeholder={
              isWindowOp(rule.op)
                ? '秒数，如 30'
                : rule.op === 'in'
                  ? 'ActivityManager,binder'
                  : '匹配内容'
            }
            aria-label="比较值"
            onChange={(event: Event) =>
              void updateFilter(rule.id, {
                value: (event.target as MdOutlinedTextField).value,
              })
            }
          />
        )}

        <label className="dl-rule__case">
          <Switch
            selected={rule.caseSensitive}
            disabled={isLevelOp(rule.op) || isWindowOp(rule.op)}
            onChange={(event: Event) =>
              void updateFilter(rule.id, {
                caseSensitive: (event.target as MdSwitch).selected,
              })
            }
          />
          <span>区分大小写</span>
        </label>
      </div>
    </div>
  )
}

/* -------------------------------------------------------------------------- */
/* Panel                                                                      */
/* -------------------------------------------------------------------------- */

export function FilterPanel(): JSX.Element {
  const structured = useAppStore((state) => state.structured)
  const setStructured = useAppStore((state) => state.setStructured)
  const filtersError = useAppStore((state) => state.filtersError)
  const advanced = useAppStore((state) => state.filters).filter(
    (rule) => !rule.id.startsWith('__'),
  )
  const addFilter = useAppStore((state) => state.addFilter)
  const clearFilters = useAppStore((state) => state.clearFilters)
  const refreshRunning = useAppStore((state) => state.refreshRunningApps)

  const [draftField, setDraftField] = useState<FilterField>('tag')
  const [draftOp, setDraftOp] = useState<FilterOp>('contains')
  const [draftValue, setDraftValue] = useState('')

  // Populate the running-app picker once a device is selected.
  useEffect(() => {
    void refreshRunning()
  }, [refreshRunning])

  const canAdd =
    draftValue.trim().length > 0 || isLevelOp(draftOp) || isWindowOp(draftOp)

  const submit = (): void => {
    if (!canAdd) {
      return
    }
    const fallback = isLevelOp(draftOp) ? 'warn' : isWindowOp(draftOp) ? '30' : ''
    void addFilter({
      id: nextRuleId(),
      enabled: true,
      field: draftField,
      op: draftOp,
      value: draftValue.trim().length > 0 ? draftValue.trim() : fallback,
      caseSensitive: false,
    })
    setDraftValue('')
  }

  const toggleLevel = (level: LogLevel): void => {
    const levels = structured.levels.includes(level)
      ? structured.levels.filter((item) => item !== level)
      : [...structured.levels, level]
    void setStructured({ levels })
  }

  return (
    <aside className="dl-filters" aria-label="过滤规则">
      <header className="dl-filters__header">
        <h2 className="dl-filters__title">过滤</h2>
        <TextButton onClick={() => void clearFilters()}>全部清除</TextButton>
      </header>

      <div className="dl-filters__list dl-scroll">
        <TargetSection />

        {/* ---------------------------------------------------------- level */}
        <section className="dl-panel__section">
          <h3 className="dl-panel__label">日志级别</h3>
          <div className="dl-levels">
            {LOG_LEVELS.map((level) => {
              const active = structured.levels.includes(level)
              return (
                <FilterChip
                  key={level}
                  className="dl-level-chip"
                  selected={active}
                  onClick={() => toggleLevel(level)}
                  title={levelLabel(level)}
                >
                  <span className={`dl-level--${level}`}>
                    {level.slice(0, 1).toUpperCase()}
                  </span>
                  <span className="dl-chip-label">{levelLabel(level)}</span>
                </FilterChip>
              )
            })}
          </div>
          <p className="dl-panel__hint">
            {structured.levels.length === 0
              ? '未选择：显示全部级别'
              : `仅显示 ${structured.levels.map(levelLabel).join(' / ')}`}
          </p>
        </section>

        {/* ------------------------------------------------------------ tag */}
        <section className="dl-panel__section">
          <h3 className="dl-panel__label">TAG</h3>
          <Textfield
            variant="outlined"
            value={structured.tagInclude}
            placeholder="TAG 包含…"
            aria-label="TAG 包含"
            onChange={(event: Event) =>
              void setStructured({
                tagInclude: (event.target as MdOutlinedTextField).value,
              })
            }
          />
          <Textfield
            variant="outlined"
            value={structured.tagExclude}
            placeholder="TAG 排除…"
            aria-label="TAG 排除"
            onChange={(event: Event) =>
              void setStructured({
                tagExclude: (event.target as MdOutlinedTextField).value,
              })
            }
          />
        </section>

        {/* -------------------------------------------------------- keyword */}
        <section className="dl-panel__section">
          <h3 className="dl-panel__label">关键字</h3>
          <Textfield
            variant="outlined"
            value={structured.keyword}
            placeholder={structured.keywordIsRegex ? '正则表达式' : '消息包含…'}
            aria-label="关键字"
            onChange={(event: Event) =>
              void setStructured({
                keyword: (event.target as MdOutlinedTextField).value,
              })
            }
          />
          <label className="dl-switch-row">
            <Switch
              selected={structured.keywordIsRegex}
              onChange={(event: Event) =>
                void setStructured({
                  keywordIsRegex: (event.target as MdSwitch).selected,
                })
              }
            />
            <span className="dl-switch-row__text">按正则匹配</span>
          </label>
        </section>

        {/* --------------------------------------------------------- window */}
        <section className="dl-panel__section">
          <h3 className="dl-panel__label">时间范围</h3>
          <Textfield
            variant="outlined"
            type="number"
            value={structured.windowSeconds === null ? '' : String(structured.windowSeconds)}
            placeholder="最近 N 秒"
            suffixText="秒"
            aria-label="最近 N 秒"
            onChange={(event: Event) => {
              const raw = (event.target as MdOutlinedTextField).value.trim()
              const parsed = Number.parseInt(raw, 10)
              void setStructured({
                windowSeconds:
                  raw.length === 0 || Number.isNaN(parsed) || parsed <= 0
                    ? null
                    : parsed,
              })
            }}
          />
          <OutlinedSegmentedButtonSet
            className="dl-segmented"
            selectType="single"
            size="xsmall"
            selectedIcon="✓"
            value={structured.windowSeconds === null ? 'none' : String(structured.windowSeconds)}
            onChange={(value) => {
              const next = Array.isArray(value) ? value[0] : value
              if (next === undefined) {
                return
              }
              const parsed = Number.parseInt(next, 10)
              void setStructured({
                windowSeconds: next === 'none' || Number.isNaN(parsed) ? null : parsed,
              })
            }}
            aria-label="时间范围预设"
          >
            {WINDOW_PRESETS.map((preset) => (
              <OutlinedSegmentedButton
                key={preset.value}
                value={preset.value}
                label={preset.label}
              />
            ))}
          </OutlinedSegmentedButtonSet>
          <p className="dl-panel__hint">
            按记录到达本机的时间计算（设备时间戳无年份与时区，无法与绝对时刻比较）。
          </p>
        </section>

        {/* ------------------------------------------------------- advanced */}
        <section className="dl-panel__section">
          <h3 className="dl-panel__label">高级规则（AND）</h3>
          {advanced.length === 0 ? (
            <p className="dl-panel__hint">没有额外规则。</p>
          ) : (
            advanced.map((rule) => <RuleRow key={rule.id} rule={rule} />)
          )}

          <div className="dl-filters__add-row">
            <OutlinedSelect
              menuPositioning="fixed"
              onOpening={(event: Event) => reserveMenuScrollbar(event.target as Element)}
              className="dl-rule__field"
              value={draftField}
              aria-label="新规则字段"
              onChange={(event: Event) =>
                setDraftField(
                  (event.target as MdOutlinedSelect).value as FilterField,
                )
              }
            >
              {FIELDS.map((field) => (
                <SelectOption key={field.value} value={field.value}>
                  <div slot="headline">{field.label}</div>
                </SelectOption>
              ))}
            </OutlinedSelect>
            <OutlinedSelect
              menuPositioning="fixed"
              onOpening={(event: Event) => reserveMenuScrollbar(event.target as Element)}
              className="dl-rule__op"
              value={draftOp}
              aria-label="新规则比较方式"
              onChange={(event: Event) =>
                setDraftOp((event.target as MdOutlinedSelect).value as FilterOp)
              }
            >
              {OPS.map((op) => (
                <SelectOption key={op.value} value={op.value}>
                  <div slot="headline">{op.label}</div>
                </SelectOption>
              ))}
            </OutlinedSelect>
          </div>
          <div className="dl-filters__add-row">
            <Textfield
              className="dl-filters__add-input"
              variant="outlined"
              value={draftValue}
              placeholder="匹配内容"
              aria-label="新规则比较值"
              onChange={(event: Event) =>
                setDraftValue((event.target as MdOutlinedTextField).value)
              }
              onKeyDown={(event: React.KeyboardEvent) => {
                if (event.key === 'Enter') {
                  submit()
                }
              }}
            />
            <OutlinedButton onClick={submit} disabled={!canAdd}>
              添加
            </OutlinedButton>
          </div>
        </section>
      </div>

      {filtersError !== null ? (
        <div className="dl-filters__error">
          <span>{filtersError.message}</span>
          {filtersError.detail !== null ? (
            <span className="dl-mono">{filtersError.detail}</span>
          ) : null}
        </div>
      ) : null}
    </aside>
  )
}
