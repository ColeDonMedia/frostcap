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

// ---------------------------------------------------------------- editor v2

type RK = RangeKey;
/** Full dotted name of a check_range call -> LIMITS key. Unknown names fail the test: map them. */
const V2_KEY: Record<string, RK> = {
  "style.webcam.margin": "webcamMargin",
  "style.webcam.borderWidth": "webcamBorderWidth",
  "style.webcam.brightness": "brightness",
  "style.webcam.contrast": "gain2",
  "style.webcam.saturation": "gain2",
  "style.webcam.shadow": "unit",
  "style.webcam.chromaKey.similarity": "unit",
  "style.webcam.chromaKey.smoothness": "unit",
  "style.webcam.chromaKey.spill": "unit",
  "style.webcam.background.blur": "unit",
  "style.backgroundImage.blur": "backgroundBlur",
  "speed.speed": "speed",
  "annotation.strokeWidth": "annStroke",
  "annotation.fontSize": "fontSize",
  "annotation.cornerRadius": "annCorner",
  // the loop over ("x","y","w","h","x2","y2","opacity","strength","dim")
  "annotation.{f}": "unit",
};

/** Column names of the contract tables -> LIMITS key (annotation and webcam tables). */
const TABLE_KEY: Record<string, RK> = {
  margin: "webcamMargin",
  borderWidth: "webcamBorderWidth",
  brightness: "brightness",
  contrast: "gain2",
  saturation: "gain2",
  shadow: "unit",
  strokeWidth: "annStroke",
  fontSize: "fontSize",
  cornerRadius: "annCorner",
  opacity: "unit",
  strength: "unit",
  dim: "unit",
};

describe("editor v2 limits cannot drift", () => {
  it("matches every check_range range enforced by Project::validate", () => {
    const rs = read("crates/frostcap-project/src/lib.rs");
    const found = [...rs.matchAll(/check_range\(\s*(?:&format!\()?"([\w.{}]+)"\)?,\s*[\w.]+,\s*(-?[\d.]+),\s*(-?[\d.]+),?\s*\)/g)].map((m) => [m[1], Number(m[2]), Number(m[3])] as const);
    expect(found.length, "no check_range calls parsed").toBeGreaterThan(10);
    for (const [name, lo, hi] of found) {
      const k = V2_KEY[name];
      expect(k, `lib.rs: unmapped check_range "${name}" (${lo}..${hi}); add it to LIMITS and V2_KEY`).toBeDefined();
      expect([...LIMITS[k]], name).toEqual([lo, hi]);
    }
    const max = (n: string) => num(rs.match(new RegExp(String.raw`pub const ${n}: usize = ([\d_ *]+);`))![1]);
    expect(max("MAX_ANNOTATIONS")).toBe(LIMITS.maxAnnotations);
    expect(max("MAX_ANNOTATION_TEXT")).toBe(LIMITS.maxAnnotationText);
    expect(max("MAX_EDITS")).toBe(LIMITS.maxSpeed);
    expect(rs.match(/fade_ms > (\d+)/)?.[1]).toBe(String(LIMITS.annFadeMs[1]));
  });

  it("matches the ranges and counts in docs/CONTRACT.md `Editor v2 fields`", () => {
    const md = read("docs/CONTRACT.md");
    const start = md.indexOf("### Editor v2 fields");
    expect(start).toBeGreaterThan(-1);
    const next = md.indexOf("\n### ", start + 1);
    const sec = md.slice(start, next < 0 ? undefined : next);
    // Table rows: | `name` | type / range | ...  (the first range in the second column)
    const rows = [...sec.matchAll(/^\|\s*`(\w+)`[^|]*\|([^|]*)\|/gm)];
    const seen = new Set<string>();
    for (const [, name, col] of rows) {
      const m = col.match(/(-?\d+(?:\.\d+)?)\.\.(-?\d+(?:\.\d+)?)/);
      if (!m) continue;
      const k = TABLE_KEY[name] ?? (["x", "y", "w", "h", "x2", "y2"].includes(name) ? "unit" : undefined);
      if (!k) continue; // chromaKey / background rows are objects; their members are covered by lib.rs
      seen.add(name);
      expect([...LIMITS[k]], `CONTRACT.md ${name}`).toEqual([Number(m[1]), Number(m[2])]);
    }
    for (const n of ["margin", "borderWidth", "brightness", "contrast", "saturation", "shadow", "strokeWidth", "fontSize", "cornerRadius", "opacity", "strength", "dim", "x"]) {
      expect(seen.has(n), `CONTRACT.md lost the ${n} range`).toBe(true);
    }
    expect(sec.match(/`speed`\s*\n?0\.25\.\.4|0\.25\.\.4/)).not.toBeNull();
    expect(sec).toContain("0..2000");
    expect(sec.match(/`blur`\s*\n?0\.\.100|0\.\.100/)).not.toBeNull();
    expect(sec.match(/at most (\d+) are drawn/i)?.[1]).toBe(String(LIMITS.maxAnnotationsDrawn));
    expect(sec.match(/`edits\.annotations`: at most (\d+)/)?.[1]).toBe(String(LIMITS.maxAnnotations));
    expect(num(sec.match(/at most ([\d ]+), segments/)![1])).toBe(LIMITS.maxSpeed);
    expect(sec.match(/<= (\d+) chars/)?.[1]).toBe(String(LIMITS.maxAnnotationText));
  });
});
