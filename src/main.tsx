/**
 * Renderer entry point.
 *
 * Styles are imported here in cascade order: the component library's own sheet
 * first, then the design tokens, the reset/primitives, and finally the layout —
 * so application rules can always override the library's defaults.
 */

import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

// The Material web components' wrapper styles. It *reads* `--md-sys-color-*` and
// defines none of them; the theme module supplies those before first paint.
// Imported through a Vite alias because the package does not export this file.
import 'material-expressive-react/styles.css'

// Fonts are bundled, not fetched: this is a desktop app that has to work offline,
// and the Google Fonts CDN is not reachable from the target network anyway.
//
// * Roboto (variable, weights 100-900) carries Latin letters and digits.
// * Noto Sans SC (variable) carries Chinese. Roboto has no CJK glyphs at all, and
//   before this the Chinese fell through to Microsoft YaHei, whose weight ramp and
//   metrics did not match the Latin text sitting next to it. Noto Sans SC is the
//   simplified-Chinese member of the family Google uses for Material.
//
// Both declare a unicode-range per slice, so the webview loads only the slices a
// given screen actually needs.
import '@fontsource-variable/roboto/wght.css'
import '@fontsource-variable/noto-sans-sc/wght.css'

// Cascade order matters: tokens first, then the reset/primitives, then the bridge
// that pins the library's tokens to this project's contracts, then layout.
import './styles/tokens.css'
import './styles/global.css'
import './styles/material-bridge.css'
import './styles/app.css'
// Crash collection: row markers, the crash-only lens and the report panel.
import './styles/collect.css'

import { App } from './App'
import { applyStoredTheme } from './store/useAppStore'
import { applyMaterialTypography } from 'material-expressive-react/theme'

const container = document.getElementById('root')

if (container === null) {
  // Nothing to render into: fail loudly in the console rather than silently.
  throw new Error('droidlog: #root container is missing from index.html')
}

// Adds the Material typescale utility classes to the document, which the
// library's own markup uses.
applyMaterialTypography()

// Paint the stored theme before the first render, so the very first frame the
// webview composites is already the right surface. The window itself is created
// hidden (`visible: false` in tauri.conf.json) and revealed by App once it has
// committed — see `revealWindow`.
applyStoredTheme()

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
