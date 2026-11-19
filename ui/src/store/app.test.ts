// @vitest-environment happy-dom
import { describe, expect, it, vi } from "vitest";

describe("settings migration", () => {
  it("drops the old unexpanded %USERPROFILE% default so the backend default is used", async () => {
    localStorage.setItem("frostcap.settings.v1", JSON.stringify({ outputFolder: "%USERPROFILE%\\Videos\\Frostcap", captureKeys: true }));
    vi.resetModules();
    const { useApp } = await import("./app");
    const s = useApp.getState().settings;
    expect(s.outputFolder).toBe("");
    // the old boolean never turns key capture on by itself
    expect(s.keyCapture).toBe("off");
    expect("captureKeys" in s).toBe(false);
  });

  it("keeps a real folder", async () => {
    localStorage.setItem("frostcap.settings.v1", JSON.stringify({ outputFolder: "D:\\Captures" }));
    vi.resetModules();
    const { useApp } = await import("./app");
    expect(useApp.getState().settings.outputFolder).toBe("D:\\Captures");
  });
});
