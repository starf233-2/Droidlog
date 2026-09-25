/**
 * Colour schemes for the Material 3 UI.
 *
 * The component library (`material-expressive-react`, wrapping `@material/web`)
 * ships no colours of its own: its stylesheet *reads* `--md-sys-color-*` 142
 * times and defines none of them. So the colour system is the application's job,
 * and that is what this module is.
 *
 * Two families of custom properties are produced from one source of truth:
 *
 * * `--md-sys-color-*` — the standard Material 3 roles, consumed by every
 *   library element (buttons, text fields, chips, menus, ...).
 * * `--dl-*` — this application's own names, kept because the log table's
 *   virtualised rows and the layout use them; they are now derived from the M3
 *   roles rather than hand-written per theme.
 *
 * Colour themes are either **authored** (the default `slate` scheme reproduces
 * the values this project used before, so the switch to M3 tokens is visually a
 * no-op) or **generated** from a seed hue by {@link fromSeed}.
 *
 * Hard constraints carried over from the original design and honoured by both
 * paths: no gradients anywhere, no vivid or neon colour, low chroma throughout.
 * Generated themes run at chroma 0.05 for the primary palette — a typical
 * Material 3 primary sits near 0.13, so these stay deliberately muted.
 */

/** Which half of a colour theme is active. */
export type SchemeMode = 'light' | 'dark'

/**
 * One Material 3 colour scheme: role -> CSS colour value.
 *
 * `primaryHover` / `primaryActive` are not part of the M3 role list; they exist
 * because the buttons in this app need a hover/pressed tone and the original
 * design expressed those as explicit colours instead of state layers.
 */
export interface Scheme {
  primary: string
  onPrimary: string
  primaryContainer: string
  onPrimaryContainer: string
  primaryHover: string
  primaryActive: string
  secondary: string
  onSecondary: string
  secondaryContainer: string
  onSecondaryContainer: string
  tertiary: string
  onTertiary: string
  tertiaryContainer: string
  onTertiaryContainer: string
  error: string
  onError: string
  errorContainer: string
  onErrorContainer: string
  background: string
  onBackground: string
  surface: string
  onSurface: string
  surfaceVariant: string
  onSurfaceVariant: string
  surfaceContainerLowest: string
  surfaceContainerLow: string
  surfaceContainer: string
  surfaceContainerHigh: string
  surfaceContainerHighest: string
  surfaceDim: string
  surfaceBright: string
  surfaceTint: string
  inverseSurface: string
  inverseOnSurface: string
  inversePrimary: string
  outline: string
  outlineVariant: string
  scrim: string
  shadow: string
}

/** A named colour theme, complete in both modes. */
export interface ColorTheme {
  id: ColorThemeId
  /** Shown in the toolbar. */
  label: string
  light: Scheme
  dark: Scheme
}

/** Stable ids for the built-in themes. */
export type ColorThemeId = 'slate' | 'jade' | 'indigo' | 'clay'

/**
 * Roles that have a standard `--md-sys-color-*` name.
 *
 * The two hover roles are excluded on purpose: they are this app's invention and
 * must not masquerade as Material roles.
 */
const MD_ROLES = [
  'primary',
  'onPrimary',
  'primaryContainer',
  'onPrimaryContainer',
  'secondary',
  'onSecondary',
  'secondaryContainer',
  'onSecondaryContainer',
  'tertiary',
  'onTertiary',
  'tertiaryContainer',
  'onTertiaryContainer',
  'error',
  'onError',
  'errorContainer',
  'onErrorContainer',
  'background',
  'onBackground',
  'surface',
  'onSurface',
  'surfaceVariant',
  'onSurfaceVariant',
  'surfaceContainerLowest',
  'surfaceContainerLow',
  'surfaceContainer',
  'surfaceContainerHigh',
  'surfaceContainerHighest',
  'surfaceDim',
  'surfaceBright',
  'surfaceTint',
  'inverseSurface',
  'inverseOnSurface',
  'inversePrimary',
  'outline',
  'outlineVariant',
  'scrim',
  'shadow',
] as const satisfies readonly (keyof Scheme)[]

