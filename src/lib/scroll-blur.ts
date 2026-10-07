/**
 * Scroll motion blur: a vertical smear while the log table is moving fast.
 *
 * The whole module is presentation and is *read-only* with respect to scrolling: it samples
 * `scrollTop` and writes one SVG attribute. It never writes `scrollTop`, never measures layout
 * per frame, and never touches the follow algorithm (DESIGN 5.6.2) — that algorithm stays the
 * only thing that decides where the table is.
 *
 * How it works:
 *
 * * A `requestAnimationFrame` loop samples `scrollTop` and derives a speed in px/s. Sampling is
 *   frame-locked rather than `setInterval`-based, so it stops when the window does.
 * * The speed goes through an asymmetric low-pass: it rises with a ~40 ms time constant and
 *   falls with a ~120 ms one, so the blur appears with the gesture and leaves without trailing.
 * * Speed maps to a blur radius: nothing below 400 px/s, then linear to 2.0 px at 3000 px/s.
 *   The radius is quantised to 0.2 px and only written when it changes.
 * * The radius is written straight into the `feGaussianBlur` element via `setAttribute` — no
 *   React state, so no re-render per frame.
 * * The filter is attached to the wrapped *visible rows* layer, never to the scroll container
 *   (that would blur the scrollbar) and never to the full-height spacer (the filter region
 *   would cover the whole virtual height). It is attached with hysteresis — at σ ≥ 0.4 — and
 *   removed at σ < 0.15, so the layer spends its idle time with no filter, no `will-change`
 *   and no extra compositing layer.
 * * Nothing is blurred when: the window is hidden, the user prefers reduced motion, the layer's
 *   height changed since the last frame (a panel opening or a window resize), or a single frame
 *   moved more than 1.5 viewports (the follow algorithm's "jump more than six screens" and the
 *   catch-up after the window was hidden).
 *
 * Only vertical blur, and only for HTML elements known to support `filter: url()`: the caller
 * decides whether to attach at all (see `SCROLL_BLUR_VERIFIED`).
 */

/** Speed below which there is no blur at all (px/s). */
const SPEED_FLOOR = 400
/** Speed at which the blur reaches its ceiling (px/s). */
const SPEED_CEIL = 3000
/** Largest blur radius, in px. */
const SIGMA_MAX = 2.0
/** Quantisation step for the radius, in px. */
const SIGMA_STEP = 0.2
/** Radius at which the filter is attached, and at which it is removed again. */
const SIGMA_ATTACH = 0.4
const SIGMA_DETACH = 0.15
/** Low-pass time constants, in seconds: fast in, slow out. */
const RISE_SECONDS = 0.04
const FALL_SECONDS = 0.12
/** A frame that moves more than this many viewports is a jump, not scrolling. */
const JUMP_VIEWPORTS = 1.5

/**
 * Whether `filter: url()` on an HTML element has been verified on this platform's WebView.
 *
 * It is measured to work on Windows (WebView2). macOS and Linux are unverified, and the rules
 * for this feature say unverified platforms stay off rather than falling back to something
 * else — a fallback would be a different effect shipped under the same name.
 */
export const SCROLL_BLUR_VERIFIED: boolean =
  typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows')

export interface ScrollBlurHandle {
  /** Stops sampling and removes the filter. */
  detach: () => void
}

export interface ScrollBlurOptions {
  /** The scroll container: read-only `scrollTop` sampling. */
  scroller: HTMLElement
  /** The layer the filter is applied to — the wrapped visible rows, not the scroller. */
  layer: HTMLElement
  /** The `feGaussianBlur` element whose `stdDeviation` is written. */
  blur: SVGFEGaussianBlurElement
  /** Set false to keep the sampler from ever being installed. */
  enabled: boolean
}

/** Maps a speed to a quantised radius. */
function sigmaFor(speed: number): number {
  if (speed <= SPEED_FLOOR) {
    return 0
  }
  const raw = Math.min(SIGMA_MAX, ((speed - SPEED_FLOOR) / (SPEED_CEIL - SPEED_FLOOR)) * SIGMA_MAX)
  return Math.round(raw / SIGMA_STEP) * SIGMA_STEP
}

/** The one-line filter reference; the `defs` live in the component. */
const FILTER_VALUE = 'url(#dl-scroll-blur)'

/**
 * Starts sampling. Returns a handle whose `detach` restores the layer exactly as it was.
 *
 * When `enabled` is false nothing is installed at all — not a listener, not a frame loop.
 */
export function attachScrollBlur(options: ScrollBlurOptions): ScrollBlurHandle {
  const { scroller, layer, blur, enabled } = options
  const noop: ScrollBlurHandle = { detach: () => undefined }
  if (!enabled) {
    return noop
  }
  if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    return noop
  }

  let frame = 0
  let lastTop = scroller.scrollTop
  let lastAt = performance.now()
  let smoothed = 0
  let attached = false
  let lastApplied = -1
  // The *viewport* height, not the layer's: during a flood of logs the rows mount and unmount
  // constantly, so the layer's own height changes every few frames — and using it here switched
  // the blur off in exactly the situation it exists for. The rule is about the viewport changing
  // (a panel opening, a window resize), and that is `clientHeight` of the scroll container.
  let lastViewport = scroller.clientHeight

  const setSigma = (sigma: number): void => {
    if (sigma === lastApplied) {
      return
    }
    lastApplied = sigma
    blur.setAttribute('stdDeviation', `0 ${sigma}`)
  }

  const apply = (sigma: number): void => {
    if (sigma >= SIGMA_ATTACH && !attached) {
      attached = true
      layer.style.filter = FILTER_VALUE
    } else if (sigma < SIGMA_DETACH && attached) {
      attached = false
      layer.style.filter = ''
      setSigma(0)
    }
    if (attached) {
      setSigma(sigma)
    }
  }

  const tick = (): void => {
    frame = window.requestAnimationFrame(tick)
    const now = performance.now()
    const top = scroller.scrollTop
    const elapsed = Math.max(1, now - lastAt) / 1000
    const moved = top - lastTop
    lastTop = top
    lastAt = now

    const viewport = scroller.clientHeight
    const viewportChanged = viewport !== lastViewport
    lastViewport = viewport

    if (document.hidden || viewportChanged || Math.abs(moved) > viewport * JUMP_VIEWPORTS) {
      // A jump, a re-layout or a hidden window: no motion to describe. The filter is left to
      // fall off through the normal path rather than being snapped, so nothing flickers.
      smoothed = 0
      apply(0)
      return
    }

    const speed = Math.abs(moved) / elapsed
    const target = sigmaFor(speed)
    // Asymmetric low-pass: quick to appear, unhurried to leave.
    const tau = target > smoothed ? RISE_SECONDS : FALL_SECONDS
    const alpha = 1 - Math.exp(-elapsed / tau)
    smoothed += (target - smoothed) * alpha
    apply(smoothed)
  }

  frame = window.requestAnimationFrame(tick)
  return {
    detach: () => {
      window.cancelAnimationFrame(frame)
      if (attached) {
        layer.style.filter = ''
      }
      blur.setAttribute('stdDeviation', '0 0')
    },
  }
}
