import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { __setInvoke } from "../backend";
import { __mockEmit } from "../mockBackend";
import { useApp } from "./app";
import { __setWindowVisibleProbe, useRecording } from "./recording";
import { useEditor } from "./editor";
import type { CaptureSource } from "../types";

const src = { id: "display:x", kind: "display", name: "D", width: 1, height: 1, wire: { kind: "primaryMonitor" } } as CaptureSource;
const tick = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  __setInvoke(async (cmd) => {
    if (cmd === "start_recording") return { projectDir: "C:\\v\\New.frostcap", canvas: { width: 1, height: 1 }, startMs: 1 } as never;
    return null as never;
  });
  useApp.setState({ toasts: [], projectPath: null, view: "recorder", settings: { ...useApp.getState().settings, countdown: false } });
  useEditor.setState({ project: null, dirty: false });
  useRecording.setState({ phase: "idle", elapsedMs: 0 });
});
afterEach(() => __setInvoke(null));

describe("hotkey://pressed", () => {
  it("an unhandled Start/Stop while idle starts the selected source via the countdown, only if the window is visible", { timeout: 10_000 }, async () => {
    const started: unknown[] = [];
    __setInvoke(async (cmd, args) => {
      if (cmd === "start_recording") {
        started.push(args);
        return { projectDir: "C:\\v\\H.frostcap", canvas: { width: 1, height: 1 }, startMs: 1 } as never;
      }
      return null as never;
    });
    let visible = false;
    __setWindowVisibleProbe(async () => visible);
    const un = await useRecording.getState().listenHotkeys();
    useRecording.getState().setSource(src);
    // Never starts silently: a hidden/minimized window ignores the hotkey.
    __mockEmit("hotkey://pressed", { action: "startStop", handled: false });
    await tick();
    await tick();
    expect(useRecording.getState().phase).toBe("idle");
    // Visible window: always through the visible countdown, even with the countdown setting off.
    visible = true;
    __mockEmit("hotkey://pressed", { action: "startStop", handled: false });
    await tick();
    await tick();
    expect(useRecording.getState().phase).toBe("countdown");
    expect(started).toHaveLength(0);
    await new Promise((r) => setTimeout(r, 3100));
    expect(started).toHaveLength(1);
    expect(useRecording.getState().phase).toBe("recording");
    // handled by the backend (it stopped/paused itself): the UI does not start again
    __mockEmit("hotkey://pressed", { action: "startStop", handled: true });
    await tick();
    expect(started).toHaveLength(1);
    un();
    __mockEmit("recording://stopped", { result: null, error: null });
    await tick();
  });
});

describe("window titles are opt-in", () => {
  async function record(captureTitles: boolean) {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    __setInvoke(async (cmd, args = {}) => {
      calls.push({ cmd, args });
      if (cmd === "start_recording") return { projectDir: "C:\\v\\T.frostcap", canvas: { width: 1, height: 1 }, startMs: 1 } as never;
      if (cmd === "stop_recording") return { projectDir: "C:\\v\\T.frostcap", durationMs: 1000, warnings: [] } as never;
      return null as never;
    });
    useApp.setState({ settings: { ...useApp.getState().settings, captureTitles } });
    await useRecording.getState().start(src);
    await useRecording.getState().stop();
    return calls;
  }

  it("asks the backend not to record titles by default (no post-recording scrub)", async () => {
    const calls = await record(false);
    const start = calls.find((c) => c.cmd === "start_recording")!;
    expect(start.args.windowTitles).toBeUndefined(); // backend default: off
    expect(calls.map((c) => c.cmd)).not.toContain("scrub_project");
    expect(useApp.getState().projectPath).toBe("C:\\v\\T.frostcap");
  });

  it("sends windowTitles: true only when the user opted in", async () => {
    const calls = await record(true);
    expect(calls.find((c) => c.cmd === "start_recording")!.args.windowTitles).toBe(true);
    expect(calls.map((c) => c.cmd)).not.toContain("scrub_project");
  });
});

describe("recording://stopped (auto-stop)", () => {
  it("returns to idle, explains the error and opens what was saved", async () => {
    await useRecording.getState().start(src);
    expect(useRecording.getState().phase).toBe("recording");
    __mockEmit("recording://stopped", {
      result: { projectDir: "C:\\v\\New.frostcap", durationMs: 3489, warnings: [] },
      error: { code: "disk_full", message: "only 280 MB left on the recording drive; recording stopped" },
    });
    await tick();
    expect(useRecording.getState().phase).toBe("idle");
    expect(useApp.getState().toasts.at(-1)).toMatchObject({ kind: "error" });
    expect(useApp.getState().toasts.at(-1)!.text).toMatch(/280 MB/);
    expect(useApp.getState().projectPath).toBe("C:\\v\\New.frostcap");
  });

  it("ticks carry the paused flag", async () => {
    await useRecording.getState().start(src);
    __mockEmit("recording://tick", { elapsedMs: 1200, paused: true });
    await tick();
    expect(useRecording.getState()).toMatchObject({ elapsedMs: 1200, phase: "paused" });
    __mockEmit("recording://tick", { elapsedMs: 1300, paused: false });
    await tick();
    expect(useRecording.getState().phase).toBe("recording");
    __mockEmit("recording://stopped", { result: null, error: null });
    await tick();
  });
});
