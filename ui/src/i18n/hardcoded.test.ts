import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseAst } from "vite";
import { describe, expect, it } from "vitest";

/**
 * Guards the "every user-visible string goes through t()" rule: parses each .tsx in src/
 * and reports JSX text and user-facing attribute string literals that contain letters.
 */
const SRC = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** Allowed literals: brand name, the version line and keyboard key names (untranslated by design). */
const ALLOW = new Set(["Frostcap", "Frostcap 0.1 · MIT", "Home", "End", "Space"]);
/** Attributes whose string value a user can read or hear. */
const TEXT_ATTRS = new Set(["aria-label", "title", "placeholder", "alt", "label", "hint", "ariaLabel", "muteLabel", "aria-description", "aria-roledescription"]);
/** Dev-only tooling and the self-test harness are not product UI. */
const SKIP_DIRS = new Set(["dev", "selftest", "__fixtures__"]);

function files(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) return SKIP_DIRS.has(e.name) ? [] : files(p);
    return e.name.endsWith(".tsx") && !e.name.endsWith(".test.tsx") ? [p] : [];
  });
}

type Node = { type: string; [k: string]: unknown };
const hasLetters = (s: string) => /[\p{L}]/u.test(s);

export function findLiterals(code: string): string[] {
  const found: string[] = [];
  const visit = (n: unknown) => {
    if (Array.isArray(n)) return n.forEach(visit);
    if (!n || typeof n !== "object") return;
    const node = n as Node;
    if (node.type === "JSXText") {
      const text = String(node.value).replace(/\s+/g, " ").trim();
      if (hasLetters(text) && !ALLOW.has(text)) found.push(`text: ${text}`);
    } else if (node.type === "JSXAttribute") {
      const name = (node.name as { name?: string }).name ?? "";
      const v = node.value as Node | null;
      if (TEXT_ATTRS.has(name) && v?.type === "Literal" && typeof v.value === "string" && hasLetters(v.value) && !ALLOW.has(v.value)) found.push(`${name}="${v.value}"`);
    } else if (node.type === "JSXExpressionContainer") {
      const e = node.expression as Node;
      if (e?.type === "Literal" && typeof e.value === "string" && hasLetters(e.value) && !ALLOW.has(e.value)) found.push(`{"${e.value}"}`);
    }
    for (const v of Object.values(node)) if (v && typeof v === "object") visit(v);
  };
  visit(parseAst(code, { lang: "tsx" }));
  return found;
}

describe("no hard-coded user-visible strings", () => {
  it("detects text, attributes and string expressions (self-check)", () => {
    const hits = findLiterals(`const a = <div title="Hello" data-x="no"><b>Save</b>{"Cancel"}{t("k")}<i>×</i>Frostcap</div>;`);
    expect(hits).toEqual(['title="Hello"', "text: Save", '{"Cancel"}']);
  });

  for (const file of files(SRC)) {
    const rel = path.relative(SRC, file).replaceAll("\\", "/");
    it(rel, () => {
      expect(findLiterals(fs.readFileSync(file, "utf8"))).toEqual([]);
    });
  }
});
