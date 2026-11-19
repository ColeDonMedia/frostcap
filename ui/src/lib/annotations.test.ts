import { describe, expect, it } from "vitest";
import { ANNOTATION_KINDS, annotationAlpha, applyDrag, duplicateAnnotation, handlesOf, hitTest, newAnnotation, visibleAnnotations } from "./annotations";
import type { Annotation } from "../types";

const rect = (over: Partial<Annotation> = {}): Annotation => newAnnotation("rect", "a", 1000, 3000, { x: 0.2, y: 0.2 }, { x: 0.6, y: 0.5 }, over);

describe("newAnnotation", () => {
  it("lists the eight tools", () => {
    expect(ANNOTATION_KINDS).toEqual(["arrow", "text", "rect", "ellipse", "highlight", "spotlight", "blur", "pixelate"]);
  });
  it("builds a normalized box from a drag in any direction", () => {
    const a = newAnnotation("rect", "a", 0, 1000, { x: 0.6, y: 0.5 }, { x: 0.2, y: 0.2 });
    expect(a).toMatchObject({ x: 0.2, y: 0.2 });
    expect(a.w).toBeCloseTo(0.4);
    expect(a.h).toBeCloseTo(0.3);
  });
  it("keeps arrow start and end as drawn (point to point)", () => {
    const a = newAnnotation("arrow", "a", 0, 1000, { x: 0.8, y: 0.1 }, { x: 0.2, y: 0.9 });
    expect(a).toMatchObject({ x: 0.8, y: 0.1, x2: 0.2, y2: 0.9 });
  });
  it("gives a click (no drag) a sensible default size and stays inside 0..1", () => {
    const a = newAnnotation("ellipse", "a", 0, 1000, { x: 0.98, y: 0.98 }, { x: 0.98, y: 0.98 });
    expect(a.w).toBeGreaterThan(0.05);
    expect(a.x + a.w).toBeLessThanOrEqual(1);
    expect(a.y + a.h).toBeLessThanOrEqual(1);
  });
  it("uses schema defaults and per-tool presets", () => {
    const r = rect();
    expect(r).toMatchObject({ color: "#4ed2fb", strokeWidth: 4, fill: false, opacity: 1, strength: 0.5, dim: 0.6, fadeMs: 150, fontSize: 40 });
    expect(newAnnotation("highlight", "h", 0, 1, { x: 0, y: 0 }, { x: 0.3, y: 0.1 })).toMatchObject({ fill: true, opacity: 0.4 });
    expect(newAnnotation("text", "t", 0, 1, { x: 0, y: 0 }, { x: 0.3, y: 0.1 }).text).not.toBe("");
  });
});

describe("visibility and fade", () => {
  const list = Array.from({ length: 10 }, (_, i) => newAnnotation("rect", `a${i}`, i * 10, 10_000, { x: 0, y: 0 }, { x: 0.1, y: 0.1 }));
  it("draws at most 8 at once: the first 8 by start time", () => {
    expect(visibleAnnotations(list, 5000).map((a) => a.id)).toEqual(["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"]);
  });
  it("is active from start (inclusive) to end (exclusive); zero length never", () => {
    const a = rect({ start: 1000, end: 2000 });
    expect(visibleAnnotations([a], 999)).toEqual([]);
    expect(visibleAnnotations([a], 1000)).toHaveLength(1);
    expect(visibleAnnotations([a], 2000)).toEqual([]);
    expect(visibleAnnotations([rect({ start: 1000, end: 1000 })], 1000)).toEqual([]);
  });
  it("fades in and out over fadeMs and scales by opacity", () => {
    const a = rect({ start: 1000, end: 3000, fadeMs: 200, opacity: 0.5 });
    expect(annotationAlpha(a, 1000)).toBe(0);
    expect(annotationAlpha(a, 1100)).toBeCloseTo(0.25);
    expect(annotationAlpha(a, 2000)).toBeCloseTo(0.5);
    expect(annotationAlpha(a, 2900)).toBeCloseTo(0.25);
    expect(annotationAlpha(rect({ fadeMs: 0 }), 1000)).toBe(1);
  });
});

describe("hit testing and dragging", () => {
  it("offers 8 handles for boxes and 2 for arrows", () => {
    expect(handlesOf(rect())).toHaveLength(8);
    expect(handlesOf(newAnnotation("arrow", "x", 0, 1, { x: 0.1, y: 0.1 }, { x: 0.5, y: 0.5 })).map((h) => h.handle)).toEqual(["start", "end"]);
  });
  it("hits handles before the body, and the body inside the box", () => {
    const a = rect();
    expect(hitTest(a, 0.2, 0.2, 0.02)).toBe("nw");
    expect(hitTest(a, 0.6, 0.5, 0.02)).toBe("se");
    expect(hitTest(a, 0.4, 0.35, 0.02)).toBe("body");
    expect(hitTest(a, 0.9, 0.9, 0.02)).toBeNull();
  });
  it("hits an arrow along its shaft", () => {
    const a = newAnnotation("arrow", "x", 0, 1, { x: 0.1, y: 0.1 }, { x: 0.5, y: 0.5 });
    expect(hitTest(a, 0.3, 0.3, 0.02)).toBe("body");
    expect(hitTest(a, 0.1, 0.1, 0.02)).toBe("start");
    expect(hitTest(a, 0.9, 0.1, 0.02)).toBeNull();
  });
  it("moves a box and clamps it inside the content", () => {
    const a = applyDrag(rect(), "body", 0.7, 0.7);
    expect(a.x + a.w).toBeCloseTo(1);
    expect(a.y + a.h).toBeCloseTo(1);
    expect(a.w).toBeCloseTo(0.4);
  });
  it("resizes from a corner and keeps a minimum size, never flipping", () => {
    const a = applyDrag(rect(), "se", -0.9, -0.9);
    expect(a.x).toBeCloseTo(0.2);
    expect(a.w).toBeGreaterThan(0);
    const b = applyDrag(rect(), "nw", 0.9, 0.9);
    expect(b.x + b.w).toBeCloseTo(0.6);
    expect(b.w).toBeGreaterThan(0);
  });
  it("moves one arrow end or the whole arrow", () => {
    const a = newAnnotation("arrow", "x", 0, 1, { x: 0.1, y: 0.1 }, { x: 0.5, y: 0.5 });
    expect(applyDrag(a, "end", 0.2, 0)).toMatchObject({ x: 0.1, x2: 0.7 });
    const m = applyDrag(a, "body", 0.1, 0.1);
    expect(m).toMatchObject({ x: 0.2, y: 0.2 });
    expect(m.x2).toBeCloseTo(0.6);
    expect(applyDrag(a, "body", 5, 0).x2).toBe(1); // clamps without distorting the shape
  });
});

describe("duplicateAnnotation", () => {
  it("copies with a new id, offset a little and kept inside 0..1", () => {
    const a = rect({ x: 0.9, w: 0.1 });
    const d = duplicateAnnotation(a, "new");
    expect(d.id).toBe("new");
    expect(d.kind).toBe(a.kind);
    expect(d.x + d.w).toBeLessThanOrEqual(1);
  });
});
