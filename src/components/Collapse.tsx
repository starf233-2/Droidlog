/**
 * The one expand/collapse mechanism in the app.
 *
 * Every collapsible surface — the target card's app lists, the collection report, the
 * timeline panel and its "展开全部" groups — uses this component. There is deliberately no
 * second implementation: two copies drift, and the drift shows up as two different feels.
 *
 * How it works, and why:
 *
 * * The box animates `grid-template-rows` between `0fr` and `1fr`. The browser interpolates
 *   that itself, so there is no measured height, no `max-height` guess, and no per-frame JS.
 *   `height` and `max-height` are never animated.
 * * The inner wrapper is `min-height: 0; overflow: hidden`, and it carries no padding or
 *   margin — those live on the children, or a closed panel would keep the padding's height.
 * * A mid-flight reversal continues from wherever the box currently is, because CSS
 *   interpolates from the computed value. Nothing here sets a height, so nothing can jump.
 * * Expanded *and settled* restores `overflow: visible` on the inner wrapper, so a
 *   `focus-visible` ring is never clipped by a container that is no longer animating.
 * * Closed and settled makes the content `visibility: hidden` + `inert`: not focusable, not
 *   tabbable, zero height. The `inert` attribute is removed again on open.
 * * Settling is decided by `transitionend` on `grid-template-rows` (checking the property and
 *   that the event is the box's own), with a fallback timer for the case where no transition
 *   event arrives — a fallback, not a guess at the duration.
 * * The first render never animates: `data-anim` is only set after the initial layout, and a
 *   long list therefore mounts and lays out completely before the box starts opening.
 * * Only `grid-template-rows`, `opacity` and `transform` are animated. No bounce, no
 *   overshoot, no scale, and no gradient or mask anywhere near it.
 *
 * Durations and curves come from `styles/tokens.css`; nothing here hard-codes them.
 */

import type { JSX, ReactNode } from 'react'
import { useCallback, useEffect, useRef, useState } from 'react'

/** Milliseconds to wait for a `transitionend` before settling anyway. */
const FALLBACK_SETTLE_MS = 450

export function Collapse({
  open,
  children,
  className,
}: {
  /** Whether the content should be shown. */
  open: boolean
  children: ReactNode
  /** Extra classes for the outer box. */
  className?: string
}): JSX.Element {
  // `rendered` follows `open`, but a long list is mounted and laid out first and only then
  // gets `1fr` on the next frame — otherwise the first animated frame is also the frame that
  // builds 400 rows, and the animation stutters.
  const [rendered, setRendered] = useState(open)
  const [animating, setAnimating] = useState(false)
  const [settled, setSettled] = useState(true)
  const boxRef = useRef<HTMLDivElement | null>(null)
  const fallbackRef = useRef(0)

  useEffect(() => {
    // After the first paint the component may animate; before that it must not, or the
    // opening panel would animate from an unlaid-out state on mount.
    const raf = window.requestAnimationFrame(() => {
      setAnimating(true)
    })
    return () => window.cancelAnimationFrame(raf)
  }, [])

  useEffect(() => {
    if (open === rendered) {
      return
    }
    setSettled(false)
    window.clearTimeout(fallbackRef.current)
    fallbackRef.current = window.setTimeout(() => {
      setRendered(open)
      setSettled(true)
    }, FALLBACK_SETTLE_MS)
    const raf = window.requestAnimationFrame(() => {
      setRendered(open)
    })
    return () => window.cancelAnimationFrame(raf)
  }, [open, rendered])

  // Settling: the box's own `grid-template-rows` transition, and nothing else.
  const onTransitionEnd = useCallback(
    (event: React.TransitionEvent<HTMLDivElement>) => {
      if (event.target !== event.currentTarget || event.propertyName !== 'grid-template-rows') {
        return
      }
      window.clearTimeout(fallbackRef.current)
      setSettled(true)
    },
    [],
  )

  const state = rendered ? 'open' : 'closed'
  const isSettled = settled && open === rendered

  return (
    <div
      ref={boxRef}
      className={className === undefined ? 'dl-collapse' : `dl-collapse ${className}`}
      data-state={state}
      // Animating is opt-in: without this attribute the box is simply at its state.
      data-anim={animating ? 'on' : 'off'}
      // Only an *open and settled* box may show overflow: a focus ring must not be clipped,
      // and during the animation the overflow has to stay hidden.
      data-settled={isSettled ? 'yes' : 'no'}
      onTransitionEnd={onTransitionEnd}
    >
      <div className="dl-collapse__inner">
        <div className="dl-collapse__content" data-state={state} inert={!rendered}>
          {children}
        </div>
      </div>
    </div>
  )
}

/**
 * The arrow that belongs to a collapse toggle.
 *
 * It rotates with the same tokens as the box, so the two read as one gesture. Kept here so a
 * call site cannot invent a second rotation curve.
 */
export function CollapseArrow({ open }: { open: boolean }): JSX.Element {
  return (
    <span className="dl-collapse__arrow" data-state={open ? 'open' : 'closed'} aria-hidden="true">
      ▾
    </span>
  )
}
