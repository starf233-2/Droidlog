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
import { useEffect, useState } from 'react'
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
import { LOG_LEVELS, levelLabel } from '../lib/format'
import type { FilterField, FilterOp, FilterRule, LogLevel } from '../types'

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
  const input = useAppStore((state) => state.appTargetInput)
  const setInput = useAppStore((state) => state.setAppTargetInput)
  const target = useAppStore((state) => state.appTarget)
  const resolve = useAppStore((state) => state.resolveAppTarget)
  const clear = useAppStore((state) => state.clearAppTarget)
  const runningApps = useAppStore((state) => state.runningApps)
  const refreshRunning = useAppStore((state) => state.refreshRunningApps)
  const selectedSerial = useAppStore((state) => state.selectedSerial)

  const submit = (): void => void resolve()

  /*
   * The picker mirrors the *current target* rather than being a one-shot command.
   *
   * It used to clear itself the moment something was chosen, which left the panel
   * with no control showing what was actually being captured — you had to read the
   * chip to know, and after `清除目标` the dropdown still displayed the old choice
   * while nothing was being followed. Now the selection is derived state: it shows
   * the target's package whenever a target is set, whoever set it (typed, resolved,
   * picked here, or restored by the 30s re-resolver), and it is empty when there is
   * no target.
   *
   * A target that is not a running app — a bare PID, a UID, or a package that has
   * since stopped — simply matches no option, so the field reads as a placeholder
   * while the chip below still states the full identity.
   */
  const selectedPackage = target?.package ?? ''

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

      {runningApps.length > 0 ? (
        <OutlinedSelect
          menuPositioning="fixed"
          onOpening={(event: Event) => reserveMenuScrollbar(event.target as Element)}
          label="从运行中选择"
          value={selectedPackage}
          disabled={selectedSerial === null}
          onChange={(event: Event) => {
            const choice = (event.target as MdOutlinedSelect).value
            if (choice !== '') {
              setInput(choice)
              void resolve()
            }
          }}
        >
          {runningApps.map((app) => (
            <SelectOption
              key={app.package}
              value={app.package}
              // Driving `selected` directly, not only the select's `value`: the
              // element's value setter looks for a matching option, so clearing
              // the value alone can leave the previous option rendered as chosen.
              selected={app.package === selectedPackage}
            >
              <div slot="headline">
                {app.package}
                {app.uid === null ? '' : `（UID ${app.uid}）`}
              </div>
            </SelectOption>
          ))}
        </OutlinedSelect>
      ) : null}

      <TextButton
        className="dl-panel__action"
        onClick={() => void refreshRunning()}
        disabled={selectedSerial === null}
      >
        刷新运行列表
      </TextButton>

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
