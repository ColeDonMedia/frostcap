/**
 * Typed editor settings in project.json (docs/CONTRACT.md "edits"):
 * edits.style.{frame,cursor,webcam,audio,captions}, edits.captions, edits.splits.
 * edits.ui only keeps zoom defaults; a legacy v1 blob is read when typed groups are absent.
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { WireProject } from "./contract";
import { LIMITS, clipsToTrims, formatBackground, fromWireProject, toWireProject, toWireStyle, toWireZoom } from "./backend";
import { defaultStyle } from "./lib/presets";

const here = dirname(fileURLToPath(import.meta.url));
const fx = (p: string) => JSON.parse(readFileSync(join(here, "../../fixtures", p), "utf8")) as WireProject;
const sample = fx("project.sample.json");
const legacy = fx("legacy/project.ui-v1.json");

describe("typed edits: project.sample.json", () => {
  const p = fromWireProject(structuredClone(sample), "C:\\p.frostcap");

  it("reads every typed group", () => {
    expect(p.style.frame).toBe("windows");
    expect(p.style.cursor).toMatchObject({ visible: true, smoothing: 0.5, motionBlur: 0.25, smooth: true });
    expect(p.style.webcam).toMatchObject({ enabled: true, shape: "rounded", size: 0.25, position: "top-right" });
    expect(p.style.audio).toEqual({ noiseGate: true, noiseGateThreshold: -45, highPass: true, highPassHz: 100, normalize: true, targetLufs: -16, micGain: 1, systemGain: 0.5 });
    expect(p.style.captions).toEqual({ enabled: true, fontSize: 44, position: "bottom", background: true });
    expect(p.captions.map(({ start, end, text }) => ({ start, end, text }))).toEqual([
      { start: 200, end: 1400, text: "Open the settings" },
      { start: 1600, end: 2800, text: "Save with Ctrl + S" },
    ]);
    expect(p.clips.map((c) => [c.start, c.end])).toEqual([
      [0, 1500],
      [1500, 3000],
    ]);
    expect(p.style.zoom).toEqual({ preset: "balanced", scale: 1.8, easing: "easeInOutCubic", transitionMs: 650 });
  });

  it("round-trips exactly, with edits.ui holding only zoom defaults", () => {
    expect(toWireProject(p)).toEqual(sample);
  });

  it("keeps the smoothing amount when the switch is off (effective smoothing 0)", () => {
    const w = structuredClone(sample);
    w.edits!.style.cursorSmoothing = false;
    const q = fromWireProject(w, "x");
    expect(q.style.cursor.smooth).toBe(false);
    expect(q.style.cursor.smoothing).toBe(0.5);
    expect(toWireProject(q).edits!.style).toMatchObject({ cursorSmoothing: false, cursor: { smoothing: 0.5 } });
  });

  it("a baked cursor is not altered in the saved style (the preview just never draws it)", () => {
    const w = { ...structuredClone(sample), cursorBaked: true };
    expect(toWireProject(fromWireProject(w, "x")).edits!.style.cursor!.visible).toBe(true);
  });
});

describe("legacy v1 blob (fixtures/legacy/project.ui-v1.json)", () => {
  const p = fromWireProject(structuredClone(legacy), "x");

  it("fills absent typed groups from the blob", () => {
    expect(p.style.frame).toBe("windows");
    expect(p.style.cursor).toMatchObject({ visible: false, smoothing: 0.25, motionBlur: 0.7 });
    expect(p.style.webcam).toMatchObject({ enabled: true, shape: "rounded", size: 0.3, position: "top-left" });
    expect(p.style.audio.micGain).toBe(0);
    expect(p.style.audio.systemGain).toBe(1.5);
    expect(p.style.captions).toEqual({ enabled: true, fontSize: 52, position: "top", background: false });
    expect(p.captions.map(({ start, end, text }) => [start, end, text])).toEqual([
      [100, 900, "hello"],
      [1200, 2400, "world"],
    ]);
    expect(p.clips.map((c) => [c.start, c.end])).toEqual([
      [0, 1000],
      [1000, 2000],
      [2000, 3000],
    ]);
    expect(p.style.zoom).toMatchObject({ preset: "snappy", scale: 2.5, easing: "linear", transitionMs: 300 });
  });

  it("writes typed fields and shrinks edits.ui to the zoom defaults", () => {
    const w = toWireProject(p);
    expect(w.edits!.style).toMatchObject({ frame: "windows", webcam: { position: "top-left" }, captions: { position: "top" } });
    expect(w.edits!.captions).toEqual([
      { startMs: 100, endMs: 900, text: "hello" },
      { startMs: 1200, endMs: 2400, text: "world" },
    ]);
    expect(w.edits!.splits).toEqual([1000, 2000]);
    expect(w.edits!.ui).toEqual({ v: 1, zoom: { preset: "snappy", scale: 2.5, easing: "linear", transitionMs: 300 } });
  });

  it("typed groups win over the blob when both exist", () => {
    const w = structuredClone(legacy);
    w.edits!.style.frame = "macos";
    expect(fromWireProject(w, "x").style.frame).toBe("macos");
  });
});

describe("clamping to the backend's ranges (a save is never rejected)", () => {
  it("clamps every typed number and sanitizes captions", () => {
    const p = fromWireProject(structuredClone(sample), "x");
    const wild = {
      ...p,
      style: {
        ...p.style,
        padding: 80,
        shadow: 300,
        cursor: { ...p.style.cursor, size: 50, smoothing: 2, motionBlur: -1 },
        webcam: { ...p.style.webcam, size: 0.9 },
        audio: { ...p.style.audio, noiseGateThreshold: -500, highPassHz: 5, targetLufs: 0, micGain: 9, systemGain: -1 },
        captions: { ...p.style.captions, fontSize: 500 },
      },
      captions: [
        { id: "a", start: 500, end: 100, text: "backwards" },
        { id: "b", start: 0, end: 10, text: "x".repeat(5000) },
      ],
    };
    const s = toWireProject(wild).edits!;
    expect(s.style).toMatchObject({ padding: LIMITS.padding[1], shadow: 1, cursorScale: LIMITS.cursorScale[1], cursor: { smoothing: 1, motionBlur: 0 } });
    expect(s.style.webcam!.size).toBe(0.6);
    expect(s.style.audio).toMatchObject({ noiseGateThreshold: -100, highPassHz: 10, targetLufs: -5, micGain: 4, systemGain: 0 });
    expect(s.style.captions!.fontSize).toBe(200);
    expect(s.captions![0]).toEqual({ startMs: 500, endMs: 500, text: "backwards" });
    expect(new TextEncoder().encode(s.captions![1].text).length).toBeLessThanOrEqual(4096);
  });

  it("clamps zoom segments to Project::validate (center 0..1, scale range, end >= start, whole ms)", () => {
    const z = toWireZoom({ id: "z", start: 900.6, end: 100.2, cx: -1, cy: 2, scale: 50, easing: "linear", transitionMs: 300.4, auto: false });
    expect(z).toEqual({ startMs: 901, endMs: 901, center: { x: 0, y: 1 }, scale: LIMITS.zoomScale[1], easing: "linear", transitionMs: 300, origin: "manual" });
    expect(toWireZoom({ id: "z", start: -5, end: 10, cx: 0.5, cy: 0.5, scale: 0.2, easing: "linear", transitionMs: -9, auto: true })).toMatchObject({ startMs: 0, endMs: 10, scale: LIMITS.zoomScale[0], transitionMs: 0 });
  });

  it("non-finite zoom numbers fall back to defaults (NaN would serialize as null and fail the save)", () => {
    const z = toWireZoom({ id: "z", start: NaN, end: Infinity, cx: NaN, cy: -Infinity, scale: NaN, easing: "linear", transitionMs: NaN, auto: true });
    const d = defaultStyle().zoom;
    expect(z).toMatchObject({ startMs: 0, endMs: 0, center: { x: 0.5, y: 0.5 }, scale: d.scale, transitionMs: d.transitionMs });
    expect(JSON.stringify(z)).not.toContain("null");
  });

  it("cornerRadius is clamped to 0..4096 and every non-finite style number becomes its default", () => {
    const p = fromWireProject(structuredClone(sample), "x");
    const big = toWireStyle({ ...p.style, radius: 99999 });
    expect(big.cornerRadius).toBe(4096);
    expect(toWireStyle({ ...p.style, radius: -3 }).cornerRadius).toBe(0);
    const n = NaN;
    const nan = toWireStyle({
      ...p.style,
      padding: n,
      radius: n,
      shadow: n,
      cursor: { ...p.style.cursor, size: n, smoothing: n, motionBlur: n },
      webcam: { ...p.style.webcam, size: n },
      audio: { ...p.style.audio, noiseGateThreshold: n, highPassHz: n, targetLufs: n, micGain: n, systemGain: n },
      captions: { ...p.style.captions, fontSize: n },
    });
    expect(JSON.stringify(nan)).not.toContain("null");
    const d = defaultStyle();
    expect(nan).toMatchObject({ padding: d.padding / 100, cornerRadius: d.radius, shadow: d.shadow / 100, cursorScale: d.cursor.size });
    expect(nan.cursor).toMatchObject({ smoothing: d.cursor.smoothing, motionBlur: d.cursor.motionBlur });
    expect(nan.webcam!.size).toBe(d.webcam.size);
    expect(nan.audio).toMatchObject({ noiseGateThreshold: d.audio.noiseGateThreshold, highPassHz: d.audio.highPassHz, targetLufs: d.audio.targetLufs, micGain: d.audio.micGain, systemGain: d.audio.systemGain });
    expect(nan.captions!.fontSize).toBe(d.captions.fontSize);
  });

  it("caps the background string at 4096 bytes (falls back to the default background)", () => {
    const p = fromWireProject(structuredClone(sample), "x");
    const stops = Array.from({ length: 1000 }, () => "#a1c4fd");
    const bg = toWireStyle({ ...p.style, background: { ...p.style.background, angle: 90, stops } }).background;
    expect(new TextEncoder().encode(bg).length).toBeLessThanOrEqual(4096);
    expect(bg).toBe(formatBackground(defaultStyle().background));
  });
});

describe("trims and splits are whole milliseconds (serde u64 rejects fractions)", () => {
  it("rounds trims and splits and drops trims that round to nothing", () => {
    const clips = [
      { id: "a", start: 0, end: 1000.4 },
      { id: "b", start: 1000.4, end: 2000.6 },
      { id: "c", start: 2500.5, end: 2999.6 },
    ];
    const { trims, splits } = clipsToTrims(clips, 3000);
    expect(splits).toEqual([1000]);
    expect(trims).toEqual([{ startMs: 2001, endMs: 2501 }]);
    for (const t of trims) expect(Number.isInteger(t.startMs) && Number.isInteger(t.endMs) && t.endMs >= t.startMs).toBe(true);
  });
});

describe("editor v2 fields (fixtures/project.editor-v2.json)", () => {
  const v2 = fx("project.editor-v2.json");
  const p = fromWireProject(structuredClone(v2), "C:\p.frostcap");

  it("round-trips exactly", () => {
    const back = toWireProject(p);
    if (v2.edits!.ui === undefined) delete back.edits!.ui; // the editor always adds its zoom defaults
    expect(back).toEqual(v2);
  });

  it("reads annotations, speed, device frame, background image and webcam effects", () => {
    expect(p.annotations.map((a) => a.kind)).toEqual(["arrow", "text", "rect", "ellipse", "highlight", "spotlight", "blur", "pixelate"]);
    expect(p.annotations[0]).toMatchObject({ start: 0, end: 1500, x: 0.1, y: 0.2, x2: 0.7, y2: 0.6, strokeWidth: 4, fadeMs: 150 });
    expect(p.speed.map((s) => [s.start, s.end, s.speed])).toEqual([
      [500, 1000, 2],
      [1000, 2000, 0.5],
    ]);
    expect(p.style.frame).toBe("laptop");
    expect(p.style.backgroundImage).toEqual({ file: "background-0123456789abcdef.png", fit: "contain", blur: 12 });
    expect(p.style.webcam).toMatchObject({ mirror: true, borderWidth: 4, borderColor: "#4ed2fb", chromaKey: { enabled: true }, background: { kind: "image", image: "background-0123456789abcdef.png" } });
  });

  it("projects without the new fields get the Rust defaults and leave backgroundImage out", () => {
    const old = structuredClone(sample);
    delete old.edits!.annotations;
    delete old.edits!.speed;
    const q = fromWireProject(old, "x");
    expect(q.annotations).toEqual([]);
    expect(q.speed).toEqual([]);
    expect(toWireProject(q).edits!.style.backgroundImage).toBeUndefined();
    expect(toWireProject(q).edits!.style.webcam!.background!.image).toBeUndefined();
  });

  it("clamps everything to the schema before saving", () => {
    const q = structuredClone(p);
    q.annotations[0] = { ...q.annotations[0], x: 9, strokeWidth: 999, fontSize: 1, opacity: Number.NaN, color: "red", fadeMs: 99999, text: "x".repeat(900), id: "i".repeat(200) };
    q.speed = [
      { id: "a", start: 0, end: 1000, speed: 99 },
      { id: "b", start: 500, end: 1500, speed: 0.01 },
    ];
    q.style.webcam = { ...q.style.webcam, margin: 5, borderWidth: -3, brightness: 7, contrast: 9, chromaKey: { ...q.style.webcam.chromaKey, spill: 3, color: "nope" } };
    q.style.backgroundImage = { file: "b.png", fit: "cover", blur: 1000 };
    const w = toWireProject(q).edits!;
    expect(w.annotations![0]).toMatchObject({ x: 1, strokeWidth: 40, fontSize: 8, opacity: 1, color: "#4ed2fb", fadeMs: 2000 });
    expect(w.annotations![0].text.length).toBe(500);
    expect(new TextEncoder().encode(w.annotations![0].id).length).toBe(64);
    expect(w.speed).toEqual([
      { startMs: 0, endMs: 1000, speed: 4 },
      { startMs: 1000, endMs: 1500, speed: 0.25 },
    ]);
    expect(w.style.webcam).toMatchObject({ margin: 0.2, borderWidth: 0, brightness: 1, contrast: 2, chromaKey: { spill: 1, color: "#00ff00" } });
    expect(w.style.backgroundImage!.blur).toBe(100);
  });
});
