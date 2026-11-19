import { describe, expect, it } from "vitest";
import { composite, contrastRatio, parseColor } from "./contrast";

describe("parseColor", () => {
  it("parses hex, rgb() and rgba()", () => {
    expect(parseColor("#4ed2fb")).toEqual({ r: 78, g: 210, b: 251, a: 1 });
    expect(parseColor("#fff")).toEqual({ r: 255, g: 255, b: 255, a: 1 });
    expect(parseColor("rgb(7, 82, 241)")).toEqual({ r: 7, g: 82, b: 241, a: 1 });
    expect(parseColor("rgba(24, 28, 44, 0.62)")).toEqual({ r: 24, g: 28, b: 44, a: 0.62 });
    expect(parseColor("rgb(7 82 241 / 0.5)")).toEqual({ r: 7, g: 82, b: 241, a: 0.5 });
  });
  it("returns null for unknown values", () => {
    expect(parseColor("transparent")?.a).toBe(0);
    expect(parseColor("var(--x)")).toBeNull();
  });
});

describe("composite", () => {
  it("alpha-blends over an opaque backdrop", () => {
    expect(composite({ r: 255, g: 255, b: 255, a: 0.5 }, { r: 0, g: 0, b: 0, a: 1 })).toEqual({ r: 127.5, g: 127.5, b: 127.5, a: 1 });
    expect(composite({ r: 10, g: 20, b: 30, a: 1 }, { r: 0, g: 0, b: 0, a: 1 })).toEqual({ r: 10, g: 20, b: 30, a: 1 });
  });
});

describe("contrastRatio (WCAG 2.x)", () => {
  it("matches known values", () => {
    expect(contrastRatio(parseColor("#000")!, parseColor("#fff")!)).toBeCloseTo(21, 5);
    expect(contrastRatio(parseColor("#fcfdfc")!, parseColor("#1a6ad8")!)).toBeCloseTo(5.03, 2);
    expect(contrastRatio(parseColor("#ffffff")!, parseColor("#c42b1c")!)).toBeCloseTo(5.66, 2);
  });
  it("is symmetric", () => {
    const a = parseColor("#389af5")!;
    const b = parseColor("#0c0f1a")!;
    expect(contrastRatio(a, b)).toBeCloseTo(contrastRatio(b, a), 12);
  });
});
