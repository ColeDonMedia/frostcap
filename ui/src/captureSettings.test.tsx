// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { __setInvoke, backend, fromWireSources } from "./backend";
import { mockInvoke } from "./mockBackend";
import { useApp } from "./store/app";
import SettingsView from "./views/SettingsView";
import RecorderView from "./views/RecorderView";
import { click, flush, render, type Rendered } from "./test/render";

const here = dirname(fileURLToPath(import.meta.url));
const cmd = (name: string) => JSON.parse(readFileSync(join(here, "../../fixtures/commands", `${name}.json`), "utf8")) as { args: unknown; response: unknown };

afterEach(() => __setInvoke(null));

describe("capture settings adapter and mock match the fixtures", () => {
  it("capture_settings", async () => {
    const f = cmd("capture_settings");
    __setInvoke(async () => f.response as never);
    expect(await backend.captureSettings()).toEqual(f.response);
    expect(await mockInvoke("capture_settings", {})).toEqual(f.response);
  });
  it("set_capture_border and set_exclude_own_windows send {enabled} and return the settings", async () => {
    for (const [name, call, key] of [
      ["set_capture_border", (e: boolean) => backend.setCaptureBorder(e), "captureBorder"],
      ["set_exclude_own_windows", (e: boolean) => backend.setExcludeOwnWindows(e), "excludeOwnWindows"],
    ] as const) {
      const f = cmd(name);
      const sent: unknown[] = [];
      __setInvoke(async (c, a) => (sent.push([c, a]), f.response as never));
      expect(await call((f.args as { enabled: boolean }).enabled)).toEqual(f.response);
      expect(sent).toEqual([[name, f.args]]);
      // The mock applies the change and answers with the same shape as the fixture.
      __setInvoke(null);
      const res = (await mockInvoke(name, { enabled: false })) as Record<string, boolean>;
      expect(Object.keys(res).sort()).toEqual(Object.keys(f.response as object).sort());
      expect(res[key]).toBe(false);
      await mockInvoke(name, { enabled: true });
    }
  });
});

describe("Settings: Capture group", () => {
  let r: Rendered;
  let calls: { cmd: string; args: unknown }[] = [];
  let fail = false;
  beforeEach(async () => {
    calls = [];
    fail = false;
    localStorage.clear();
    useApp.setState({ toasts: [] });
    __setInvoke(async (c, a) => {
      calls.push({ cmd: c, args: a });
      if (c === "capture_settings") return { captureBorder: true, excludeOwnWindows: false } as never;
      if (c === "set_capture_border" || c === "set_exclude_own_windows") {
        if (fail) throw { code: "internal", message: "boom" };
        const on = (a as { enabled: boolean }).enabled;
        return (c === "set_capture_border" ? { captureBorder: on, excludeOwnWindows: false } : { captureBorder: true, excludeOwnWindows: on }) as never;
      }
      if (c === "hotkeys") return { chords: [] } as never;
      if (c === "default_output_dir") return "C:\\V" as never;
      return null as never;
    });
    r = await render(<SettingsView />);
    await flush(10);
  });
  afterEach(() => r.unmount());

  const sw = (id: string) => r.container.querySelector<HTMLElement>(`#${id}`)!;

  it("loads both values from capture_settings", () => {
    expect(r.container.textContent).toContain("Show capture border");
    expect(r.container.textContent).toContain("Hide Frostcap windows from recordings");
    expect(sw("set-capture-border").getAttribute("aria-checked")).toBe("true");
    expect(sw("set-exclude-own").getAttribute("aria-checked")).toBe("false");
  });

  it("saves a change with its setter", async () => {
    await click(sw("set-exclude-own"));
    await flush(10);
    expect(calls.find((c) => c.cmd === "set_exclude_own_windows")?.args).toEqual({ enabled: true });
    expect(sw("set-exclude-own").getAttribute("aria-checked")).toBe("true");
  });

  it("reverts silently (no toast) when the user declines the confirmation (code cancelled)", async () => {
    __setInvoke(async (cmd, a) => {
      calls.push({ cmd, args: a });
      if (cmd === "capture_settings") return { captureBorder: true, excludeOwnWindows: false } as never;
      if (cmd === "set_capture_border") throw { code: "cancelled", message: "cancelled" };
      return null as never;
    });
    r.unmount();
    r = await render(<SettingsView />);
    await flush(10);
    await click(sw("set-capture-border"));
    await flush(10);
    expect(sw("set-capture-border").getAttribute("aria-checked")).toBe("true");
    expect(useApp.getState().toasts).toHaveLength(0);
  });

  it("reverts the toggle and shows an error toast when saving fails", async () => {
    fail = true;
    await click(sw("set-capture-border"));
    await flush(10);
    expect(sw("set-capture-border").getAttribute("aria-checked")).toBe("true");
    const toast = useApp.getState().toasts.at(-1);
    expect(toast?.kind).toBe("error");
  });
});

describe("source list: allDisplays", () => {
  const monitor = { id: "\\\\.\\DISPLAY1", name: "A", primary: true, bounds: { x: 0, y: 0, width: 2560, height: 1440 }, scaleFactor: 1, refreshRate: 60 };
  const base = { monitors: [monitor], windows: [] };
  const all = { id: "all-displays", kind: "allDisplays", name: "All displays", width: 4480, height: 1440, source: { kind: "allDisplays" } };

  it("maps the item and keeps its wire source for start_recording", () => {
    const list = fromWireSources({ ...base, sources: [{ id: "display:1", kind: "display", name: "A", width: 2560, height: 1440, source: { kind: "monitor", id: monitor.id } }, all] } as never);
    expect(list[1]).toMatchObject({ id: "all-displays", kind: "allDisplays", wire: { kind: "allDisplays" } });
    expect(list[1].thumbnail).toBeNull();
  });

  it("works when the backend omits it", () => {
    const list = fromWireSources({ ...base, sources: [{ id: "display:1", kind: "display", name: "A", width: 2560, height: 1440, source: { kind: "monitor", id: monitor.id } }] } as never);
    expect(list.some((s) => s.kind === "allDisplays")).toBe(false);
  });

  it("the recorder shows All displays with an icon, under the Displays filter, and selects it", async () => {
    __setInvoke(async (c) => {
      if (c === "list_sources") return { ...base, sources: [{ id: "display:1", kind: "display", name: "A", width: 2560, height: 1440, source: { kind: "monitor", id: monitor.id } }, all] } as never;
      if (c === "list_recent_projects") return [] as never;
      return null as never;
    });
    const r = await render(<RecorderView />);
    await flush(20);
    const card = [...r.container.querySelectorAll<HTMLElement>("[role=radio]")].find((e) => e.textContent?.includes("All displays"))!;
    expect(card).toBeTruthy();
    expect(card.querySelector("img")).toBeNull();
    expect(card.querySelector("svg")).toBeTruthy();
    const displays = [...r.container.querySelectorAll<HTMLElement>("[role=radio]")].find((e) => e.textContent?.trim() === "Displays")!;
    await click(displays);
    expect([...r.container.querySelectorAll("[role=radio]")].some((e) => e.textContent?.includes("All displays"))).toBe(true);
    r.unmount();
  });
});
