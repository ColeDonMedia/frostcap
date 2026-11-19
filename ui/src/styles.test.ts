import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "styles.css"), "utf8");

describe("styles.css guardrails", () => {
  it("no text smaller than 12px", () => {
    const small = [...css.matchAll(/font-size:\s*([\d.]+)px/g)].map((m) => Number(m[1])).filter((n) => n < 12);
    expect(small).toEqual([]);
  });

  it("pages reserve room for the floating recorder bar at every width", () => {
    expect(css).toMatch(/--recbar-space:\s*\d+px/);
    expect(css).toMatch(/\.page\s*\{[^}]*padding-bottom:\s*var\(--recbar-space\)/);
  });

  it("wide screens cap the page width", () => {
    expect(css).toMatch(/\.page__inner\s*\{[^}]*max-width:\s*\d+px/);
  });

  it("styles exist for the new UI pieces", () => {
    for (const sel of [".skip-link", ".load-error", ".banner__list", ".field__error", ".toasts", ".handle--clip", ".sheet--small", ".export-status"]) {
      expect(css, sel).toContain(sel);
    }
  });
});
