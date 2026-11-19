import { describe, expect, it } from "vitest";
import { CAPTION_MAX_CHARS, CAPTION_MAX_LINES, activeCaption, canvasToContent, ease, effectiveCursor, webcamVisible, wrapCaption, zoomAt } from "./render";
import type { Project, ZoomSegment } from "../types";

describe("wrapCaption (security L1: bounded, linear, matches the Rust renderer)", () => {
  // Monospace stand-in for ctx.measureText: 1 unit per char; counts calls.
  let calls = 0;
  let measured = 0;
  const measure = (s: string) => {
    calls++;
    measured += s.length;
    return s.length;
  };
  const reset = () => ((calls = 0), (measured = 0));

  it("greedy word wrap, at most 3 lines", () => {
    expect(wrapCaption(measure, "one two three four", 9)).toEqual(["one two", "three", "four"]);
    expect(wrapCaption(measure, "a b c d e f g h", 1)).toHaveLength(CAPTION_MAX_LINES);
    expect(CAPTION_MAX_LINES).toBe(3);
  });

  it("breaks a word longer than a line instead of overflowing", () => {
    const lines = wrapCaption(measure, "hi " + "x".repeat(25), 10);
    expect(lines).toEqual(["hi", "xxxxxxxxxx", "xxxxxxxxxx"]);
    for (const l of wrapCaption(measure, "y".repeat(40), 7)) expect(l.length).toBeLessThanOrEqual(7);
  });

  it("renders at most 500 chars per cue", () => {
    expect(CAPTION_MAX_CHARS).toBe(500);
    const lines = wrapCaption(measure, "z".repeat(5000), 10_000);
    expect(lines.join("").length).toBe(500);
  });

  it("work is linear in the (capped) text, even for hostile input", () => {
    reset();
    wrapCaption(measure, "w".repeat(4096), 10_000);
    expect(measured).toBeLessThanOrEqual(4 * CAPTION_MAX_CHARS);
    reset();
    wrapCaption(measure, "a ".repeat(2048), 10_000);
    expect(measured).toBeLessThanOrEqual(4 * CAPTION_MAX_CHARS);
    reset();
    wrapCaption(measure, "q".repeat(4096), 3);
    expect(calls).toBeLessThanOrEqual(2 * CAPTION_MAX_CHARS);
  });

  it("empty / whitespace-only text yields no lines", () => {
    expect(wrapCaption(measure, "   ", 10)).toEqual([]);
  });
});

// Values mirror the unit tests in crates/frostcap-render/src/math.rs so the
// preview and the exporter agree.
describe("easing parity with math.rs", () => {
  it("endpoints and known values", () => {
    for (const e of ["linear", "easeInOutCubic", "easeOutQuint", "spring"] as const) {
      expect(Math.abs(ease(e, 0))).toBeLessThan(1e-12);
      expect(ease(e, 1)).toBeCloseTo(1, 12);
    }
    expect(ease("easeInOutCubic", 0.5)).toBeCloseTo(0.5, 12);
    expect(ease("easeInOutCubic", 0.25)).toBeCloseTo(0.0625, 12);
    expect(ease("easeOutCubic", 0.5)).toBeCloseTo(0.875, 12);
    expect(ease("easeOutQuint", 0.5)).toBeCloseTo(1 - 0.5 ** 5, 12);
    expect(ease("linear", 0.3)).toBeCloseTo(0.3, 12);
    // spring overshoots
    expect(Math.max(...Array.from({ length: 100 }, (_, i) => ease("spring", i / 100)))).toBeGreaterThan(1);
  });
});

