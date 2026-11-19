/**
 * Global hotkey chords, validated like `Chord::from_str` in
 * crates/frostcap-core/src/hotkeys.rs (the backend re-validates in set_hotkeys):
 * Ctrl or Win, at least two modifiers, never Ctrl+Alt alone (AltGr).
 */

const MODS: [string, string][] = [
  ["CTRL", "Ctrl"],
  ["CONTROL", "Ctrl"],
  ["ALT", "Alt"],
  ["SHIFT", "Shift"],
  ["WIN", "Win"],
  ["SUPER", "Win"],
];
const ORDER = ["Ctrl", "Alt", "Shift", "Win"];

export type ChordResult = { ok: true; chord: string } | { ok: false; error: string };

export function validateChord(input: string): ChordResult {
  const fail = (why: string): ChordResult => ({ ok: false, error: `“${input}” is not a valid hotkey. ${why}` });
  const bad = () => fail("Use two modifiers including Ctrl or Win, plus a letter, digit or F1–F24, e.g. Ctrl+Alt+Shift+R.");
  const parts = input.split("+").map((p) => p.trim().toUpperCase());
  if (parts.length < 2) return bad();
  const key = parts.pop()!;
  const mods = new Set<string>();
  for (const p of parts) {
    const m = MODS.find(([k]) => k === p)?.[1];
    if (!m || mods.has(m)) return bad();
    mods.add(m);
  }
  if (!mods.has("Ctrl") && !mods.has("Win")) return bad();
  let k: string;
  if (/^[A-Z0-9]$/.test(key)) k = key;
  else if (/^F([1-9]|1\d|2[0-4])$/.test(key)) k = key;
  else return bad();
  // One modifier collides with ordinary app shortcuts (Ctrl+S...).
  if (mods.size < 2) return fail("Use at least two modifiers, e.g. Ctrl+Shift+R.");
  // Ctrl+Alt is AltGr on many keyboard layouts: it would swallow typed characters.
  if (mods.size === 2 && mods.has("Ctrl") && mods.has("Alt")) return fail("Ctrl+Alt alone is AltGr on many keyboards; add Shift or Win.");
  return { ok: true, chord: [...ORDER.filter((m) => mods.has(m)), k].join("+") };
}

export interface HotkeyBindings {
  startStop: string;
  pauseResume: string;
}

/** Null when valid; otherwise an error message. */
export function validateBindings(b: HotkeyBindings): string | null {
  const a = validateChord(b.startStop);
  if (!a.ok) return a.error;
  const p = validateChord(b.pauseResume);
  if (!p.ok) return p.error;
  if (a.chord === p.chord) return "Start/stop and pause/resume need different hotkeys.";
  return null;
}

/** Chord for a keydown while recording a new hotkey; null while only modifiers are held. */
export function chordFromEvent(e: KeyboardEvent): string | null {
  let key: string | null = null;
  const m = e.code.match(/^(?:Key([A-Z])|Digit([0-9])|(F\d{1,2}))$/);
  if (m) key = m[1] ?? m[2] ?? m[3];
  if (!key) return null;
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Win"].filter(Boolean);
  return [...mods, key].join("+");
}
