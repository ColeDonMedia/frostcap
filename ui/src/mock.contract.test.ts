/** The browser mock must answer with the same shapes as Rust (golden fixtures). */
import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { mockInvoke, mockListen } from "./mockBackend";

const here = dirname(fileURLToPath(import.meta.url));
const dir = join(here, "../../fixtures");
const fx = (n: string) => JSON.parse(readFileSync(join(dir, `${n}.json`), "utf8"));

/** Shape signature: object keys (recursively for plain objects, first element for arrays), JS types for leaves. */
function shape(v: unknown, optional: Set<string> = new Set(), path = ""): unknown {
  if (Array.isArray(v)) return v.length ? [shape(v[0], optional, path + "[]")] : [];
  if (v && typeof v === "object") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(v).sort()) if (!optional.has(`${path}.${k}`)) out[k] = shape((v as Record<string, unknown>)[k], optional, `${path}.${k}`);
    return out;
  }
  return v === null ? "null" : typeof v;
}

/** An empty array carries no element shape: let it match any array on the other side. */
function unify(actual: unknown, expected: unknown): unknown {
  if (Array.isArray(actual) && Array.isArray(expected)) return actual.length === 0 ? [] : expected.length ? [unify(actual[0], expected[0])] : expected;
  if (actual && expected && typeof actual === "object" && typeof expected === "object") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(expected)) out[k] = unify((actual as Record<string, unknown>)[k], (expected as Record<string, unknown>)[k]);
    return out;
  }
  return expected;
}

// Fields Rust omits when None (skip_serializing_if) or that the demo legitimately varies.
const OPTIONAL = new Set([".sources[].thumbnail", ".media.webcam", ".media.webcamVideo", ".media.micAudio", ".media.systemAudio", ".edits.ui", ".output", ".pauses", "[].interrupted"]);

describe("mock responses have the Rust shapes", () => {
  const cmds = readdirSync(join(dir, "commands")).map((f) => f.replace(/\.json$/, ""));
  it.each(cmds.filter((c) => !["start_recording", "stop_recording", "pause_recording", "resume_recording", "recording_state"].includes(c)))("%s", async (name) => {
    const f = fx(`commands/${name}`);
    const res = await mockInvoke(name, structuredClone(f.args));
    // The scrub fixture shows the response type only; a real scrub with keys:true empties tracks.keys.
    const opt = name === "scrub_project" ? new Set([...OPTIONAL, ".tracks.keys"]) : OPTIONAL;
    const got = shape(res, opt);
    expect(got).toEqual(unify(got, shape(f.response, opt)));
    if (name === "scrub_project") expect((res as { tracks: { keys: unknown[] } }).tracks.keys).toEqual([]);
  });

  it("recording lifecycle: start, tick, state, pause/resume, stop", async () => {
    const ticks: unknown[] = [];
    const un = mockListen("recording://tick", (p) => ticks.push(p));
    const start = await mockInvoke("start_recording", fx("commands/start_recording").args);
    expect(shape(start)).toEqual(shape(fx("commands/start_recording").response));
    expect(shape(await mockInvoke("recording_state"))).toEqual(shape(fx("commands/recording_state").response));
    expect(await mockInvoke("pause_recording")).toBe(true);
    expect(await mockInvoke("resume_recording")).toBe(true);
    await new Promise((r) => setTimeout(r, 250));
    expect(shape(ticks[0])).toEqual(shape(fx("events/recording_tick")));
    const stop = await mockInvoke("stop_recording");
    expect(shape(stop)).toEqual(shape(fx("commands/stop_recording").response));
    un();
  });

  it("errors are {code, message} with Rust codes", async () => {
    await expect(mockInvoke("stop_recording")).rejects.toMatchObject({ code: "internal" });
    await expect(mockInvoke("load_project", { path: "C:\\Windows" })).rejects.toMatchObject({ code: "invalid_path" });
  });

  it("export progress events have the Rust shape", async () => {
    const seen: unknown[] = [];
    const un = mockListen("export://progress", (p) => seen.push(p));
    const f = fx("commands/export_project");
    await mockInvoke("export_project", { ...f.args, __fast: true });
    expect(shape(seen[0])).toEqual(shape(fx("events/export_progress")));
    un();
  });
});
