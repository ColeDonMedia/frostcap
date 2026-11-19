import { describe, expect, it } from "vitest";
import { chordFromEvent, validateBindings, validateChord } from "./hotkeys";

describe("validateChord (mirrors frostcap-core hotkeys.rs)", () => {
  it("normalizes valid chords", () => {
    expect(validateChord("ctrl+alt+shift+r")).toEqual({ ok: true, chord: "Ctrl+Alt+Shift+R" });
    expect(validateChord("shift+Win+F5")).toEqual({ ok: true, chord: "Shift+Win+F5" });
    expect(validateChord("Control + shift + 9")).toEqual({ ok: true, chord: "Ctrl+Shift+9" });
    expect(validateChord("Ctrl+Win+F24")).toEqual({ ok: true, chord: "Ctrl+Win+F24" });
    expect(validateChord("Ctrl+Alt+Win+A")).toEqual({ ok: true, chord: "Ctrl+Alt+Win+A" });
  });
  it("requires Ctrl or Win and a single A-Z / 0-9 / F1-F24 key", () => {
    for (const bad of ["Alt+Shift+R", "Shift+R", "R", "Ctrl+Ctrl+R", "Ctrl+Shift+Space", "Ctrl+Shift+F25", "Ctrl+Shift+", "Ctrl+Hyper+R", ""]) {
      expect(validateChord(bad).ok, bad).toBe(false);
    }
  });
  it("requires at least two modifiers (one-modifier chords collide with app shortcuts)", () => {
    for (const bad of ["Ctrl+R", "Win+F5", "Control + 9", "Ctrl+F1"]) {
      const r = validateChord(bad);
      expect(r.ok, bad).toBe(false);
      if (!r.ok) expect(r.error).toMatch(/two modifiers/i);
    }
  });
  it("rejects Ctrl+Alt-only chords (AltGr on many layouts types characters)", () => {
    for (const bad of ["Ctrl+Alt+K", "alt+ctrl+9", "Ctrl+Alt+F5"]) {
      const r = validateChord(bad);
      expect(r.ok, bad).toBe(false);
      if (!r.ok) expect(r.error).toMatch(/AltGr/);
    }
    expect(validateChord("Ctrl+Alt+Shift+K").ok).toBe(true);
  });
});

describe("validateBindings", () => {
  it("rejects identical chords for both actions", () => {
    expect(validateBindings({ startStop: "Ctrl+Shift+R", pauseResume: "ctrl+shift+r" })).toMatch(/different/);
    expect(validateBindings({ startStop: "Ctrl+Shift+R", pauseResume: "Ctrl+Shift+P" })).toBeNull();
  });
  it("reports an invalid chord", () => {
    expect(validateBindings({ startStop: "Ctrl+Alt+R", pauseResume: "Ctrl+Shift+P" })).toMatch(/AltGr/);
  });
});

describe("chordFromEvent (recording a new chord)", () => {
  const ev = (key: string, code: string, m: Partial<KeyboardEvent> = {}) => ({ key, code, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...m }) as KeyboardEvent;
  it("builds a chord from modifiers + key code", () => {
    expect(chordFromEvent(ev("R", "KeyR", { ctrlKey: true, altKey: true, shiftKey: true }))).toBe("Ctrl+Alt+Shift+R");
    expect(chordFromEvent(ev("F5", "F5", { metaKey: true }))).toBe("Win+F5");
    expect(chordFromEvent(ev("!", "Digit1", { ctrlKey: true, shiftKey: true }))).toBe("Ctrl+Shift+1");
  });
  it("returns null while only modifiers are held", () => {
    expect(chordFromEvent(ev("Control", "ControlLeft", { ctrlKey: true }))).toBeNull();
  });
});
