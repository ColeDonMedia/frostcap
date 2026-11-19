import { getLocale, t } from "../i18n";

export function fmtTime(ms: number, withMs = false): string {
  const neg = ms < 0;
  ms = Math.max(0, Math.abs(ms));
  const total = Math.floor(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  const base = `${neg ? "-" : ""}${m}:${s.toString().padStart(2, "0")}`;
  if (!withMs) return base;
  const cs = Math.floor((ms % 1000) / 10);
  return `${base}.${cs.toString().padStart(2, "0")}`;
}

export function fmtClock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => n.toString().padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

export function fmtRelative(iso: string): string {
  const d = new Date(iso).getTime();
  if (!Number.isFinite(d)) return "";
  const diff = Date.now() - d;
  const min = Math.round(diff / 60000);
  if (min < 1) return t("time.justNow");
  if (min < 60) return t("time.minAgo", { n: min });
  const h = Math.round(min / 60);
  if (h < 24) return t("time.hAgo", { n: h });
  const days = Math.round(h / 24);
  if (days < 30) return t("time.dAgo", { n: days });
  return new Date(iso).toLocaleDateString(getLocale());
}

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

let idc = 0;
export const uid = (p = "id") => `${p}_${Date.now().toString(36)}${(idc++).toString(36)}${Math.random().toString(36).slice(2, 6)}`;
