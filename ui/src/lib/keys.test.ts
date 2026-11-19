import { describe, expect, it } from "vitest";
import { isTypingTarget, navShortcut } from "./keys";

const target = (tagName: string, extra: Partial<{ isContentEditable: boolean; type: string }> = {}) => ({ tagName, isContentEditable: false, ...extra }) as unknown as EventTarget;
const ev = (key: string, mods: Partial<{ ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean }>, t: EventTarget | null = target("DIV")) =>
  ({ key, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...mods, target: t }) as unknown as KeyboardEvent;

describe("isTypingTarget", () => {
  it("is true for text fields, selects and contenteditable", () => {
    expect(isTypingTarget(target("INPUT"))).toBe(true);
    expect(isTypingTarget(target("TEXTAREA"))).toBe(true);
    expect(isTypingTarget(target("SELECT"))).toBe(true);
    expect(isTypingTarget(target("DIV", { isContentEditable: true }))).toBe(true);
  });
  it("is false for buttons, divs and null", () => {
    expect(isTypingTarget(target("BUTTON"))).toBe(false);
    expect(isTypingTarget(target("DIV"))).toBe(false);
    expect(isTypingTarget(null)).toBe(false);
  });
});

describe("navShortcut (Ctrl+1/2/3)", () => {
  it("maps Ctrl+digit to views", () => {
    expect(navShortcut(ev("1", { ctrlKey: true }))).toBe("recorder");
    expect(navShortcut(ev("2", { ctrlKey: true }))).toBe("editor");
    expect(navShortcut(ev("3", { ctrlKey: true }))).toBe("settings");
  });
  it("ignores other modifiers and plain digits", () => {
    expect(navShortcut(ev("1", {}))).toBeNull();
    expect(navShortcut(ev("1", { ctrlKey: true, shiftKey: true }))).toBeNull();
    expect(navShortcut(ev("1", { ctrlKey: true, altKey: true }))).toBeNull();
    expect(navShortcut(ev("4", { ctrlKey: true }))).toBeNull();
  });
  it("does not fire while typing in a field", () => {
    expect(navShortcut(ev("2", { ctrlKey: true }, target("INPUT")))).toBeNull();
    expect(navShortcut(ev("2", { ctrlKey: true }, target("TEXTAREA")))).toBeNull();
  });
});
