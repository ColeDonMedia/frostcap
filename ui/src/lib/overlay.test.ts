// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { drawAnnotations, webcamRect, type Content } from "./overlay";
import { newAnnotation } from "./annotations";
import { ANNOTATION_KINDS } from "./annotations";
import { defaultStyle } from "./presets";

/** A recording stand-in for CanvasRenderingContext2D: every method is a no-op that logs its name. */
function fakeCtx() {
  const calls: { name: string; args: unknown[] }[] = [];
  const state: Record<string, unknown> = {};
  const ctx = new Proxy(state, {
    get(t, k: string) {
      if (k in t) return t[k];
      if (k === "measureText") return (s: string) => ({ width: s.length * 10 });
      if (k === "createLinearGradient") return () => ({ addColorStop() {} });
      return (...args: unknown[]) => void calls.push({ name: k, args });
    },
    set(t, k: string, v) {
      t[k] = v;
      return true;
    },
  }) as unknown as CanvasRenderingContext2D;
  return { ctx, calls, state };
}

const content: Content = { x: 100, y: 50, w: 800, h: 450 };
const src = { width: 1600, height: 900 } as unknown as CanvasImageSource;

describe("drawAnnotations", () => {
  it("draws each of the eight kinds without error and reports how many it drew", () => {
    const { ctx } = fakeCtx();
    const list = ANNOTATION_KINDS.map((k, i) => newAnnotation(k, `a${i}`, 0, 5000, { x: 0.1, y: 0.1 }, { x: 0.5, y: 0.4 }));
    expect(drawAnnotations(ctx, list, 2500, content, 1, 1, src)).toBe(8);
  });

  it("never draws more than 8 at once and skips inactive ones", () => {
    const { ctx } = fakeCtx();
    const list = Array.from({ length: 12 }, (_, i) => newAnnotation("rect", `a${i}`, i, 5000, { x: 0, y: 0 }, { x: 0.2, y: 0.2 }));
    expect(drawAnnotations(ctx, list, 2500, content, 1, 1, src)).toBe(8);
    expect(drawAnnotations(ctx, list, 9000, content, 1, 1, src)).toBe(0);
  });

  it("maps content coordinates onto the content rect", () => {
    const { ctx, calls } = fakeCtx();
    drawAnnotations(ctx, [newAnnotation("arrow", "a", 0, 5000, { x: 0, y: 0 }, { x: 1, y: 1 }, { fadeMs: 0 })], 100, content, 1, 1, src);
    const move = calls.find((c) => c.name === "moveTo")!;
    const line = calls.find((c) => c.name === "lineTo")!;
    expect(move.args).toEqual([100, 50]);
    expect(line.args[0]).toBeGreaterThan(800); // the shaft stops at the arrow head base, near the end point
    expect(line.args[0]).toBeLessThan(900);
  });

  it("does not scale stroke widths with the zoom", () => {
    const a = fakeCtx();
    const b = fakeCtx();
    const r = newAnnotation("rect", "a", 0, 5000, { x: 0.1, y: 0.1 }, { x: 0.5, y: 0.4 }, { fadeMs: 0, strokeWidth: 8 });
    drawAnnotations(a.ctx, [r], 100, content, 1, 1, src);
    drawAnnotations(b.ctx, [r], 100, content, 1, 2, src);
    expect(a.state.lineWidth).toBe(8);
    expect(b.state.lineWidth).toBe(4);
  });

  it("writes the text of a text annotation", () => {
    const { ctx, calls } = fakeCtx();
    drawAnnotations(ctx, [newAnnotation("text", "t", 0, 5000, { x: 0.1, y: 0.1 }, { x: 0.6, y: 0.3 }, { text: "Look here", fadeMs: 0 })], 100, content, 1, 1, src);
    expect(calls.filter((c) => c.name === "fillText").map((c) => c.args[0])).toContain("Look here");
  });

  it("spotlight dims only inside the content rect (clipped)", () => {
    const { ctx, calls } = fakeCtx();
    drawAnnotations(ctx, [newAnnotation("spotlight", "s", 0, 5000, { x: 0.3, y: 0.3 }, { x: 0.6, y: 0.6 }, { fadeMs: 0 })], 100, content, 1, 1, src);
    const names = calls.map((c) => c.name);
    expect(names).toContain("clip");
    expect(calls.find((c) => c.name === "fill")!.args[0]).toBe("evenodd");
  });
});

describe("webcamRect", () => {
  it("uses the margin (fraction of the output height) and the corner", () => {
    const st = defaultStyle().webcam;
    const r = webcamRect(1920, 1080, { ...st, margin: 0.05, size: 0.2, position: "bottom-right" });
    expect(r.size).toBeCloseTo(216);
    expect(r.x).toBeCloseTo(1920 - 54 - 216);
    expect(r.y).toBeCloseTo(1080 - 54 - 216);
    const tl = webcamRect(1920, 1080, { ...st, margin: 0.05, size: 0.2, position: "top-left" });
    expect([tl.x, tl.y]).toEqual([54, 54]);
  });
});
