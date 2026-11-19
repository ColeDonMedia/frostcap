/**
 * Golden command fixtures (fixtures/commands/*.json, generated from Rust by
 * `golden_fixtures` in src-tauri/src/dto.rs): the adapter must send exactly
 * `args` and map `response` / `error`.
 */
import { afterEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { __setInvoke, backend, fromWireProject, toBackendError } from "./backend";
import type { WireProject } from "./contract";
import type { CaptureSource } from "./types";

const here = dirname(fileURLToPath(import.meta.url));
const fx = (name: string) => JSON.parse(readFileSync(join(here, "../../fixtures", `${name}.json`), "utf8"));
const cmd = (name: string) => fx(`commands/${name}`) as { args: Record<string, unknown>; response: unknown; error?: { code: string; message: string } };

let calls: { cmd: string; args: unknown }[] = [];
function serve(name: string, mode: "ok" | "error" = "ok") {
  const f = cmd(name);
  calls = [];
  __setInvoke(async (c, a) => {
    calls.push({ cmd: c, args: a ?? {} });
    if (mode === "error") throw f.error;
    return f.response as never;
  });
  return f;
}
afterEach(() => __setInvoke(null));

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

describe("command args and responses match the Rust golden fixtures", () => {
  it("list_sources", async () => {
    const f = serve("list_sources");
    const s = await backend.listSources();
    expect(calls[0]).toEqual({ cmd: "list_sources", args: f.args });
    expect(s[0]).toMatchObject({ id: "display:\\\\.\\DISPLAY1", kind: "display", name: "DELL U2720Q", width: 2560, height: 1440, thumbnail: "data:image/png;base64,iVBORw0KGgo=", wire: { kind: "monitor", id: "\\\\.\\DISPLAY1" } });
    expect(s[0].primary).toBe(true);
    expect(s[1]).toMatchObject({ kind: "window", app: "Notepad.exe", wire: { kind: "window", hwnd: 65812, pid: 4242 } });
  });

  it("list_cameras", async () => {
    const f = serve("list_cameras");
    expect(await backend.listCameras()).toEqual(f.response);
    expect(calls[0]).toEqual({ cmd: "list_cameras", args: f.args });
  });

  it("start_recording", async () => {
    const f = serve("start_recording");
    const source = { id: "display:x", kind: "display", name: "", width: 1, height: 1, wire: { kind: "monitor", id: "\\\\.\\DISPLAY1" } } as CaptureSource;
    const dir = await backend.startRecording({ source, fps: 60, cursor: false, mic: true, systemAudio: true, keys: "shortcuts", camera: "default" });
    expect(calls[0]).toEqual({ cmd: "start_recording", args: f.args });
    expect(dir).toBe((f.response as { projectDir: string }).projectDir);
  });

  it("start_recording error keeps the Rust code", async () => {
    serve("start_recording", "error");
    const source = { wire: { kind: "primaryMonitor" } } as CaptureSource;
    await expect(backend.startRecording({ source, fps: 60, cursor: true, mic: false, systemAudio: false, keys: "off", camera: null })).rejects.toMatchObject({ code: "disk_full" });
  });

  it("pause / resume return booleans", async () => {
    serve("pause_recording");
    expect(await backend.pauseRecording()).toBe(true);
    expect(calls[0]).toEqual({ cmd: "pause_recording", args: {} });
    serve("resume_recording");
    expect(await backend.resumeRecording()).toBe(true);
  });

  it("stop_recording and recording_state", async () => {
    let f = serve("stop_recording");
    expect(await backend.stopRecording()).toEqual({ projectPath: (f.response as { projectDir: string }).projectDir, durationMs: 3489, warnings: [] });
    f = serve("recording_state");
    expect(await backend.recordingState()).toEqual({ status: "recording", elapsedMs: 1200, projectDir: (f.response as { projectDir: string }).projectDir });
  });

  it("load / save / recover / scrub project", async () => {
    let f = serve("load_project");
    const p = await backend.loadProject(f.args.path as string);
    expect(calls[0]).toEqual({ cmd: "load_project", args: f.args });
    expect(p.wire).toEqual(f.response);

    f = serve("save_project");
    await backend.saveProject(f.args.path as string, fromWireProject(f.args.project as WireProject, "x"));
    const sent = calls[0].args as { path: string; project: WireProject };
    expect(sent.path).toBe(f.args.path);
    expect(stripUi(sent.project, f.args.project as WireProject)).toEqual(f.args.project);

    f = serve("recover_project");
    const rec = await backend.recoverProject(f.args.path as string);
    expect(calls[0]).toEqual({ cmd: "recover_project", args: f.args });
    expect(rec.wire).toEqual(f.response);

    f = serve("scrub_project");
    const scrubbed = await backend.scrubProject(f.args.path as string, { keys: true, titles: true });
    expect(calls[0]).toEqual({ cmd: "scrub_project", args: f.args });
    expect(scrubbed.wire).toEqual(f.response);
  });

  it("load_project error maps to invalid_path", async () => {
    serve("load_project", "error");
    await expect(backend.loadProject("C:\\Windows")).rejects.toMatchObject({ code: "invalid_path" });
  });

  it("generate_auto_zoom", async () => {
    const f = serve("generate_auto_zoom");
    const segs = await backend.generateAutoZoom(fromWireProject(f.args.project as WireProject, "x"), "balanced");
    const sent = calls[0].args as { project: WireProject; preset: string };
    expect(stripUi(sent.project, f.args.project as WireProject)).toEqual(f.args.project);
    expect(sent.preset).toBe("balanced");
    expect(segs[0]).toMatchObject({ start: 0, end: 1800, cx: 0.278, cy: 0.278, scale: 1.8, transitionMs: 650, auto: true });
  });

  it("export_project + cancel_export", async () => {
    let f = serve("export_project");
    const a = f.args as { path: string; outPath: string; jobId: string; preset: { aspect: "16:9"; width: number; height: number; fps: number; format: "mp4" } };
    const r = await backend.exportProject({ path: a.path, outPath: a.outPath, jobId: a.jobId, preset: a.preset });
    expect(calls[0]).toEqual({ cmd: "export_project", args: f.args });
    const res = f.response as { jobId: string; outPath: string; bytes: number; baked: boolean };
    expect(r).toEqual({ jobId: res.jobId, outPath: res.outPath, bytes: res.bytes, baked: res.baked });
    f = serve("cancel_export");
    expect(await backend.cancelExport("job-1")).toBe(true);
    expect(calls[0]).toEqual({ cmd: "cancel_export", args: f.args });
  });

  it("dialogs, reveal, region, default dir, hotkeys", async () => {
    let f = serve("pick_project");
    expect(await backend.pickProject()).toBe(f.response);
    expect(calls[0].args).toEqual(f.args);
    f = serve("pick_folder");
    expect(await backend.pickFolder("Choose a folder")).toBe(f.response);
    expect(calls[0].args).toEqual(f.args);
    f = serve("pick_save_path");
    expect(await backend.pickSavePath("clip", "mp4")).toBe(f.response);
    expect(calls[0].args).toEqual(f.args);
    f = serve("reveal_in_folder");
    await backend.revealInFolder((f.args as { path: string }).path);
    expect(calls[0].args).toEqual(f.args);
    f = serve("pick_region");
    expect(await backend.pickRegion()).toEqual(f.response);
    f = serve("default_output_dir");
    expect(await backend.defaultOutputDir()).toBe(f.response);
    f = serve("hotkeys");
    expect(await backend.hotkeys()).toEqual((f.response as { chords: unknown[] }).chords);
  });
});

describe("set_hotkeys", () => {
  it("sends Rust Bindings {startStop, pauseResume} and maps the returned HotkeyInfo", async () => {
    const info = cmd("hotkeys").response as { chords: unknown[] };
    calls = [];
    __setInvoke(async (c, a) => {
      calls.push({ cmd: c, args: a ?? {} });
      return info as never;
    });
    const res = await backend.setHotkeys({ startStop: "Ctrl+Alt+Shift+R", pauseResume: "Ctrl+Alt+Shift+P" });
    expect(calls[0]).toEqual({ cmd: "set_hotkeys", args: { bindings: { startStop: "Ctrl+Alt+Shift+R", pauseResume: "Ctrl+Alt+Shift+P" } } });
    expect(res).toEqual(info.chords);
  });
});

describe("event payloads", () => {
  it("are typed to the fixtures", () => {
    expect(fx("events/recording_tick")).toEqual({ elapsedMs: 1200, paused: false });
    expect(Object.keys(fx("events/export_progress")).sort()).toEqual(["jobId", "outPath", "path", "progress", "stage"]);
    expect(Object.keys(fx("events/recording_stopped")).sort()).toEqual(["error", "result"]);
    expect(Object.keys(fx("events/hotkey_pressed")).sort()).toEqual(["action", "handled"]);
  });

  it("Rust error codes get actionable text", () => {
    const e = toBackendError(fx("events/recording_error"));
    expect(e.code).toBe("source_gone");
  });
});
