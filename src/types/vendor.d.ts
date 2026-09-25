/**
 * Ambient declarations for assets and aliased entry points that TypeScript
 * cannot resolve on its own.
 *
 * `material-expressive-react/styles.css` is a *Vite alias* (see vite.config.ts):
 * the library ships the stylesheet but does not expose it through its
 * `package.json#exports` map, so the alias is the only importable name for it.
 * TypeScript has no way to know that, hence this declaration.
 */

declare module 'material-expressive-react/styles.css'
