import { describe, expect, it } from "vitest";
import { slideStyle } from "./indicator";

describe("slideStyle (CSS sliding selection indicator, replaces motion layoutId)", () => {
  it("sizes to one equal cell and translates by whole cells horizontally", () => {
    expect(slideStyle(0, 3)).toEqual({ width: "calc((100% - 6px - 4px) / 3)", transform: "translateX(calc(0 * (100% + 2px)))" });
    expect(slideStyle(2, 3).transform).toBe("translateX(calc(2 * (100% + 2px)))");
  });
  it("supports a vertical stack with a fixed step", () => {
    expect(slideStyle(2, 3, { axis: "y", stepPx: 42 })).toEqual({ transform: "translateY(84px)" });
  });
  it("hides when nothing is selected", () => {
    expect(slideStyle(-1, 3)).toEqual({ opacity: 0 });
  });
});
