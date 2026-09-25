import { fileURLToPath } from 'node:url'

import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Tauri drives the dev server; the port is fixed so `devUrl` always matches.
const DEV_PORT = 1420

/**
 * The component library ships its stylesheet but does **not** list it in its
 * `package.json#exports` map, so a deep import of the file is rejected by the
 * resolver. An explicit alias is the only way to consume it without either
 * copying the file into this repository (which would then go stale silently) or
 * patching the dependency.
 */
const LIBRARY_CSS = fileURLToPath(
  new URL(
    './node_modules/material-expressive-react/dist/material-expressive-react.css',
    import.meta.url,
  ),
)

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      'material-expressive-react/styles.css': LIBRARY_CSS,
    },
  },
  // Tauri surfaces Rust-side build errors on stderr, so keep the vite output clean.
  clearScreen: false,
  server: {
    port: DEV_PORT,
    strictPort: true,
    host: '127.0.0.1',
    watch: {
      ignored: [
        // src-tauri is watched by the Rust toolchain, not by vite.
        '**/src-tauri/**',
        // Editors and tools write atomically via a temp file + rename. On
        // Windows, chokidar can hit EBUSY trying to watch such a file while it
        // is still open, which crashes the whole watcher (and with it the dev
        // server, leaving the webview showing a stale page). Ignoring the temp
        // patterns keeps hot reload alive through atomic saves.
        '**/*.tmp',
        '**/*.tmpdir/**',
        '**/*~',
      ],
    },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    // Tauri v2 ships a modern WebView2 / WebKit, so we can target evergreen engines.
    target: 'es2022',
    minify: 'esbuild',
    sourcemap: false,
  },
})
