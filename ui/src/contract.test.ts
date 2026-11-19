import { describe, expect, it } from "vitest";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { WireProject, WireRecentProject, WireSourceList } from "./contract";
import {
  clipsToTrims,
  formatBackground,
  fromWireProject,
  fromWireRecent,
  fromWireSources,
  joinPath,
  parseBackground,
  toBackendError,
  toWireProject,
  trimsToClips,
} from "./backend";
import { mockInvoke } from "./mockBackend";
import { useEditor } from "./store/editor";
import { effectiveCursor } from "./lib/render";

const here = dirname(fileURLToPath(import.meta.url));
const handFixture = JSON.parse(readFileSync(join(here, "__fixtures__/project.v1.json"), "utf8")) as WireProject;
/** Golden fixtures generated from Rust serialization by the backend agent (may not exist yet). */
const goldenDir = join(here, "../../fixtures");
const golden = join(goldenDir, "project.sample.json");

/** Back from the adapter, minus the editor fields the UI always writes but `orig` did not have. */
const stripUi = (back: WireProject, orig?: WireProject) => {
  const c = structuredClone(back);
  const o = orig?.edits;
  if (c.edits) {
    if (!o || o.ui === undefined) delete c.edits.ui;
    if (!o || o.captions === undefined) delete c.edits.captions;
    if (!o || o.splits === undefined) delete c.edits.splits;
    for (const k of ["frame", "cursor", "webcam", "audio", "captions"] as const) if (!o || o.style?.[k] === undefined) delete c.edits.style[k];
  }
  return c;
};

function roundTrip(w: WireProject) {
  const ui = fromWireProject(w, "C:\\p.frostcap");
  return { ui, back: toWireProject(ui) };
}

describe("project wire <-> UI", () => {
  it("normalizes pixels with the canvas and keeps tMs", () => {
    const p = fromWireProject(handFixture, "C:\\p.frostcap");
    expect(p.cursor[0]).toEqual({ t: 0, x: 0.5, y: 0.5 });
    expect(p.cursor[2].x).toBeCloseTo(-12 / 1920, 12); // outside the canvas stays outside
    expect(p.clicks[0]).toMatchObject({ t: 1200, x: 0.25, y: 0.25, button: "left", state: "down" });
    expect(p.keys[0]).toMatchObject({ t: 2500, vk: 83, label: "S" });
    expect(p.focus[0].rect.height).toBeCloseTo(1040 / 1080, 12);
    expect(p.media.mic?.offsetMs).toBe(-35);
    expect(p.media.dir).toBe("C:\\p.frostcap");
  });

  it("maps zoom segments, easing names and origin", () => {
    const p = fromWireProject(handFixture, "x");
    expect(p.zoomSegments.map(({ id: _id, ...z }) => z)).toEqual([
      { start: 750, end: 3000, cx: 0.3, cy: 0.35, scale: 1.8, easing: "easeInOutCubic", transitionMs: 650, auto: true },
      { start: 5000, end: 7000, cx: 0.7, cy: 0.6, scale: 2.2, easing: "easeOutQuint", transitionMs: 350, auto: false },
    ]);
  });

  it("maps style scales", () => {
    const s = fromWireProject(handFixture, "x").style;
    expect(s.padding).toBe(8);
    expect(s.shadow).toBe(50);
    expect(s.radius).toBe(12);
    expect(s.cursor.size).toBe(1.25);
    expect(s.cursor.smoothing).toBeGreaterThan(0);
    expect(s.background).toMatchObject({ angle: 135, stops: ["#a1c4fd", "#c2e9fb"] });
  });

  it("round-trips wire -> UI -> wire exactly (apart from the added edits.ui)", () => {
    const { back } = roundTrip(handFixture);
    expect(stripUi(back, handFixture)).toEqual(handFixture);
  });

  it("reuses the original tracks object (no copy of large tracks on save)", () => {
    const { back } = roundTrip(handFixture);
    expect(back.tracks).toBe(handFixture.tracks);
  });

  it("round-trips UI edits through edits.ui (splits, captions, UI-only style)", () => {
    const p = fromWireProject(handFixture, "x");
    const c0 = p.clips[0];
    const mid = Math.round((c0.start + c0.end) / 2);
    const edited = {
      ...p,
      clips: [{ ...c0, end: mid }, { ...c0, id: "b", start: mid }, ...p.clips.slice(1)],
      captions: [{ id: "c1", start: 100, end: 900, text: "Hello" }],
      style: { ...p.style, frame: "macos" as const, padding: 12.5, cursor: { ...p.style.cursor, smoothing: 0.33, motionBlur: 0.7 } },
    };
    const w = toWireProject(edited);
    expect(w.edits!.style.padding).toBe(0.125);
    const again = fromWireProject(JSON.parse(JSON.stringify(w)), "x");
    expect(again.clips.map((c) => [c.start, c.end])).toEqual(edited.clips.map((c) => [c.start, c.end]));
    expect(again.captions.map(({ id: _id, ...c }) => c)).toEqual([{ start: 100, end: 900, text: "Hello" }]);
    expect(again.style).toEqual(edited.style);
  });

  it("does not draw a second cursor when the system cursor is baked into the video", () => {
    const w = { ...structuredClone(handFixture), cursorBaked: true };
    const p = fromWireProject(w, "x");
    expect(p.cursorBaked).toBe(true);
    // The saved style is left alone; the renderer decides (effectiveCursor).
    expect(effectiveCursor(p).draw).toBe(false);
    expect(effectiveCursor(fromWireProject({ ...w, cursorBaked: false }, "x")).draw).toBe(true);
  });

  it("defaults missing edits like serde(default)", () => {
    const w = structuredClone(handFixture);
    delete w.edits;
    const p = fromWireProject(w, "x");
    expect(p.clips).toHaveLength(1);
    expect(p.clips[0]).toMatchObject({ start: 0, end: 10000 });
    expect(p.zoomSegments).toEqual([]);
    expect(p.style.padding).toBe(8);
  });
});