describe("canvasToContent (preview click -> normalized focus)", () => {
  const base = {
    media: { width: 1920, height: 1080 },
    style: { padding: 0, frame: "none" },
    zoomSegments: [] as ZoomSegment[],
  } as unknown as Project;

  it("maps through the layout when not zoomed", () => {
    const p = canvasToContent(960, 540, 1920, 1080, base, 0);
    expect(p.x).toBeCloseTo(0.5, 6);
    expect(p.y).toBeCloseTo(0.5, 6);
    expect(canvasToContent(0, 0, 1920, 1080, base, 0)).toEqual({ x: 0, y: 0 });
  });

  it("inverts the zoom transform while zoomed in", () => {
    const seg: ZoomSegment = { id: "z", start: 0, end: 4000, cx: 0.3, cy: 0.4, scale: 2, easing: "linear", transitionMs: 0 };
    const p = { ...base, zoomSegments: [seg] } as Project;
    // At full zoom the canvas centre shows the (clamped) focus point.
    const c = canvasToContent(960, 540, 1920, 1080, p, 2000);
    expect(c.x).toBeCloseTo(0.3, 6);
    expect(c.y).toBeCloseTo(0.4, 6);
    // Moving 192 px right on screen is 96 px (0.05) in content at 2x.
    expect(canvasToContent(960 + 192, 540, 1920, 1080, p, 2000).x).toBeCloseTo(0.35, 6);
  });
});

describe("effectiveCursor (export semantics)", () => {
  const p = (cursor: Partial<Project["style"]["cursor"]>, cursorBaked = false) =>
    ({ cursorBaked, style: { cursor: { visible: true, smooth: true, smoothing: 0.6, size: 1, clickRipple: true, motionBlur: 0.3, ...cursor } } }) as unknown as Project;
  it("smoothing applies only while the switch is on", () => {
    expect(effectiveCursor(p({})).smoothing).toBe(0.6);
    expect(effectiveCursor(p({ smooth: false })).smoothing).toBe(0);
  });
  it("hidden or baked cursors are never drawn", () => {
    expect(effectiveCursor(p({ visible: false })).draw).toBe(false);
    expect(effectiveCursor(p({}, true)).draw).toBe(false);
    expect(effectiveCursor(p({})).draw).toBe(true);
  });
});

describe("captions and webcam follow the exporter", () => {
  const proj = (captions: { start: number; end: number; text: string }[], media: object, enabled = true) =>
    ({ captions: captions.map((c, i) => ({ id: String(i), ...c })), media, style: { captions: { enabled }, webcam: { enabled: true } } }) as unknown as Project;
  it("skips cues with no visible text or zero length", () => {
    const p = proj([{ start: 0, end: 0, text: "zero" }, { start: 0, end: 500, text: "   " }, { start: 0, end: 500, text: "hi" }], {});
    expect(activeCaption(p, 0)?.text).toBe("hi");
    expect(activeCaption(proj([{ start: 0, end: 500, text: "hi" }], {}, false), 0)).toBeNull();
  });
  it("draws the webcam bubble only when there is a webcam stream (demo projects always show the placeholder)", () => {
    expect(webcamVisible(proj([], { screen: { url: "asset://x" }, webcam: null }))).toBe(false);
    expect(webcamVisible(proj([], { screen: { url: "asset://x" }, webcam: { url: "asset://w" } }))).toBe(true);
    expect(webcamVisible(proj([], { screen: { url: null }, webcam: null }))).toBe(true);
  });
});

describe("zoomAt parity with zoom_at()", () => {
  const seg = (start: number, end: number, transitionMs: number): ZoomSegment => ({ id: "s", start, end, cx: 0.5, cy: 0.5, scale: 2, easing: "linear", transitionMs });
  it("ramps, holds and has an inclusive end", () => {
    const s = [seg(1000, 4000, 600)];
    expect(zoomAt(s, 999).seg).toBeNull();
    expect(zoomAt(s, 1000).e).toBe(0);
    expect(zoomAt(s, 1300).e).toBeCloseTo(0.5, 12);
    expect(zoomAt(s, 2500).e).toBe(1);
    expect(zoomAt(s, 4000).e).toBe(0);
    expect(zoomAt(s, 4001).seg).toBeNull();
  });
  it("caps the ramp at half the segment and handles zero transition", () => {
    expect(zoomAt([seg(0, 400, 1000)], 200).e).toBe(1);
    expect(zoomAt([seg(0, 400, 0)], 0).e).toBe(1);
  });
});
