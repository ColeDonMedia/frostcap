/** Annotation model helpers: creation, fading, picking and dragging (pure; the canvas drawing lives in lib/render.ts). */
import type { Annotation, AnnotationKind } from "../types";
import { LIMITS } from "../backend";
import { clamp } from "./format";

export const ANNOTATION_KINDS: AnnotationKind[] = ["arrow", "text", "rect", "ellipse", "highlight", "spotlight", "blur", "pixelate"];

const MIN_SIZE = 0.02;
type P = { x: number; y: number };

const DEFAULT_SIZE: Record<AnnotationKind, [number, number]> = {
  arrow: [0.15, 0.1],
  text: [0.3, 0.12],
  rect: [0.2, 0.15],
  ellipse: [0.2, 0.15],
  highlight: [0.25, 0.08],
  spotlight: [0.25, 0.2],
  blur: [0.2, 0.12],
  pixelate: [0.2, 0.12],
};

const bbox = (a: Pick<Annotation, "x" | "y" | "x2" | "y2">) => ({ x: Math.min(a.x, a.x2), y: Math.min(a.y, a.y2), w: Math.abs(a.x2 - a.x), h: Math.abs(a.y2 - a.y) });

/** A new annotation from a drag between two content points (0..1). A plain click gets a default size. */
export function newAnnotation(kind: AnnotationKind, id: string, start: number, end: number, p1: P, p2: P, over: Partial<Annotation> = {}): Annotation {
  const [dw, dh] = DEFAULT_SIZE[kind];
  let { x, y } = p1;
  let { x: x2, y: y2 } = p2;
  const clicked = Math.abs(x2 - x) < MIN_SIZE && Math.abs(y2 - y) < MIN_SIZE;
  let box: { x: number; y: number; w: number; h: number };
  if (kind === "arrow") {
    if (clicked) {
      x2 = Math.min(1, x + dw);
      y2 = Math.min(1, y + dh);
    }
    box = bbox({ x, y, x2, y2 });
  } else if (clicked) {
    box = { x: clamp(x, 0, 1 - dw), y: clamp(y, 0, 1 - dh), w: dw, h: dh };
    x = box.x;
    y = box.y;
    x2 = box.x + dw;
    y2 = box.y + dh;
  } else {
    box = bbox({ x, y, x2, y2 });
  }
  const base: Annotation = {
    id,
    kind,
    start,
    end,
    x: kind === "arrow" ? x : box.x,
    y: kind === "arrow" ? y : box.y,
    w: box.w,
    h: box.h,
    x2: kind === "arrow" ? x2 : box.x + box.w,
    y2: kind === "arrow" ? y2 : box.y + box.h,
    color: "#4ed2fb",
    strokeWidth: 4,
    fill: false,
    text: kind === "text" ? "Text" : "",
    fontSize: 40,
    opacity: 1,
    strength: 0.5,
    dim: 0.6,
    cornerRadius: 0,
    fadeMs: 150,
  };
  if (kind === "highlight") Object.assign(base, { fill: true, opacity: 0.4, color: "#ffe14d" });
  return { ...base, ...over };
}

/** Active at t (start inclusive, end exclusive), the first 8 by start time (what the exporter draws). */
export function visibleAnnotations(list: Annotation[], t: number): Annotation[] {
  return list
    .filter((a) => a.end > a.start && t >= a.start && t < a.end)
    .sort((a, b) => a.start - b.start)
    .slice(0, LIMITS.maxAnnotationsDrawn);
}

/** Fade in and out over `fadeMs` (at most half the duration), times opacity. */
export function annotationAlpha(a: Annotation, t: number): number {
  if (a.end <= a.start) return 0;
  const fade = Math.min(a.fadeMs, (a.end - a.start) / 2);
  const k = fade > 0 ? Math.min(1, (t - a.start) / fade, (a.end - t) / fade) : 1;
  return clamp(k, 0, 1) * a.opacity;
}

export type Handle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w" | "start" | "end" | "body";

