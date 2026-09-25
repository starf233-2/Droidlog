/**
 * Small fixes that have to reach *inside* a Material component's shadow root.
 *
 * Shadow DOM is why the rest of the styling lives in stylesheets: our rules
 * cannot select a component's internals, and that isolation is usually a
 * feature. There is one case in this app where it is not — see below.
 */

/**
 * Reserves the scrollbar's width inside a Material select's menu.
 *
 * The menu's own scroll container (`md-menu`'s internal `.items`) is
 * `overflow-y: auto` with `scrollbar-gutter: auto`. The menu animates its height
 * over roughly half a second, and only when it reaches its maximum height does
 * the list overflow and the scrollbar appear — so the options visibly shift by
 * the scrollbar's width (~15px) *after* the menu has finished opening.
 *
 * Measured before the fix: `.items` was 419px wide with no scrollbar while the
 * menu animated, then 404px once the scrollbar appeared.
 *
 * `scrollbar-gutter` does not inherit, so this cannot be fixed from a
 * stylesheet: the declaration has to be injected into the menu's shadow root.
 * It is injected on `opening`, i.e. before the animation starts, so no frame is
 * ever laid out without the gutter.
 *
 * `.items` is `md-menu`'s internal class name in `@material/web` 2.5.0; the
 * selector list is deliberately tolerant of a rename (the extra selectors are
 * harmless when they match nothing).
 *
 * @param select the `md-outlined-select` / `md-filled-select` element whose menu
 *   is about to open.
 */
export function reserveMenuScrollbar(select: Element): void {
  const menu = select.shadowRoot?.querySelector('md-menu')
  const root = menu?.shadowRoot
  if (!root) {
    return
  }
  if (root.querySelector('style[data-dl-menu-gutter]') !== null) {
    return
  }
  const style = document.createElement('style')
  style.setAttribute('data-dl-menu-gutter', '')
  style.textContent = '.items, .menu, [role="menu"], [role="listbox"] { scrollbar-gutter: stable; }'
  root.appendChild(style)
}