describe("media paths are confined to the project folder", () => {
  it("joins plain file names", () => {
    expect(joinPath("C:\\p.frostcap", "screen.mp4")).toBe("C:\\p.frostcap\\screen.mp4");
  });
  it("rejects absolute paths, traversal and separators in media names", () => {
    for (const bad of ["C:\\Windows\\win.ini", "\\\\server\\share\\x.wav", "/etc/passwd", "..\\secret.wav", "sub\\x.mp4", "sub/x.mp4", "..", "file:///c:/x", ""]) {
      expect(() => joinPath("C:\\p.frostcap", bad), bad).toThrow();
    }
  });
  it("a project with a hostile media name gets no URL for it", () => {
    const w = structuredClone(handFixture);
    w.media.micAudio = { ...w.media.micAudio!, file: "..\\..\\Users\\me\\secret.wav" };
    const p = fromWireProject(w, "C:\\p.frostcap", (abs) => `asset://${abs}`);
    expect(p.media.mic?.url).toBeNull();
    expect(p.media.screen.url).toBe("asset://C:\\p.frostcap\\screen.mp4");
  });
});

describe("trims <-> clips", () => {
  it("are complements over [0, duration]", () => {
    const clips = trimsToClips(
      [
        { startMs: 0, endMs: 400 },
        { startMs: 8000, endMs: 8600 },
      ],
      10000,
    );
    expect(clips.map((c) => [c.start, c.end])).toEqual([
      [400, 8000],
      [8600, 10000],
    ]);
    expect(clipsToTrims(clips, 10000)).toEqual({
      trims: [
        { startMs: 0, endMs: 400 },
        { startMs: 8000, endMs: 8600 },
      ],
      splits: [],
    });
  });

  it("merges overlapping/unsorted trims, clamps to duration, drops empty ones", () => {
    const clips = trimsToClips(
      [
        { startMs: 5000, endMs: 6000 },
        { startMs: 4500, endMs: 5500 },
        { startMs: 9000, endMs: 20000 },
        { startMs: 100, endMs: 100 },
      ],
      10000,
    );
    expect(clips.map((c) => [c.start, c.end])).toEqual([
      [0, 4500],
      [6000, 9000],
    ]);
  });

  it("keeps split points as adjacent clips", () => {
    const clips = trimsToClips([], 10000, [2500, 7000]);
    expect(clips.map((c) => [c.start, c.end])).toEqual([
      [0, 2500],
      [2500, 7000],
      [7000, 10000],
    ]);
    expect(clipsToTrims(clips, 10000)).toEqual({ trims: [], splits: [2500, 7000] });
  });
});

describe("background strings", () => {
  it("parses and formats the Rust default", () => {
    const g = parseBackground("linear-gradient(135deg,#a1c4fd,#c2e9fb)");
    expect(formatBackground(g)).toBe("linear-gradient(135deg,#a1c4fd,#c2e9fb)");
  });
  it("handles spaces, rgba() stops and plain colours", () => {
    const g = parseBackground("linear-gradient( 90deg , rgba(0,0,0,0.5) 10%, #fff)");
    expect(g.angle).toBe(90);
    expect(g.stops).toEqual(["rgba(0,0,0,0.5) 10%", "#fff"]);
    expect(formatBackground(parseBackground("#0b1020"))).toBe("#0b1020");
  });
});

