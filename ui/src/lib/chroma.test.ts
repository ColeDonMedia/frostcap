import { describe, expect, it } from "vitest";
import { chromaKeyPixels, hexToRgb, rgbToHex } from "./chroma";

const px = (...rgba: number[]) => new Uint8ClampedArray(rgba);

describe("hex helpers", () => {
  it("round-trips #rrggbb", () => {
    expect(hexToRgb("#00ff00")).toEqual([0, 255, 0]);
    expect(rgbToHex(78, 210, 251)).toBe("#4ed2fb");
    expect(hexToRgb("garbage")).toEqual([0, 255, 0]);
  });
});

describe("chromaKeyPixels", () => {
  const key = { color: "#00ff00", similarity: 0.4, smoothness: 0.1, spill: 0 };

  it("makes the key colour transparent and keeps a far colour opaque", () => {
    const d = px(0, 255, 0, 255, 220, 40, 40, 255);
    chromaKeyPixels(d, key);
    expect(d[3]).toBe(0);
    expect(d[7]).toBe(255);
  });

  it("is a soft edge between similarity and similarity + smoothness", () => {
    // A green that is only a little off the key: inside the soft band alpha is between 0 and 255.
    let mid = 0;
    for (let g = 255; g >= 120; g -= 5) {
      const d = px(40, g, 40, 255);
      chromaKeyPixels(d, key);
      if (d[3] > 0 && d[3] < 255) mid++;
    }
    expect(mid).toBeGreaterThan(0);
  });

  it("a larger similarity keys more of the picture", () => {
    const base = () => px(90, 200, 90, 255);
    const a = base();
    chromaKeyPixels(a, { ...key, similarity: 0.1 });
    const b = base();
    chromaKeyPixels(b, { ...key, similarity: 0.7 });
    expect(b[3]).toBeLessThanOrEqual(a[3]);
    expect(b[3]).toBe(0);
  });

  it("spill suppression removes the green cast from kept pixels", () => {
    // Skin-like pixel with a green fringe: g above r and b.
    const d = px(200, 215, 190, 255);
    chromaKeyPixels(d, { ...key, similarity: 0.05, smoothness: 0.01, spill: 1 });
    expect(d[3]).toBe(255);
    expect(d[1]).toBeLessThanOrEqual(Math.max(d[0], d[2]));
    const e = px(200, 215, 190, 255);
    chromaKeyPixels(e, { ...key, similarity: 0.05, smoothness: 0.01, spill: 0 });
    expect(e[1]).toBe(215);
  });

  it("works for non-green keys (blue)", () => {
    const d = px(0, 0, 255, 255, 255, 255, 0, 255);
    chromaKeyPixels(d, { color: "#0000ff", similarity: 0.4, smoothness: 0.1, spill: 0 });
    expect(d[3]).toBe(0);
    expect(d[7]).toBe(255);
  });
});
