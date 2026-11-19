/**
 * LIMITS (backend.ts) must equal the backend's validation ranges exactly, so
 * the UI clamps to what Project::validate accepts and a save is never rejected.
 * Two sources are parsed so neither can drift silently:
 *  - docs/CONTRACT.md, the `edits` section (the documented contract);
 *  - every `check_f64(...)` in crates/frostcap-project/src/lib.rs (what is enforced).
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { LIMITS } from "./backend";

const root = join(dirname(fileURLToPath(import.meta.url)), "../..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

type RangeKey = { [K in keyof typeof LIMITS]: (typeof LIMITS)[K] extends readonly [number, number] ? K : never }[keyof typeof LIMITS];

/** Field name (last path segment) -> LIMITS key. An unknown name fails the test: map it. */
const KEY: Record<string, RangeKey> = {
  padding: "padding",
  cornerRadius: "cornerRadius",
  shadow: "shadow",
  cursorScale: "cursorScale",
  smoothing: "unit",
  motionBlur: "unit",
  size: "webcamSize",
  noiseGateThreshold: "noiseGateThreshold",
  highPassHz: "highPassHz",
  targetLufs: "targetLufs",
  micGain: "gain",
  systemGain: "gain",
  fontSize: "fontSize",
  x: "unit",
  y: "unit",
  center: "unit",
  scale: "zoomScale",
};

const num = (s: string) => Number(s.replace(/[\s_]/g, ""));

function check(found: [string, number, number][], source: string) {
  for (const [name, lo, hi] of found) {
    const k = KEY[name];
    expect(k, `${source}: unmapped range "${name}" (${lo}..${hi}); add it to LIMITS`).toBeDefined();
    expect([...LIMITS[k]], `${source}: ${name}`).toEqual([lo, hi]);
  }
}

describe("LIMITS cannot drift from the backend", () => {
  it("matches every range in docs/CONTRACT.md `edits`", () => {
    const md = read("docs/CONTRACT.md");
    const start = md.indexOf("### `edits`");
    expect(start).toBeGreaterThan(-1);
    const next = md.indexOf("\n### ", start + 1);
    const sec = md.slice(start, next < 0 ? undefined : next);
    const found: [string, number, number][] = [];
    for (const m of sec.matchAll(/`?([A-Za-z]+)`?\s*\(?\s*(-?\d+(?:\.\d+)?)\s*\.\.\s*(-?\d+(?:\.\d+)?)/g)) found.push([m[1], Number(m[2]), Number(m[3])]);
    const names = new Set(found.map((f) => f[0]));
    for (const n of ["padding", "shadow", "cursorScale", "smoothing", "motionBlur", "size", "noiseGateThreshold", "highPassHz", "targetLufs", "micGain", "systemGain", "fontSize"]) {
      expect(names.has(n), `CONTRACT.md lost the ${n} range`).toBe(true);
    }
    check(found, "CONTRACT.md");
    const bytes = sec.match(/text\s*<=\s*([\d\s]+)\s*bytes/);
    const cues = sec.match(/<=\s*([\d\s]+?)\s*cues/);
    const splits = sec.match(/`edits\.splits`[^<]*<=\s*([\d\s]+?)\)/);
    expect(bytes && num(bytes[1])).toBe(LIMITS.maxCueBytes);
    expect(cues && num(cues[1])).toBe(LIMITS.maxCues);
    expect(splits && num(splits[1])).toBe(LIMITS.maxSplits);
  });

  it("matches every check_f64 range enforced by Project::validate", () => {
    const rs = read("crates/frostcap-project/src/lib.rs");
    const found: [string, number, number][] = [];
    for (const m of rs.matchAll(/check_f64\(\s*"([\w.]+)",\s*[\w.]+,\s*(-?[\d.]+),\s*(-?[\d.]+),?\s*\)/g)) {
      found.push([m[1].split(".").pop()!, Number(m[2]), Number(m[3])]);
    }
    const names = found.map((f) => f[0]).sort();
    expect(names).toEqual(["cornerRadius", "cursorScale", "fontSize", "highPassHz", "micGain", "motionBlur", "noiseGateThreshold", "padding", "scale", "shadow", "size", "smoothing", "systemGain", "targetLufs", "x", "y"]);
    check(found, "lib.rs");
    const max = (n: string) => num(rs.match(new RegExp(`pub const ${n}: usize = ([\\d_ *]+);`))![1]);
    expect(max("MAX_STRING")).toBe(LIMITS.maxStringBytes);
    expect(max("MAX_STRING")).toBe(LIMITS.maxCueBytes);
    expect(max("MAX_EDITS")).toBe(LIMITS.maxCues);
    expect(max("MAX_EDITS")).toBe(LIMITS.maxSplits);
  });
});