/** `surfaceContainerLow` -> `surface-container-low`. */
function kebab(name: string): string {
  return name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)
}

/**
 * Every custom property a scheme defines, ready for `setProperty`.
 *
 * Emitting the full M3 role list (not just the 23 the library's own stylesheet
 * reads) matters because the underlying `@material/web` elements read more of
 * them from inside their shadow roots — `error`, `on-error` and `surface-tint`
 * never appear in the wrapper CSS but a filled button still wants them.
 */
export function cssVariablesFor(scheme: Scheme): Record<string, string> {
  const vars: Record<string, string> = {}

  for (const role of MD_ROLES) {
    vars[`--md-sys-color-${kebab(role)}`] = scheme[role]
  }

  // The app's own names, derived from the same roles. Keeping them means the
  // 900 lines of existing layout CSS did not have to be rewritten to change
  // colour, and any future component can read either family.
  vars['--dl-primary'] = scheme.primary
  vars['--dl-on-primary'] = scheme.onPrimary
  vars['--dl-primary-container'] = scheme.primaryContainer
  vars['--dl-on-primary-container'] = scheme.onPrimaryContainer
  vars['--dl-primary-hover'] = scheme.primaryHover
  vars['--dl-primary-active'] = scheme.primaryActive
  vars['--dl-secondary'] = scheme.secondary
  vars['--dl-on-secondary'] = scheme.onSecondary
  vars['--dl-secondary-container'] = scheme.secondaryContainer
  vars['--dl-on-secondary-container'] = scheme.onSecondaryContainer
  vars['--dl-tertiary'] = scheme.tertiary
  vars['--dl-on-tertiary'] = scheme.onTertiary
  vars['--dl-tertiary-container'] = scheme.tertiaryContainer
  vars['--dl-on-tertiary-container'] = scheme.onTertiaryContainer
  vars['--dl-error'] = scheme.error
  vars['--dl-on-error'] = scheme.onError
  vars['--dl-error-container'] = scheme.errorContainer
  vars['--dl-on-error-container'] = scheme.onErrorContainer
  vars['--dl-background'] = scheme.background
  vars['--dl-on-background'] = scheme.onBackground
  vars['--dl-surface'] = scheme.surface
  vars['--dl-on-surface'] = scheme.onSurface
  vars['--dl-surface-variant'] = scheme.surfaceVariant
  vars['--dl-on-surface-variant'] = scheme.onSurfaceVariant
  vars['--dl-surface-container-lowest'] = scheme.surfaceContainerLowest
  vars['--dl-surface-container-low'] = scheme.surfaceContainerLow
  vars['--dl-surface-container'] = scheme.surfaceContainer
  vars['--dl-surface-container-high'] = scheme.surfaceContainerHigh
  vars['--dl-surface-container-highest'] = scheme.surfaceContainerHighest
  vars['--dl-inverse-surface'] = scheme.inverseSurface
  vars['--dl-inverse-on-surface'] = scheme.inverseOnSurface
  vars['--dl-outline'] = scheme.outline
  vars['--dl-outline-variant'] = scheme.outlineVariant
  vars['--dl-scrim'] = scheme.scrim

  // Neutral scrims for hover/press, which the original design expressed as
  // translucent neutrals over whatever surface was underneath.
  vars['--dl-hover-overlay'] = hoverOverlay(scheme)
  vars['--dl-pressed-overlay'] = pressedOverlay(scheme)

  return vars
}

/**
 * Hover/pressed tints.
 *
 * Derived with `color-mix` in Oklab so they stay correct for every scheme
 * instead of being tabulated per theme: 4.5% of the scheme's own on-surface
 * tone over the surface, which is the M3 state-layer recipe.
 */
