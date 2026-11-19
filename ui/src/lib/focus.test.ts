import { describe, expect, it } from "vitest";
import { trapTab } from "./focus";

describe("trapTab (dialog focus trap)", () => {
  it("wraps from last to first and first to last", () => {
    expect(trapTab(4, 3, false)).toBe(0);
    expect(trapTab(4, 0, true)).toBe(3);
  });
  it("lets the browser move focus inside the dialog", () => {
    expect(trapTab(4, 1, false)).toBeNull();
    expect(trapTab(4, 2, true)).toBeNull();
  });
  it("pulls focus back in when it escaped (or sits on a now-disabled control)", () => {
    expect(trapTab(4, -1, false)).toBe(0);
    expect(trapTab(4, -1, true)).toBe(3);
  });
  it("returns -1 (focus the dialog itself) when nothing is focusable", () => {
    expect(trapTab(0, -1, false)).toBe(-1);
  });
});
