// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { __setInvoke } from "../backend";
import { useApp } from "../store/app";
import SettingsView from "./SettingsView";
import { byText, click, flush, key, render, type Rendered } from "../test/render";

let r: Rendered;
let calls: string[] = [];
let setArgs: unknown = null;
let failSet = false;
beforeEach(async () => {
  calls = [];
  setArgs = null;
  failSet = false;
  try {
    localStorage.clear();
  } catch {
    /* ignore */
  }
  __setInvoke(async (cmd, args) => {
    calls.push(cmd);
    if (cmd === "set_hotkeys") {
      if (failSet) throw { code: "internal", message: "Ctrl+J is already registered by another application" };
      setArgs = args;
      const b = (args as { bindings: { startStop: string; pauseResume: string } }).bindings;
      return { chords: [{ action: "startStop", chord: b.startStop, registered: true }, { action: "pauseResume", chord: b.pauseResume, registered: true }] } as never;
    }
    if (cmd === "default_output_dir") return "C:\\Users\\me\\Videos\\Frostcap" as never;
    if (cmd === "pick_folder") return "D:\\Captures" as never;
    if (cmd === "hotkeys") return { chords: [{ action: "startStop", chord: "Ctrl+Alt+Shift+R", registered: true }, { action: "pauseResume", chord: "Ctrl+Alt+Shift+P", registered: false }] } as never;
    return null as never;
  });
  useApp.setState({ settings: { ...useApp.getState().settings, outputFolder: "", keyCapture: "off", captureTitles: false } });
  r = await render(<SettingsView />);
  await flush(10);
});
afterEach(() => {
  r.unmount();
  __setInvoke(null);
});

const input = () => r.container.querySelector<HTMLInputElement>("#output-folder")!;

async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function blur(el: HTMLElement) {
  await act(async () => {
    el.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    el.dispatchEvent(new FocusEvent("blur"));
  });
}

describe("Settings output folder", () => {
  it("shows the backend default instead of an unexpanded %USERPROFILE%", () => {
    expect(r.container.textContent).not.toContain("%USERPROFILE%");
    expect(input().value).toBe("C:\\Users\\me\\Videos\\Frostcap");
  });

  it("validates and saves on blur, not on every keystroke", async () => {
    await type(input(), "relative\\path");
    expect(useApp.getState().settings.outputFolder).toBe("");
    expect(r.container.querySelector("#output-folder-error")).toBeNull();
    await blur(input());
    expect(r.container.querySelector("#output-folder-error")?.textContent).toMatch(/full path/i);
    expect(input().getAttribute("aria-invalid")).toBe("true");
    expect(useApp.getState().settings.outputFolder).toBe("");
    await type(input(), "D:\\Videos\\Frostcap");
    await blur(input());
    expect(r.container.querySelector("#output-folder-error")).toBeNull();
    expect(useApp.getState().settings.outputFolder).toBe("D:\\Videos\\Frostcap");
  });

  it("Browse uses the native folder picker", async () => {
    await click(byText(r.container, "button", "Browse…"));
    await flush(10);
    expect(useApp.getState().settings.outputFolder).toBe("D:\\Captures");
  });
});

describe("Settings privacy (security review)", () => {
  it("All keys needs an explicit confirmation that mentions passwords", async () => {
    const group = r.container.querySelector('[role=radiogroup][aria-label="Key capture"]')!;
    const all = [...group.querySelectorAll<HTMLButtonElement>("[role=radio]")][2];
    await click(all);
    const dlg = r.container.querySelector("[role=alertdialog]")!;
    expect(dlg.textContent).toMatch(/password/i);
    expect(useApp.getState().settings.keyCapture).toBe("off");
    await click(byText(dlg, "button", /Cancel/));
    expect(useApp.getState().settings.keyCapture).toBe("off");
    await click(all);
    await click(byText(r.container.querySelector("[role=alertdialog]")!, "button", /Record all keys/));
    expect(useApp.getState().settings.keyCapture).toBe("all");
  });

  it("window titles are opt-in (default off) with a privacy note", async () => {
    const sw = r.container.querySelector<HTMLButtonElement>('[role=switch][aria-label="Record window titles"]')!;
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(sw.closest(".setting")!.textContent).toMatch(/title/i);
    await click(sw);
    expect(useApp.getState().settings.captureTitles).toBe(true);
  });

  it("never presents an unregistered hotkey as active", () => {
    // (Superseded "list only registered": the hotkey editor must show every chord, so it marks unregistered ones instead.)
    const rows = [...r.container.querySelectorAll('[aria-labelledby="s-hotkeys"] .hotkey')];
    const p = rows.find((row) => row.textContent?.includes("Ctrl+Alt+Shift+P"))!;
    expect(p.textContent).toMatch(/not active/i);
    const rr = rows.find((row) => row.textContent?.includes("Ctrl+Alt+Shift+R"))!;
    expect(rr.textContent).not.toMatch(/not active/i);
  });
});

