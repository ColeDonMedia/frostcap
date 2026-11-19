import { afterEach, describe, expect, it, vi } from "vitest";
import { LOCALES, detectLocale, getLocale, loadCatalog, setLocale, t, tRich } from "./index";
import { type Locale, type Msg } from "./catalogs";
import { en } from "./en";

afterEach(async () => {
  await setLocale("en");
});

describe("detectLocale", () => {
  it("maps navigator languages to supported locales", async () => {
    expect(detectLocale(["es-MX"])).toBe("es");
    expect(detectLocale(["de-AT", "en"])).toBe("de");
    expect(detectLocale(["fr-CA"])).toBe("fr");
    expect(detectLocale(["pt-BR"])).toBe("pt-BR");
    expect(detectLocale(["pt-PT"])).toBe("pt-BR");
    expect(detectLocale(["ja-JP"])).toBe("ja");
  });
  it("skips unsupported languages and falls back to English", async () => {
    expect(detectLocale(["sv-SE", "de"])).toBe("de");
    expect(detectLocale(["sv"])).toBe("en");
    expect(detectLocale([])).toBe("en");
    expect(detectLocale(["xx_bad"])).toBe("en");
  });
});

describe("t()", () => {
  it("interpolates", async () => {
    await setLocale("en");
    const s = t("recorder.capturing", { name: "Screen 1", fps: 60 });
    expect(s).toContain("Screen 1");
    expect(s).toContain("60");
    expect(s).not.toContain("{");
  });
  it("selects plural forms with Intl.PluralRules", async () => {
    await setLocale("en");
    expect(t("recorder.interrupted", { count: 1 })).toBe("A recording was interrupted");
    expect(t("recorder.interrupted", { count: 3 })).toBe("3 recordings were interrupted");
    await setLocale("ja");
    expect(t("recorder.interrupted", { count: 3 })).toContain("3");
    // French treats 0 as singular ("one"), English as plural.
    expect(new Intl.PluralRules("fr").select(0)).toBe("one");
    await setLocale("fr");
    expect(t("recorder.interrupted", { count: 0 })).not.toContain("{");
  });
  it("falls back to the key for unknown keys", async () => {
    await setLocale("de");
    // @ts-expect-error not a key
    expect(t("no.such.key")).toBe("no.such.key");
  });
  it("tracks the active locale", async () => {
    await setLocale("de");
    expect(getLocale()).toBe("de");
    expect(t("nav.record")).not.toBe(en["nav.record"]);
  });
});

describe("tRich", () => {
  it("wraps <b> segments", async () => {
    await setLocale("en");
    const parts = tRich("recorder.capturing", { name: "X", fps: 30 }, (s) => `[${s}]`);
    expect(parts.join("")).toContain("[X]");
  });
});

describe("LOCALES", () => {
  it("lists English plus five translations with native names", async () => {
    expect(LOCALES.map((l) => l.id)).toEqual(["en", "es", "de", "fr", "pt-BR", "ja"]);
    expect(LOCALES.find((l) => l.id === "ja")?.name).toBe("日本語");
  });
});

const names = (m: Msg): string[] =>
  [...new Set((typeof m === "string" ? [m] : Object.values(m)).flatMap((f) => [...f.matchAll(/\{(\w+)\}/g)].map((x) => x[1])))].sort();
// A singular plural form may drop {count} ("A recording was interrupted").
const withoutCount = (a: string[]) => a.filter((x) => x !== "count");

describe("catalog integrity", async () => {
  const keys = Object.keys(en).sort();
  const base = en as Record<string, Msg>;
  for (const id of ["es", "de", "fr", "pt-BR", "ja"] as Locale[]) {
    const cat = await loadCatalog(id as Exclude<Locale, "en">);
    const c = cat as Record<string, Msg>;
    it(`${id} has exactly the English keys`, () => {
      expect(Object.keys(cat).sort()).toEqual(keys);
    });
    it(`${id} keeps the placeholders of English`, () => {
      const bad = keys.filter((k) => withoutCount(names(base[k])).join() !== withoutCount(names(c[k])).join());
      expect(bad).toEqual([]);
    });
    it(`${id} has no empty messages and every plural has "other"`, () => {
      for (const k of keys) {
        const m = c[k];
        if (typeof m === "string") expect(m.trim(), k).not.toBe("");
        else expect(m.other?.trim(), k).toBeTruthy();
      }
    });
  }
});

describe("lazy loading", () => {
  it("keeps English until the chosen catalog has loaded, then switches and notifies", async () => {
    vi.resetModules();
    const { subscribeLocale, setLocale, getLocale } = await import("./index");
    let notified = 0;
    const un = subscribeLocale(() => notified++);
    const p = setLocale("pt-BR");
    expect(getLocale()).toBe("en");
    await p;
    expect(getLocale()).toBe("pt-BR");
    expect(notified).toBeGreaterThan(0);
    un();
  });
  it("ignores a stale load when a newer language was requested", async () => {
    const a = setLocale("fr");
    const b = setLocale("ja");
    await Promise.all([a, b]);
    expect(getLocale()).toBe("ja");
  });
});
