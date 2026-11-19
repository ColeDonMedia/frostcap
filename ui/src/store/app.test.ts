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

describe("language setting", () => {
  it("defaults to the system language and follows navigator.language", async () => {
    localStorage.clear();
    vi.stubGlobal("navigator", { language: "de-DE", languages: ["de-DE", "en"] });
    vi.resetModules();
    const { useApp } = await import("./app");
    const { getLocale } = await import("../i18n");
    expect(useApp.getState().settings.language).toBe("system");
    await vi.waitFor(() => expect(getLocale()).toBe("de"));
    vi.unstubAllGlobals();
  });

  it("an explicit choice overrides the OS and is persisted", async () => {
    localStorage.clear();
    vi.stubGlobal("navigator", { language: "de-DE", languages: ["de-DE"] });
    vi.resetModules();
    const { useApp } = await import("./app");
    const { getLocale } = await import("../i18n");
    useApp.getState().setSetting("language", "ja");
    await vi.waitFor(() => expect(getLocale()).toBe("ja"));
    expect(JSON.parse(localStorage.getItem("frostcap.settings.v1")!).language).toBe("ja");
    useApp.getState().setSetting("language", "system");
    await vi.waitFor(() => expect(getLocale()).toBe("de"));
    vi.unstubAllGlobals();
  });

  it("restores a saved language and ignores an unknown one", async () => {
    localStorage.setItem("frostcap.settings.v1", JSON.stringify({ language: "fr" }));
    vi.resetModules();
    let mod = await import("./app");
    await vi.waitFor(async () => expect((await import("../i18n")).getLocale()).toBe("fr"));
    expect(mod.useApp.getState().settings.language).toBe("fr");
    localStorage.setItem("frostcap.settings.v1", JSON.stringify({ language: "klingon" }));
    vi.resetModules();
    mod = await import("./app");
    expect(mod.useApp.getState().settings.language).toBe("system");
  });
});