describe("other DTOs", () => {
  it("maps list_sources", () => {
    const list: WireSourceList = {
      monitors: [{ id: "\\\\.\\DISPLAY1", name: "Dell", bounds: { x: 0, y: 0, width: 2560, height: 1440 }, refreshRate: 60, scaleFactor: 1.5, primary: true }],
      windows: [{ hwnd: 42, pid: 7, title: "Code", processName: "Code.exe", bounds: { x: 1, y: 2, width: 800, height: 600 } }],
      sources: [],
    };
    const s = fromWireSources(list);
    expect(s[0]).toMatchObject({ kind: "display", width: 2560, primary: true, wire: { kind: "monitor", id: "\\\\.\\DISPLAY1" } });
    expect(s[1]).toMatchObject({ kind: "window", app: "Code.exe", wire: { kind: "window", hwnd: 42 } });
  });

  it("maps recent projects", () => {
    const r: WireRecentProject = { path: "C:\\a.frostcap", name: "A", createdAtMs: 0, durationMs: 5, canvas: { width: 10, height: 20 } };
    expect(fromWireRecent(r)).toMatchObject({ createdAt: "1970-01-01T00:00:00.000Z", width: 10, height: 20, interrupted: false });
  });

  it("maps string and structured errors to codes", () => {
    expect(toBackendError("a recording is already in progress").code).toBe("alreadyRecording");
    expect(toBackendError("not recording").code).toBe("notRecording");
    expect(toBackendError("capture source not found: x").code).toBe("sourceNotFound");
    expect(toBackendError("Command pick_project not found").code).toBe("notImplemented");
    expect(toBackendError({ code: "diskLow", message: "2 GB left" })).toMatchObject({ code: "diskLow", message: "2 GB left" });
  });
});

describe("mock backend speaks the Rust wire format", () => {
  it("load_project returns a project.json-shaped object that round-trips", async () => {
    const w = await mockInvoke<WireProject>("load_project", { path: "C:\\x\\Demo.frostcap" });
    expect(w.tracks.cursor[0]).toHaveProperty("tMs");
    expect(Number.isInteger(w.tracks.cursor[1].x)).toBe(true); // pixels
    expect(w.edits?.zoomSegments[0]).toHaveProperty("center");
    const { back } = roundTrip(w);
    expect(stripUi(back, w)).toEqual(w);
  });

  it("generate_auto_zoom returns wire segments with clamped centers", async () => {
    const w = await mockInvoke<WireProject>("load_project", { path: "C:\\x\\Demo.frostcap" });
    const segs = await mockInvoke<{ center: { x: number; y: number }; scale: number }[]>("generate_auto_zoom", { project: w, preset: "snappy" });
    for (const s of segs) {
      const half = 0.5 / s.scale;
      expect(s.center.x).toBeGreaterThanOrEqual(half - 1e-9);
      expect(s.center.x).toBeLessThanOrEqual(1 - half + 1e-9);
    }
  });

  it("unknown commands fail like Tauri", async () => {
    await expect(mockInvoke("nope")).rejects.toMatch(/not found/);
  });
});

describe("undo history shares structure", () => {
  it("does not copy 400k cursor samples per edit", () => {
    const w = structuredClone(handFixture);
    w.durationMs = 3_600_000;
    w.tracks.cursor = Array.from({ length: 400_000 }, (_, i) => ({ tMs: i * 9, x: i % 1920, y: i % 1080 }));
    const p = fromWireProject(w, "x");
    useEditor.setState({ project: p, past: [], future: [], path: "x" });
    const t0 = performance.now();
    for (let i = 0; i < 200; i++) useEditor.getState().setStyle("cursor", { size: 1 + i / 1000 }, `k${i}`);
    const ms = performance.now() - t0;
    const st = useEditor.getState();
    expect(st.past.length).toBe(200);
    expect(st.past.every((h) => h.cursor === p.cursor && h.wire === p.wire)).toBe(true);
    expect(st.project!.cursor).toBe(p.cursor);
    expect(ms).toBeLessThan(500);
    useEditor.getState().undo();
    expect(useEditor.getState().project!.cursor).toBe(p.cursor);
    expect(toWireProject(useEditor.getState().project!).tracks).toBe(w.tracks);
  });
});

describe("golden fixtures from Rust (fixtures/)", () => {
  const hasGolden = existsSync(golden);
  it.skipIf(!hasGolden)("project.sample.json round-trips through the adapter", () => {
    const w = JSON.parse(readFileSync(golden, "utf8")) as WireProject;
    const { back } = roundTrip(w);
    expect(stripUi(back, w)).toEqual(w);
  });

  const cmdDir = join(goldenDir, "commands");
  const files = existsSync(cmdDir) ? readdirSync(cmdDir).filter((f) => f.endsWith(".json")) : [];
  it.skipIf(!files.length)("command fixtures map without loss of required fields", () => {
    for (const f of files) {
      const raw = JSON.parse(readFileSync(join(cmdDir, f), "utf8"));
      const res = raw.response ?? raw.result ?? raw;
      if (f.startsWith("list_sources")) expect(fromWireSources(res).length).toBe(res.monitors.length + res.windows.length);
      if (f.startsWith("list_recent_projects")) for (const r of res) expect(fromWireRecent(r).path).toBe(r.path);
      if (f.startsWith("load_project")) expect(stripUi(roundTrip(res).back, res)).toEqual(res);
    }
  });
});
