import { describe, expect, it } from "vitest";
import { buildFlow, parentDir, type Deps } from "./flow";
import { runSteps } from "./runner";

const BS = String.fromCharCode(92);
/** Windows-style path written with forward slashes for readability. */
const win = (s: string) => s.replaceAll("/", BS);
const PROJECT = win("C:/V/Frostcap/x.frostcap");

function fakeDeps(over: Partial<Deps> = {}, log: string[] = []): Deps {
  const project = { name: "p", media: { dir: PROJECT, durationMs: 2600, screen: { url: "asset://screen" }, systemAudio: { url: "asset://sys" } }, zoomSegments: [{ id: "z" }], captions: [{ text: "Hello from the self-test" }], style: { frame: "macos" } };
  return {
    backend: {
      listSources: async () => (log.push("list_sources"), [{ id: "primary", kind: "display", primary: true, name: "Screen", width: 1, height: 1, wire: { kind: "primaryMonitor" } }]) as never,
      startRecording: async () => (log.push("start"), PROJECT),
      pauseRecording: async () => (log.push("pause"), true),
      resumeRecording: async () => (log.push("resume"), true),
      stopRecording: async () => (log.push("stop"), { projectPath: PROJECT, durationMs: 2600, warnings: [] }),
      loadProject: async () => (log.push("load"), project) as never,
      generateAutoZoom: async () => (log.push("autozoom"), []),
      exportProject: async (r) => (log.push("export:" + r.outPath), { jobId: r.jobId, outPath: r.outPath, bytes: 1234, baked: false }),
      cleanup: async (paths) => void log.push("cleanup:" + paths.length),
    },
    editor: {
      load: async () => void log.push("ed.load"),
      setPlayhead: () => void log.push("ed.playhead"),
      addZoomAtPlayhead: () => void log.push("ed.zoom"),
      setCaptions: () => void log.push("ed.captions"),
      setFrame: () => void log.push("ed.frame"),
      save: async () => void log.push("ed.save"),
    },
    sleep: async (ms) => void log.push("sleep:" + ms),
    tone: { start: () => void log.push("tone.start"), stop: () => void log.push("tone.stop") },
    probeVideo: async () => ({ duration: 2.6, seekedTo: 1.3 }),
    probeAudio: async () => ({ peak: 0.4, rms: 0.2 }),
    assetUrl: async (p) => "asset://" + p,
    ...over,
  };
}

describe("parentDir", () => {
  it("handles both separators", () => {
    expect(parentDir(PROJECT)).toBe(win("C:/V/Frostcap"));
    expect(parentDir("/a/b/c")).toBe("/a/b");
  });
});

describe("self-test flow", () => {
  it("runs the scripted sequence in order and passes", async () => {
    const log: string[] = [];
    const rep = await runSteps(buildFlow(fakeDeps({}, log)), {});
    expect(rep.steps.map((s) => s.name)).toEqual(["list_sources", "record", "load_project", "video", "system_audio", "auto_zoom", "edit", "reload", "export", "cleanup"]);
    expect(rep.ok, JSON.stringify(rep.steps.filter((s) => !s.ok))).toBe(true);
    const calls = log.filter((l) => !l.startsWith("sleep"));
    expect(calls.slice(0, 7)).toEqual(["list_sources", "tone.start", "start", "pause", "resume", "stop", "tone.stop"]);
    expect(log.find((l) => l.startsWith("export:"))).toContain(win("C:/V/Frostcap/frostcap-selftest-"));
    expect(log.at(-1)).toBe("cleanup:2");
  });

  it("pauses for 0.5 s between two recording stretches", async () => {
    const log: string[] = [];
    await runSteps(buildFlow(fakeDeps({}, log)), {});
    const i = log.indexOf("pause");
    expect(log[i + 1]).toBe("sleep:500");
    expect(log[i + 2]).toBe("resume");
  });

  it("fails the audio step when the system WAV is silent, still cleans up and stops the tone", async () => {
    const log: string[] = [];
    const rep = await runSteps(buildFlow(fakeDeps({ probeAudio: async () => ({ peak: 0, rms: 0 }) }, log)), {});
    expect(rep.ok).toBe(false);
    const audio = rep.steps.find((s) => s.name === "system_audio")!;
    expect(audio.ok).toBe(false);
    expect(audio.error).toMatch(/silent/i);
    expect(rep.steps.find((s) => s.name === "export")!.skipped).toBe(true);
    expect(log.at(-1)).toBe("cleanup:1");
  });

  it("stops the tone even when recording fails", async () => {
    const log: string[] = [];
    const base = fakeDeps();
    const rep = await runSteps(buildFlow(fakeDeps({ backend: { ...base.backend, startRecording: async () => { throw { code: "permission", message: "denied" }; } } }, log)), {});
    expect(rep.steps.find((s) => s.name === "record")!.error).toBe("permission: denied");
    expect(log).toContain("tone.stop");
  });

  it("fails when the exported file has the wrong duration", async () => {
    let n = 0;
    const rep = await runSteps(buildFlow(fakeDeps({ probeVideo: async () => ({ duration: n++ === 0 ? 2.6 : 9, seekedTo: n === 1 ? 1.3 : 1 }) })), {});
    expect(rep.steps.find((s) => s.name === "export")!.error).toMatch(/duration/);
  });
});