export function handlesOf(a: Annotation): { handle: Handle; x: number; y: number }[] {
  if (a.kind === "arrow")
    return [
      { handle: "start", x: a.x, y: a.y },
      { handle: "end", x: a.x2, y: a.y2 },
    ];
  const r = a.x + a.w;
  const b = a.y + a.h;
  const mx = a.x + a.w / 2;
  const my = a.y + a.h / 2;
  return [
    { handle: "nw", x: a.x, y: a.y },
    { handle: "n", x: mx, y: a.y },
    { handle: "ne", x: r, y: a.y },
    { handle: "e", x: r, y: my },
    { handle: "se", x: r, y: b },
    { handle: "s", x: mx, y: b },
    { handle: "sw", x: a.x, y: b },
    { handle: "w", x: a.x, y: my },
  ];
}

function distToSegment(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  const k = len2 ? clamp(((px - ax) * dx + (py - ay) * dy) / len2, 0, 1) : 0;
  return Math.hypot(px - (ax + k * dx), py - (ay + k * dy));
}

/** Handle under the point (within `tol`), else "body" when on the shape, else null. */
export function hitTest(a: Annotation, x: number, y: number, tol: number): Handle | null {
  for (const h of handlesOf(a)) if (Math.abs(h.x - x) <= tol && Math.abs(h.y - y) <= tol) return h.handle;
  if (a.kind === "arrow") return distToSegment(x, y, a.x, a.y, a.x2, a.y2) <= tol ? "body" : null;
  return x >= a.x && x <= a.x + a.w && y >= a.y && y <= a.y + a.h ? "body" : null;
}

/** Moves the whole annotation or one handle by (dx, dy) in content units; always stays inside 0..1. */
export function applyDrag(a: Annotation, handle: Handle, dx: number, dy: number): Annotation {
  if (a.kind === "arrow") {
    if (handle === "start") return withBox({ ...a, x: clamp(a.x + dx, 0, 1), y: clamp(a.y + dy, 0, 1) });
    if (handle === "end") return withBox({ ...a, x2: clamp(a.x2 + dx, 0, 1), y2: clamp(a.y2 + dy, 0, 1) });
    const mx = clamp(dx, -Math.min(a.x, a.x2), 1 - Math.max(a.x, a.x2));
    const my = clamp(dy, -Math.min(a.y, a.y2), 1 - Math.max(a.y, a.y2));
    return withBox({ ...a, x: a.x + mx, y: a.y + my, x2: a.x2 + mx, y2: a.y2 + my });
  }
  if (handle === "body") return boxed(a, clamp(a.x + dx, 0, 1 - a.w), clamp(a.y + dy, 0, 1 - a.h), a.w, a.h);
  let left = a.x;
  let top = a.y;
  let right = a.x + a.w;
  let bottom = a.y + a.h;
  if (handle.includes("w")) left = clamp(left + dx, 0, right - MIN_SIZE);
  if (handle.includes("e")) right = clamp(right + dx, left + MIN_SIZE, 1);
  if (handle.includes("n")) top = clamp(top + dy, 0, bottom - MIN_SIZE);
  if (handle.includes("s")) bottom = clamp(bottom + dy, top + MIN_SIZE, 1);
  return boxed(a, left, top, right - left, bottom - top);
}

function boxed(a: Annotation, x: number, y: number, w: number, h: number): Annotation {
  return { ...a, x, y, w, h, x2: x + w, y2: y + h };
}

function withBox(a: Annotation): Annotation {
  const b = bbox(a);
  return { ...a, w: b.w, h: b.h };
}

/** A copy offset a little (still inside 0..1) with a new id. */
export function duplicateAnnotation(a: Annotation, id: string): Annotation {
  const off = 0.02;
  if (a.kind === "arrow") {
    const d = clamp(off, 0, Math.min(1 - a.x, 1 - a.x2, 1 - a.y, 1 - a.y2));
    return withBox({ ...a, id, x: a.x + d, y: a.y + d, x2: a.x2 + d, y2: a.y2 + d });
  }
  return boxed({ ...a, id }, clamp(a.x + off, 0, 1 - a.w), clamp(a.y + off, 0, 1 - a.h), a.w, a.h);
}
