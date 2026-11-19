// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import { __setInvoke } from "../backend";
import { useRecording } from "../store/recording";
import { RecorderBar } from "./RecorderBar";
import { flush, render, type Rendered } from "../test/render";

let r: Rendered | null = null;
afterEach(() => {
  r?.unmount();
  r = null;
  __setInvoke(null);
  useRecording.setState({ phase: "idle" });
});

async function mount(registered: boolean) {
  __setInvoke(async (cmd) => {
    if (cmd === "hotkeys")
      return {
        chords: [
          { action: "startStop", chord: "Ctrl+Shift+S", registered },
          { action: "pauseResume", chord: "Ctrl+Shift+P", registered },
        ],
      } as never;
    return null as never;
  });
  useRecording.setState({ phase: "recording", elapsedMs: 1000, pauseSupported: true });
  r = await render(<RecorderBar />);
  await flush(20);
  return [...document.querySelectorAll<HTMLButtonElement>(".recbar button")].map((b) => `${b.title} ${b.getAttribute("aria-keyshortcuts") ?? ""}`).join("|");
}

describe("key capture badge", () => {
  it("warns loudly when every key is being recorded", async () => {
    const { useApp } = await import("../store/app");
    useApp.setState({ settings: { ...useApp.getState().settings, keyCapture: "all" } });
    useRecording.setState({ capturingKeys: true });
    await mount(false);
    const badge = document.querySelector(".recbar__keys")!;
    expect(badge.textContent).toMatch(/All keys/);
    expect(badge.classList.contains("recbar__keys--all")).toBe(true);
    useApp.setState({ settings: { ...useApp.getState().settings, keyCapture: "shortcuts" } });
  });
});

describe("recorder bar hotkey hints", () => {
  it("omits hotkey hints when the backend did not register them", async () => {
    expect(await mount(false)).not.toMatch(/Ctrl\+Shift/);
  });
  it("shows the chords the backend registered", async () => {
    const t = await mount(true);
    expect(t).toMatch(/Ctrl\+Shift\+S/);
    expect(t).toMatch(/Ctrl\+Shift\+P/);
  });
});
