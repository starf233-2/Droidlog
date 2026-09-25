/**
 * Applies a colour theme to the document.
 *
 * Written as one place that touches `<html>` so the theme can never be half
 * applied — the previous implementation set `data-theme` in one function and
 * relied on a stylesheet for the colours, which worked only because there was a
 * single palette. There are several now, so the scheme is pushed as custom
 * properties and the attribute is only a hook for stylesheet rules that need to
 * know the mode (`color-scheme`, scrollbar tinting, media-query-free variants).
 */

import {
  colorThemeById,
  cssVariablesFor,
  schemeFor,
  windowBackgroundFor,
  type ColorThemeId,
  type SchemeMode,
} from './schemes'

/** Attribute carrying the resolved mode, read by the stylesheets. */
export const THEME_ATTRIBUTE = 'data-theme'

/** Attribute carrying the colour theme id, for CSS that wants to know. */
export const COLOR_THEME_ATTRIBUTE = 'data-color-theme'

/**
 * Writes the scheme's custom properties and mode attributes onto `<html>`.
 *
 * Custom properties are set inline rather than swapped between stylesheet blocks
 * because they are generated: one source of truth (the scheme object) instead of
 * one hand-maintained block per theme per mode.
 */
export function applyColorScheme(id: ColorThemeId, mode: SchemeMode): void {
  const root = document.documentElement
  const scheme = schemeFor(id, mode)

  for (const [name, value] of Object.entries(cssVariablesFor(scheme))) {
    root.style.setProperty(name, value)
  }

  root.setAttribute(THEME_ATTRIBUTE, mode)
  root.setAttribute(COLOR_THEME_ATTRIBUTE, id)
  // Tells the engine which way native widgets (scrollbars, form controls inside
  // the webview) should render, and keeps `prefers-color-scheme` consumers in
  // step with an explicit choice.
  root.style.colorScheme = mode
}

/** The native window colour for a theme, so resizing never flashes a default. */
export function windowBackground(id: ColorThemeId, mode: SchemeMode): string {
  return windowBackgroundFor(id, mode)
}

/** Display label for a theme id, for the toolbar. */
export function colorThemeLabel(id: string): string {
  return colorThemeById(id).label
}

export { COLOR_THEMES, colorThemeById } from './schemes'
export type { ColorTheme, ColorThemeId, Scheme, SchemeMode } from './schemes'