describe("Settings global hotkeys (set_hotkeys)", () => {
  it("shows each chord and flags ones another app owns", () => {
    const sec = r.container.querySelector('[aria-labelledby="s-hotkeys"]')!;
    expect(sec.textContent).toContain("Ctrl+Alt+Shift+R");
    expect(sec.textContent).toContain("Ctrl+Alt+Shift+P");
    expect(sec.textContent).toMatch(/in use by another app/i);
  });

  it("records a new chord, rejects duplicates, and saves through set_hotkeys", async () => {
    const sec = r.container.querySelector('[aria-labelledby="s-hotkeys"]')!;
    await click(byText(sec, "button", /Change start\/stop/));
    // same chord as pause/resume -> error, nothing saved
    await key(document.activeElement, "P", { code: "KeyP", ctrlKey: true, altKey: true, shiftKey: true } as KeyboardEventInit);
    expect(sec.querySelector("[role=alert]")?.textContent).toMatch(/different/i);
    expect(calls.some((c) => c === "set_hotkeys")).toBe(false);
    // Ctrl+Alt only (AltGr) -> error, nothing saved (mirrors the backend)
    await key(document.activeElement, "K", { code: "KeyK", ctrlKey: true, altKey: true } as KeyboardEventInit);
    expect(sec.querySelector("[role=alert]")?.textContent).toMatch(/AltGr/);
    // a single modifier -> error, nothing saved
    await key(document.activeElement, "K", { code: "KeyK", ctrlKey: true } as KeyboardEventInit);
    expect(sec.querySelector("[role=alert]")?.textContent).toMatch(/two modifiers/i);
    expect(calls.some((c) => c === "set_hotkeys")).toBe(false);
    await key(document.activeElement, "K", { code: "KeyK", ctrlKey: true, shiftKey: true } as KeyboardEventInit);
    await flush(10);
    expect(setArgs).toEqual({ bindings: { startStop: "Ctrl+Shift+K", pauseResume: "Ctrl+Alt+Shift+P" } });
    expect(sec.textContent).toContain("Ctrl+Shift+K");
  });

  it("shows the backend's error when saving fails", async () => {
    failSet = true;
    const sec = r.container.querySelector('[aria-labelledby="s-hotkeys"]')!;
    await click(byText(sec, "button", /Change pause\/resume/));
    await key(document.activeElement, "J", { code: "KeyJ", ctrlKey: true, shiftKey: true } as KeyboardEventInit);
    await flush(10);
    expect(sec.querySelector("[role=alert]")?.textContent).toMatch(/already registered/);
  });
});

describe("Settings privacy", () => {
  it("offers Off / Shortcuts / All keys for key capture, default Off", async () => {
    const group = r.container.querySelector('[role=radiogroup][aria-label="Key capture"]')!;
    const radios = [...group.querySelectorAll<HTMLButtonElement>("[role=radio]")];
    expect(radios.map((b) => b.textContent)).toEqual(["Off", "Shortcuts only", "All keys"]);
    expect(radios[0].getAttribute("aria-checked")).toBe("true");
    await click(radios[1]);
    expect(useApp.getState().settings.keyCapture).toBe("shortcuts");
  });
});
