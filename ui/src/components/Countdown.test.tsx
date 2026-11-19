// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { useRecording } from "../store/recording";
import { Countdown } from "./RecorderBar";
import { key, render, type Rendered } from "../test/render";

let r: Rendered | null = null;
afterEach(() => {
  r?.unmount();
  r = null;
  useRecording.setState({ phase: "idle", countdown: 0 });
});

describe("Countdown", () => {
  it("keeps one live region mounted so screen readers hear every number", async () => {
    r = await render(<Countdown />);
    const live = () => document.querySelector("#countdown-live");
    expect(live()).not.toBeNull();
    expect(live()!.getAttribute("aria-live")).toBe("assertive");
    await act(async () => useRecording.setState({ phase: "countdown", countdown: 3 }));
    expect(live()!.textContent).toMatch(/3/);
    await act(async () => useRecording.setState({ countdown: 2 }));
    expect(live()!.textContent).toMatch(/2/);
  });

  it("traps Tab on the Cancel button and Escape cancels", async () => {
    useRecording.setState({ phase: "countdown", countdown: 3 });
    r = await render(<Countdown />);
    const cancel = [...document.querySelectorAll("button")].find((b) => /Cancel/.test(b.textContent ?? ""))!;
    expect(document.activeElement).toBe(cancel);
    await key(cancel, "Tab");
    expect(document.activeElement).toBe(cancel);
    await key(cancel, "Tab", { shiftKey: true });
    expect(document.activeElement).toBe(cancel);
    await key(cancel, "Escape");
    expect(useRecording.getState().phase).toBe("idle");
  });
});
