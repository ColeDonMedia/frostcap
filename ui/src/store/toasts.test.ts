import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useApp } from "./app";

beforeEach(() => {
  vi.useFakeTimers();
  useApp.setState({ toasts: [] });
});
afterEach(() => vi.useRealTimers());

const texts = () => useApp.getState().toasts.map((t) => t.text);

describe("toast stack", () => {
  it("keeps a small stack, newest last, dropping the oldest non-error beyond 3", () => {
    const s = useApp.getState();
    s.showToast("a");
    s.showToast("b");
    s.showToast("c");
    s.showToast("d");
    expect(texts()).toEqual(["b", "c", "d"]);
  });

  it("info lasts at least 5 s; warnings longer; errors persist until dismissed", () => {
    const s = useApp.getState();
    s.showToast("info");
    s.showToast("warn", "warning");
    s.showToast("err", "error");
    vi.advanceTimersByTime(4900);
    expect(texts()).toEqual(["info", "warn", "err"]);
    vi.advanceTimersByTime(200);
    expect(texts()).toEqual(["warn", "err"]);
    vi.advanceTimersByTime(10_000);
    expect(texts()).toEqual(["err"]);
    vi.advanceTimersByTime(60_000);
    expect(texts()).toEqual(["err"]);
    s.dismissToast(useApp.getState().toasts[0].id);
    expect(texts()).toEqual([]);
  });

  it("pausing (hover/focus) stops the timer; resuming restarts it", () => {
    const s = useApp.getState();
    s.showToast("info");
    const id = useApp.getState().toasts[0].id;
    vi.advanceTimersByTime(3000);
    s.pauseToast(id, true);
    vi.advanceTimersByTime(20_000);
    expect(texts()).toEqual(["info"]);
    s.pauseToast(id, false);
    vi.advanceTimersByTime(5100);
    expect(texts()).toEqual([]);
  });

  it("toasts with an action stay long enough to reach by keyboard", () => {
    useApp.getState().showToast("deleted", "info", { label: "Undo", run: () => undefined });
    vi.advanceTimersByTime(9000);
    expect(texts()).toEqual(["deleted"]);
  });
});
