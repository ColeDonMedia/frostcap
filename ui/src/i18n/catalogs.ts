import type { en } from "./en";

/** A plain string, or plural forms keyed by Intl.PluralRules category (`other` is required). */
export type Msg = string | ({ other: string } & Partial<Record<Intl.LDMLPluralRule, string>>);

export type MessageKey = keyof typeof en;
/** Every locale must define exactly the English keys (checked by tsc and by i18n.test.ts). */
export type Catalog = Record<MessageKey, Msg>;

export type Locale = "en" | "es" | "de" | "fr" | "pt-BR" | "ja";

/** Non-English catalogs load on demand (one chunk each); English lives in en.ts and is eager. */
export const loaders: Record<Exclude<Locale, "en">, () => Promise<Catalog>> = {
  es: () => import("./es").then((m) => m.es),
  de: () => import("./de").then((m) => m.de),
  fr: () => import("./fr").then((m) => m.fr),
  "pt-BR": () => import("./pt-BR").then((m) => m.ptBR),
  ja: () => import("./ja").then((m) => m.ja),
};
