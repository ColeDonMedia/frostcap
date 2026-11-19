import { useSyncExternalStore } from "react";
import { en } from "./en";
import { loaders, type Catalog, type Locale, type MessageKey, type Msg } from "./catalogs";

export type { Locale, MessageKey };

/** Native names, so a user can always find their language. Order = the Settings menu order. */
export const LOCALES: { id: Locale; name: string }[] = [
  { id: "en", name: "English" },
  { id: "es", name: "Español" },
  { id: "de", name: "Deutsch" },
  { id: "fr", name: "Français" },
  { id: "pt-BR", name: "Português (Brasil)" },
  { id: "ja", name: "日本語" },
];

const PRIMARY: Record<string, Locale> = { en: "en", es: "es", de: "de", fr: "fr", pt: "pt-BR", ja: "ja" };

/** First supported language of a `navigator.languages`-style list; English when none matches. */
export function detectLocale(languages: readonly string[]): Locale {
  for (const tag of languages) {
    const hit = PRIMARY[tag.toLowerCase().split(/[-_]/)[0]];
    if (hit) return hit;
  }
  return "en";
}

export function systemLocale(): Locale {
  if (typeof navigator === "undefined") return "en";
  return detectLocale(navigator.languages?.length ? navigator.languages : [navigator.language ?? "en"]);
}

let current: Locale = "en";
const loaded: Partial<Record<Locale, Catalog>> = {};
let requestSeq = 0;

/** Resolves a locale's catalog (English is always available; others are fetched once). */
export async function loadCatalog(locale: Exclude<Locale, "en">): Promise<Catalog> {
  return (loaded[locale] ??= await loaders[locale]());
}
const listeners = new Set<() => void>();

export function getLocale(): Locale {
  return current;
}

/**
 * Switches language. English applies immediately; others keep the current text until their catalog
 * has loaded, then switch and notify subscribers. A newer request supersedes an older one.
 */
export async function setLocale(locale: Locale): Promise<void> {
  const seq = ++requestSeq;
  if (locale !== "en" && !loaded[locale]) {
    try {
      await loadCatalog(locale);
    } catch {
      return; // chunk failed to load: stay on the current language
    }
    if (seq !== requestSeq) return;
  }
  if (locale === current) return;
  current = locale;
  if (typeof document !== "undefined") document.documentElement.lang = locale;
  listeners.forEach((l) => l());
}

export function subscribeLocale(l: () => void): () => void {
  listeners.add(l);
  return () => void listeners.delete(l);
}

/** Subscribes a component to locale changes; returns the active locale. */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribeLocale, getLocale, getLocale);
}

type Params = Record<string, string | number>;

const rules = new Map<Locale, Intl.PluralRules>();
function pluralCategory(locale: Locale, n: number): Intl.LDMLPluralRule {
  let r = rules.get(locale);
  if (!r) rules.set(locale, (r = new Intl.PluralRules(locale)));
  return r.select(n);
}

function lookup(key: string): Msg | undefined {
  const own = current === "en" ? undefined : (loaded[current] as Record<string, Msg> | undefined)?.[key];
  return own ?? (en as Record<string, Msg>)[key];
}

function resolve(key: string, params?: Params): string {
  const msg = lookup(key);
  if (msg === undefined) return key;
  let text: string;
  if (typeof msg === "string") text = msg;
  else {
    const n = Number(params?.count);
    text = (Number.isFinite(n) && msg[pluralCategory(current, n)]) || msg.other;
  }
  return params ? text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole)) : text;
}

/** Translate `key` in the active locale (English fallback, then the key itself). */
export function t(key: MessageKey, params?: Params): string {
  return resolve(key, params);
}

/**
 * Like t(), but `<b>…</b>` segments are passed through `wrap` (e.g. to render <strong>).
 * Returns the parts in order; a plain string array when `wrap` returns strings.
 */
export function tRich<T>(key: MessageKey, params: Params | undefined, wrap: (inner: string) => T): (string | T)[] {
  const parts: (string | T)[] = [];
  resolve(key, params)
    .split(/<b>(.*?)<\/b>/s)
    .forEach((seg, i) => {
      if (i % 2) parts.push(wrap(seg));
      else if (seg) parts.push(seg);
    });
  return parts;
}