function hoverOverlay(scheme: Scheme): string {
  return `color-mix(in oklab, ${scheme.onSurface} 4.5%, transparent)`
}

function pressedOverlay(scheme: Scheme): string {
  return `color-mix(in oklab, ${scheme.onSurface} 8.5%, transparent)`
}

/**
 * One tone of a tonal palette, as an OKLCH colour string.
 *
 * Deliberately not implemented with a colour library: emitting `oklch()` and
 * letting Chromium convert keeps this file free of colour maths, and the webview
 * is Chromium 153. `tone` follows the Material scale (0 = black, 100 = white),
 * which maps closely enough onto OKLCH lightness for a UI palette.
 */
function tone(hue: number, chroma: number, toneValue: number): string {
  return `oklch(${toneValue}% ${chroma} ${hue})`
}

/** Chroma per palette family. Low numbers on purpose — see the file header. */
const CHROMA = {
  primary: 0.05,
  secondary: 0.026,
  tertiary: 0.04,
  // Slightly above the authored scheme's 0.005–0.008: a generated theme has to
  // read as *its* hue family next to its own buttons, and at 0.005 the neutral
  // surfaces were indistinguishable grey.
  neutral: 0.009,
  neutralVariant: 0.013,
  error: 0.055,
} as const

/**
 * Neutral tones for the generated **dark** schemes.
 *
 * Material 3's standard dark ladder is 4/6/10/12/17/22, and in OKLCH those are
 * black: 4% lightness renders as `#000`-ish, so three of the four colour themes
 * had a dead-black background while the authored `slate` theme showed its
 * charcoal grey. Measured from `slate`'s dark palette (sRGB → OKLCH):
 *
 * | role | slate | L |
 * | --- | --- | --- |
 * | `surfaceContainerLowest` / dim | `#0d0f11` | 16.7 |
 * | `surface` / background | `#121517` | 19.3 |
 * | `surfaceContainerLow` | `#1a1d1f` | 22.8 |
 * | `surfaceContainer` | `#1e2224` | 24.9 |
 * | `surfaceContainerHigh` | `#282c2f` | 29.0 |
 * | `surfaceContainerHighest` / bright | `#333739` | 33.4 |
 * | `surfaceVariant` / `outlineVariant` | `#3f4447` | 38.3 |
 *
 * The generated themes now use exactly this ladder, tinted with their own hue,
 * so all four are the same weight in dark mode and differ only in colour family.
 */
const DARK_TONE = {
  containerLowest: 17,
  background: 19,
  containerLow: 23,
  container: 25,
  containerHigh: 29,
  containerHighest: 33,
  surfaceVariant: 38,
} as const

/**
 * Builds a light/dark scheme pair from three seed hues.
 *
 * The role-to-tone mapping is the standard Material 3 table, so anything that
 * knows M3 (`@material/web` included) sees the expected relationships: `surface`
 * and `on-surface` ten tones apart, containers at 90/10 in light and 30/90 in
 * dark, and so on.
 */
