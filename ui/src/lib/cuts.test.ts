import { describe, expect, it } from "vitest";
import { removeRanges } from "./cuts";
import type { Clip } from "../types";

const clip = (id: string, start: number, end: number): Clip => ({ id, start, end });
const span = (c: Clip[]) => c.map((x) => [x.start, x.end]);

describe("removeRanges (apply suggested cuts as trims)", () => {
  it("cuts a range out of the middle of a clip, keeping both sides", () => {
    const out = removeRanges([clip("a", 0, 10000)], [{ startMs: 4000, endMs: 6000 }]);
    expect(span(out)).toEqual([[0, 4000], [6000, 10000]]);
    expect(out[0].id).toBe("a");
    expect(new Set(out.map((c) => c.id)).size).toBe(2);
  });
  it("trims clip edges and drops clips that are fully covered", () => {
    const out = removeRanges([clip("a", 0, 3000), clip("b", 3000, 6000), clip("c", 6000, 9000)], [{ startMs: 2000, endMs: 7000 }]);
    expect(span(out)).toEqual([[0, 2000], [7000, 9000]]);
  });
  it("handles several, unsorted and overlapping ranges", () => {
    const out = removeRanges([clip("a", 0, 10000)], [{ startMs: 7000, endMs: 8000 }, { startMs: 1000, endMs: 2000 }, { startMs: 1500, endMs: 2500 }]);
    expect(span(out)).toEqual([[0, 1000], [2500, 7000], [8000, 10000]]);
  });
  it("never removes everything: at least one clip stays", () => {
    const out = removeRanges([clip("a", 0, 1000)], [{ startMs: 0, endMs: 1000 }]);
    expect(out).toHaveLength(1);
    expect(span(out)).toEqual([[0, 1000]]);
  });
  it("ignores empty ranges and ranges outside the clips", () => {
    expect(span(removeRanges([clip("a", 0, 1000)], [{ startMs: 500, endMs: 500 }, { startMs: 5000, endMs: 6000 }]))).toEqual([[0, 1000]]);
  });
});
