import { describe, expect, it } from "vitest";
import { addSpeedSegment, outputLength, outputTime, speedAt, resizeSpeed } from "./speed";
import type { Clip, SpeedSegment } from "../types";

const clip = (start: number, end: number): Clip => ({ id: `c${start}`, start, end });
const seg = (start: number, end: number, speed: number, id = `s${start}`): SpeedSegment => ({ id, start, end, speed });

describe("speedAt", () => {
  it("is 1 outside every segment and the segment speed inside (end exclusive)", () => {
    const s = [seg(1000, 2000, 2)];
    expect(speedAt(s, 500)).toBe(1);
    expect(speedAt(s, 1000)).toBe(2);
    expect(speedAt(s, 1999)).toBe(2);
    expect(speedAt(s, 2000)).toBe(1);
  });
});

describe("outputLength", () => {
  it("is the clip length when there is no speed segment", () => {
    expect(outputLength([clip(0, 10000)], [])).toBe(10000);
  });
  it("divides each part by its speed", () => {
    expect(outputLength([clip(0, 10000)], [seg(2000, 4000, 2)])).toBe(9000);
    expect(outputLength([clip(0, 10000)], [seg(0, 1000, 0.5)])).toBe(11000);
  });
  it("applies cuts first: a segment spanning a cut only affects what remains", () => {
    // kept 0-3000 and 5000-10000; 2x over 2000-6000 -> 2000 + 1000/2 + 1000/2 + 4000
    expect(outputLength([clip(0, 3000), clip(5000, 10000)], [seg(2000, 6000, 2)])).toBe(7000);
  });
});

describe("outputTime", () => {
  it("maps a source time through cuts and speed", () => {
    const clips = [clip(0, 10000)];
    const s = [seg(2000, 4000, 2)];
    expect(outputTime(clips, s, 1000)).toBe(1000);
    expect(outputTime(clips, s, 3000)).toBe(2500);
    expect(outputTime(clips, s, 6000)).toBe(5000);
  });
});

describe("addSpeedSegment / resizeSpeed", () => {
  it("adds a segment in free space and refuses to overlap", () => {
    const base = [seg(1000, 2000, 2)];
    expect(addSpeedSegment(base, 3000, 4000, 1.5, 10000)?.length).toBe(2);
    expect(addSpeedSegment(base, 1500, 2500, 1.5, 10000)).toBeNull();
  });
  it("lets segments touch", () => {
    expect(addSpeedSegment([seg(1000, 2000, 2)], 2000, 3000, 0.5, 10000)).not.toBeNull();
  });
  it("clamps speed to 0.25..4 and the range to the recording", () => {
    const r = addSpeedSegment([], 9000, 20000, 99, 10000)!;
    expect(r[0]).toMatchObject({ start: 9000, end: 10000, speed: 4 });
  });
  it("resizing stops at the neighbours and keeps a minimum length", () => {
    const list = [seg(0, 1000, 2, "a"), seg(2000, 3000, 2, "b")];
    expect(resizeSpeed(list, "a", { end: 5000 }, 10000).find((s) => s.id === "a")!.end).toBe(2000);
    expect(resizeSpeed(list, "b", { start: 0 }, 10000).find((s) => s.id === "b")!.start).toBe(1000);
    const tiny = resizeSpeed(list, "a", { end: 10 }, 10000).find((s) => s.id === "a")!;
    expect(tiny.end - tiny.start).toBeGreaterThanOrEqual(100);
  });
});

describe("moving a segment (both edges)", () => {
  it("keeps its length and stops at the neighbours", () => {
    const list = [seg(0, 1000, 2, "a"), seg(2000, 3000, 2, "b"), seg(5000, 6000, 2, "c")];
    const moved = resizeSpeed(list, "b", { start: 3500, end: 4500 }, 10000).find((s) => s.id === "b")!;
    expect([moved.start, moved.end]).toEqual([3500, 4500]);
    const blocked = resizeSpeed(list, "b", { start: 4500, end: 5500 }, 10000).find((s) => s.id === "b")!;
    expect([blocked.start, blocked.end]).toEqual([4000, 5000]);
    const left = resizeSpeed(list, "b", { start: -500, end: 500 }, 10000).find((s) => s.id === "b")!;
    expect([left.start, left.end]).toEqual([1000, 2000]);
  });
});

describe("resizeSpeed edge cases (review)", () => {
  it("rejects a move that does not fit between the neighbours instead of overlapping them", () => {
    const list = [seg(0, 1000, 2, "a"), seg(1500, 3000, 2, "b"), seg(3200, 4000, 2, "c")];
    // b is 1500 long but the gap a..c is only 2200 wide: moving right must not push its end past c.start.
    const moved = resizeSpeed(list, "b", { start: 1800, end: 3300 }, 10000);
    for (const s of moved) for (const o of moved) if (s !== o) expect(s.end <= o.start || o.end <= s.start).toBe(true);
    // A patch whose length does not fit the gap (1000..2200) is rejected: the list comes back unchanged.
    const tight = [seg(0, 1000, 2, "a"), seg(1100, 2100, 2, "b"), seg(2200, 3000, 2, "c")];
    expect(resizeSpeed(tight, "b", { start: 1100, end: 3000 }, 10000)).toBe(tight);
  });
  it("never produces an inverted clamp for a segment shorter than the minimum", () => {
    const list = [seg(0, 950, 2, "a"), seg(1000, 1050, 2, "b"), seg(1100, 3000, 2, "c")];
    const s = resizeSpeed(list, "b", { start: 900 }, 10000).find((x) => x.id === "b")!;
    expect(s.start).toBeGreaterThanOrEqual(950);
    expect(s.end).toBe(1050);
    const e = resizeSpeed(list, "b", { end: 5000 }, 10000).find((x) => x.id === "b")!;
    expect(e.end).toBeLessThanOrEqual(1100);
    expect(e.end).toBeGreaterThanOrEqual(1000);
  });
});