function fromSeed(primaryHue: number, tertiaryHue: number): {
  light: Scheme
  dark: Scheme
} {
  const h = primaryHue
  const t = tertiaryHue
  const secondaryHue = primaryHue + 12
  const errorHue = 25

  const p = (value: number): string => tone(h, CHROMA.primary, value)
  const s = (value: number): string => tone(secondaryHue, CHROMA.secondary, value)
  const te = (value: number): string => tone(t, CHROMA.tertiary, value)
  const n = (value: number): string => tone(h, CHROMA.neutral, value)
  const nv = (value: number): string => tone(h, CHROMA.neutralVariant, value)
  const e = (value: number): string => tone(errorHue, CHROMA.error, value)

  const light: Scheme = {
    primary: p(40),
    onPrimary: p(100),
    primaryContainer: p(90),
    onPrimaryContainer: p(10),
    primaryHover: p(32),
    primaryActive: p(24),
    secondary: s(40),
    onSecondary: s(100),
    secondaryContainer: s(90),
    onSecondaryContainer: s(10),
    tertiary: te(40),
    onTertiary: te(100),
    tertiaryContainer: te(90),
    onTertiaryContainer: te(10),
    error: e(40),
    onError: e(100),
    errorContainer: e(90),
    onErrorContainer: e(10),
    background: n(98),
    onBackground: n(10),
    surface: n(98),
    onSurface: n(10),
    surfaceVariant: nv(90),
    onSurfaceVariant: nv(30),
    surfaceContainerLowest: n(100),
    surfaceContainerLow: n(96),
    surfaceContainer: n(94),
    surfaceContainerHigh: n(92),
    surfaceContainerHighest: n(90),
    surfaceDim: n(87),
    surfaceBright: n(100),
    surfaceTint: p(40),
    inverseSurface: n(20),
    inverseOnSurface: n(95),
    inversePrimary: p(80),
    outline: nv(50),
    outlineVariant: nv(80),
    scrim: 'rgb(0 0 0 / 32%)',
    shadow: 'rgb(0 0 0)',
  }

  const dark: Scheme = {
    primary: p(80),
    onPrimary: p(20),
    primaryContainer: p(30),
    onPrimaryContainer: p(90),
    primaryHover: p(86),
    primaryActive: p(92),
    secondary: s(80),
    onSecondary: s(20),
    secondaryContainer: s(30),
    onSecondaryContainer: s(90),
    tertiary: te(80),
    onTertiary: te(20),
    tertiaryContainer: te(30),
    onTertiaryContainer: te(90),
    error: e(80),
    onError: e(20),
    errorContainer: e(30),
    onErrorContainer: e(90),
    background: n(DARK_TONE.background),
    onBackground: n(92),
    surface: n(DARK_TONE.background),
    onSurface: n(92),
    surfaceVariant: nv(DARK_TONE.surfaceVariant),
    onSurfaceVariant: nv(82),
    surfaceContainerLowest: n(DARK_TONE.containerLowest),
    surfaceContainerLow: n(DARK_TONE.containerLow),
    surfaceContainer: n(DARK_TONE.container),
    surfaceContainerHigh: n(DARK_TONE.containerHigh),
    surfaceContainerHighest: n(DARK_TONE.containerHighest),
    surfaceDim: n(DARK_TONE.containerLowest),
    surfaceBright: n(DARK_TONE.containerHighest),
    surfaceTint: p(80),
    inverseSurface: n(90),
    inverseOnSurface: n(20),
    inversePrimary: p(40),
    outline: nv(65),
    outlineVariant: nv(DARK_TONE.surfaceVariant),
    scrim: 'rgb(0 0 0 / 50%)',
    shadow: 'rgb(0 0 0)',
  }

  return { light, dark }
}

/**
 * The default theme, authored rather than generated.
 *
 * These are the exact colours this application shipped before it moved onto
 * Material tokens, so adopting the library and the theme engine did not restyle
 * the product: the desaturated gray-blue primary (`#4a5a6a`) and its whole
 * surface scale survive as the `slate` theme.
 *
 * `inversePrimary` is the other mode's primary, which is what M3 means by it.
 */
