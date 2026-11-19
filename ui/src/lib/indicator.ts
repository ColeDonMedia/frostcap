import type { CSSProperties } from "react";

/**
 * Position of a sliding selection indicator (segmented control thumb, tab
 * background, nav pill) using plain CSS transforms + a CSS transition, so the
 * app can use motion's smaller `domAnimation` feature set (no layout animations).
 *
 * Horizontal: the indicator is `position:absolute; left:pad` inside a container
 * with `padding: pad` and `gap` between `count` equal cells.
 */
export function slideStyle(index: number, count: number, opts: { axis?: "x" | "y"; padPx?: number; gapPx?: number; stepPx?: number } = {}): CSSProperties {
  if (index < 0) return { opacity: 0 };
  if (opts.axis === "y") return { transform: `translateY(${index * (opts.stepPx ?? 0)}px)` };
  const pad = opts.padPx ?? 3;
  const gap = opts.gapPx ?? 2;
  return {
    width: `calc((100% - ${pad * 2}px - ${(count - 1) * gap}px) / ${count})`,
    transform: `translateX(calc(${index} * (100% + ${gap}px)))`,
  };
}
