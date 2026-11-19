// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { __setInvoke } from "../backend";
import { useApp } from "../store/app";
import RecorderView from "./RecorderView";
import { byText, click, flush, render, type Rendered } from "../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const project = JSON.parse(readFileSync(join(here, "../../../fixtures/project.sample.json"), "utf8"));
const recent = (n: number, interrupted: boolean) => ({
  path: `C:\\v\\R${n}.frostcap`,
  name: `R${n}`,
  createdAtMs: 0,
  durationMs: 1000,
  canvas: { width: 1920, height: 1080 },
  status: interrupted ? "recording" : "complete",
  interrupted,
});

let calls: { cmd: string; args: Record<string, unknown> }[] = [];
let r: Rendered;
beforeEach(async () => {
  calls = [];
  __setInvoke(async (cmd, args = {}) => {
    calls.push({ cmd, args });
    if (cmd === "list_sources") return { monitors: [], windows: [], sources: [] } as never;
    if (cmd === "list_recent_projects") return [recent(1, true), recent(2, true), recent(3, true), recent(4, false)] as never;
    if (cmd === "recover_project") return project as never;
    if (cmd === "pick_region") return { monitorId: "\\\\.\\DISPLAY1", rect: { x: 0.25, y: 0.25, width: 0.5, height: 0.5 }, screenRect: { x: 640, y: 360, width: 1280, height: 720 } } as never;
    return null as never;
  });
  useApp.setState({ view: "recorder", projectPath: null, settings: { ...useApp.getState().settings, keyCapture: "off" } });
  r = await render(<RecorderView />);
  await flush(20);
});
afterEach(() => {
  r.unmount();
  __setInvoke(null);
});

describe("recovery banner", () => {
  it("lists every interrupted recording", () => {
    const banner = r.container.querySelector(".banner")!;
    for (const n of ["R1", "R2", "R3"]) expect(banner.textContent).toContain(n);
    expect(banner.textContent).not.toContain("R4");
    expect(banner.querySelectorAll("li")).toHaveLength(3);
  });

  it("Recover all recovers each one", async () => {
    await click(byText(r.container, ".banner button", /Recover all/));
    await flush(20);
    expect(calls.filter((c) => c.cmd === "recover_project").map((c) => c.args.path)).toEqual(["C:\\v\\R1.frostcap", "C:\\v\\R2.frostcap", "C:\\v\\R3.frostcap"]);
  });
});

describe("region source", () => {
  it("picks a region with the native overlay and selects it for recording", async () => {
    const { useRecording } = await import("../store/recording");
    await click(byText(r.container, "button", /Select region/));
    await flush(20);
    expect(calls.map((c) => c.cmd)).toContain("pick_region");
    const sel = r.container.querySelector('.srccard[aria-checked="true"]')!;
    expect(sel.textContent).toMatch(/Region/);
    expect(sel.textContent).toMatch(/1280×720/);
    expect(useRecording.getState().source?.wire).toEqual({ kind: "region", monitorId: "\\\\.\\DISPLAY1", rect: { x: 0.25, y: 0.25, width: 0.5, height: 0.5 } });
  });
});

describe("key capture switch", () => {
  it("maps to the Rust KeyCapture mode", async () => {
    const sw = r.container.querySelector<HTMLButtonElement>('[role=switch][aria-label="Capture keystrokes"]')!;
    await click(sw);
    expect(useApp.getState().settings.keyCapture).toBe("shortcuts");
    await click(sw);
    expect(useApp.getState().settings.keyCapture).toBe("off");
  });

  it("turning keys on from the recorder never selects All keys (that needs Settings + confirmation)", async () => {
    await act(async () => useApp.getState().setSetting("keyCapture", "all"));
    const sw = r.container.querySelector<HTMLButtonElement>('[role=switch][aria-label="Capture keystrokes"]')!;
    await click(sw); // off
    await click(sw); // on again
    expect(useApp.getState().settings.keyCapture).toBe("shortcuts");
  });
});