const SLATE: ColorTheme = {
  id: 'slate',
  label: '灰蓝',
  light: {
    primary: '#4a5a6a',
    onPrimary: '#ffffff',
    primaryContainer: '#d6dee6',
    onPrimaryContainer: '#16232e',
    primaryHover: '#405062',
    primaryActive: '#374556',
    secondary: '#54606b',
    onSecondary: '#ffffff',
    secondaryContainer: '#e0e5ea',
    onSecondaryContainer: '#1b242c',
    tertiary: '#5c6470',
    onTertiary: '#ffffff',
    tertiaryContainer: '#e2e5eb',
    onTertiaryContainer: '#20262e',
    error: '#8a5c5b',
    onError: '#ffffff',
    errorContainer: '#f0e4e3',
    onErrorContainer: '#3a2221',
    background: '#fbfcfd',
    onBackground: '#191c1e',
    surface: '#fbfcfd',
    onSurface: '#191c1e',
    surfaceVariant: '#e7eaed',
    onSurfaceVariant: '#454b50',
    surfaceContainerLowest: '#ffffff',
    surfaceContainerLow: '#f6f8f9',
    surfaceContainer: '#f1f4f6',
    surfaceContainerHigh: '#eaeef1',
    surfaceContainerHighest: '#e3e8ec',
    surfaceDim: '#eaeef1',
    surfaceBright: '#ffffff',
    surfaceTint: '#4a5a6a',
    inverseSurface: '#2e3235',
    inverseOnSurface: '#f0f2f4',
    inversePrimary: '#a9bac9',
    outline: '#757b80',
    outlineVariant: '#c5cace',
    scrim: 'rgba(20, 24, 27, 0.32)',
    shadow: '#000000',
  },
  dark: {
    primary: '#a9bac9',
    onPrimary: '#1b2a36',
    primaryContainer: '#33424f',
    onPrimaryContainer: '#d3e1ee',
    primaryHover: '#b6c6d4',
    primaryActive: '#c2d0dc',
    secondary: '#b0bac4',
    onSecondary: '#232b32',
    secondaryContainer: '#333c44',
    onSecondaryContainer: '#dde3e9',
    tertiary: '#b4bac4',
    onTertiary: '#262c33',
    tertiaryContainer: '#373e45',
    onTertiaryContainer: '#dfe3e9',
    error: '#c99a99',
    onError: '#3a2221',
    errorContainer: '#4a2e2e',
    onErrorContainer: '#f0dedd',
    background: '#121517',
    onBackground: '#e2e5e8',
    surface: '#121517',
    onSurface: '#e2e5e8',
    surfaceVariant: '#3f4447',
    onSurfaceVariant: '#bfc5ca',
    surfaceContainerLowest: '#0d0f11',
    surfaceContainerLow: '#1a1d1f',
    surfaceContainer: '#1e2224',
    surfaceContainerHigh: '#282c2f',
    surfaceContainerHighest: '#333739',
    surfaceDim: '#0d0f11',
    surfaceBright: '#333739',
    surfaceTint: '#a9bac9',
    inverseSurface: '#e2e5e8',
    inverseOnSurface: '#2e3235',
    inversePrimary: '#4a5a6a',
    outline: '#8a9095',
    outlineVariant: '#3f4447',
    scrim: 'rgba(4, 6, 8, 0.5)',
    shadow: '#000000',
  },
}

/** Generated alternatives: same restrained chroma, different hue family. */
const JADE: ColorTheme = { id: 'jade', label: '青瓷', ...fromSeed(175, 205) }
const INDIGO: ColorTheme = { id: 'indigo', label: '靛紫', ...fromSeed(285, 320) }
const CLAY: ColorTheme = { id: 'clay', label: '陶土', ...fromSeed(45, 20) }

/** Every selectable colour theme, in toolbar order. `slate` is the default. */
export const COLOR_THEMES: readonly ColorTheme[] = [SLATE, JADE, INDIGO, CLAY]

/** Looks a theme up by id, falling back to the default. */
export function colorThemeById(id: string): ColorTheme {
  return COLOR_THEMES.find((theme) => theme.id === id) ?? SLATE
}

/** Picks the half of a theme that a resolved mode asks for. */
export function schemeFor(id: string, mode: SchemeMode): Scheme {
  const theme = colorThemeById(id)
  return mode === 'dark' ? theme.dark : theme.light
}

/** The colour the native window should be painted, to match the theme. */
export function windowBackgroundFor(id: string, mode: SchemeMode): string {
  return schemeFor(id, mode).surface
}
