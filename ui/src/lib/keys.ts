import type { View } from "../store/app";

/** True when keyboard input should go to a form control instead of app shortcuts. */
export function isTypingTarget(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el) return false;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable === true;
}

const NAV: Record<string, View> = { "1": "recorder", "2": "editor", "3": "settings" };

/** Ctrl+1/2/3 view switching; null when the event is not a nav shortcut or the user is typing. */
export function navShortcut(e: KeyboardEvent): View | null {
  if (!e.ctrlKey || e.altKey || e.shiftKey || e.metaKey) return null;
  if (isTypingTarget(e.target)) return null;
  return NAV[e.key] ?? null;
}
