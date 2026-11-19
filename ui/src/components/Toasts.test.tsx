// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { useApp } from "../store/app";
import { Toasts } from "./Toasts";
import { click, key, render, type Rendered } from "../test/render";

let r: Rendered;
beforeEach(async () => {
  useApp.setState({ toasts: [] });
  r = await render(<Toasts />);
});
afterEach(() => r.unmount());

describe("Toasts", () => {
  it("renders a labelled region with a stack; errors use role=alert", async () => {
    await act(async () => {
      useApp.getState().showToast("Saved");
      useApp.getState().showToast("Disk full", "error");
    });
    const region = r.container.querySelector('[role=region][aria-label="Notifications"]')!;
    const items = [...region.querySelectorAll(".toast")];
    expect(items.map((i) => i.getAttribute("role"))).toEqual(["status", "alert"]);
  });

  it("F6 moves focus to the newest toast's action, and the action runs", async () => {
    const run = vi.fn();
    await act(async () => {
      useApp.getState().showToast("Segment deleted", "info", { label: "Undo", run });
    });
    await key(document.body, "F6");
    expect(document.activeElement?.textContent).toBe("Undo");
    await click(document.activeElement);
    expect(run).toHaveBeenCalledOnce();
    expect(useApp.getState().toasts).toHaveLength(0);
  });

  it("action toasts tell screen readers how to reach them", async () => {
    await act(async () => {
      useApp.getState().showToast("Segment deleted", "info", { label: "Undo", run: () => undefined });
    });
    expect(r.container.querySelector(".toast .sr-only")?.textContent).toMatch(/F6/);
  });

  it("hover pauses auto-dismiss; leaving resumes it; Dismiss removes it", async () => {
    vi.useFakeTimers();
    try {
      await act(async () => {
        useApp.getState().showToast("Saved");
      });
      const t = r.container.querySelector(".toast")!;
      await act(async () => {
        t.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
        vi.advanceTimersByTime(30_000);
      });
      expect(useApp.getState().toasts).toHaveLength(1);
      await act(async () => {
        t.dispatchEvent(new MouseEvent("mouseout", { bubbles: true, relatedTarget: document.body }));
        vi.advanceTimersByTime(6000);
      });
      expect(useApp.getState().toasts).toHaveLength(0);
      await act(async () => {
        useApp.getState().showToast("Again");
      });
      const again = [...r.container.querySelectorAll(".toast")].find((el) => el.textContent?.includes("Again"))!;
      await click(again.querySelector("button[aria-label=Dismiss]"));
      expect(useApp.getState().toasts).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
