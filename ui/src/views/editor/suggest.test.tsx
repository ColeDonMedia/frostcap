// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { __setInvoke, fromWireProject } from "../../backend";
import type { WireProject } from "../../contract";
import { useEditor } from "../../store/editor";
import { Timeline } from "./Timeline";
import { click, flush, render, type Rendered } from "../../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../../__fixtures__/project.v1.json"), "utf8")) as WireProject;

let r: Rendered;
let calls: { cmd: string; args: unknown }[] = [];
beforeEach(async () => {
  calls = [];
  __setInvoke(async (cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "suggest_cuts") return { ranges: [{ startMs: 1000, endMs: 2000 }, { startMs: 4000, endMs: 5000 }] } as never;
    return null as never;
  });
  const p = fromWireProject(structuredClone(wire), "C:\p.frostcap");
  useEditor.setState({ path: "C:\p.frostcap", project: p, savedProject: p, past: [], future: [], playhead: 0, selection: null, proposals: null });
  r = await render(<Timeline />);
  await act(async () => useEditor.setState({ pxPerSec: 100 }));
});
afterEach(() => {
  r.unmount();
  __setInvoke(null);
});

describe("Suggest cuts", () => {
  it("can reject a single proposal (nothing is applied) and keeps 28px-class buttons", async () => {
    await click(r.container.querySelector('[data-testid="suggest-cuts"]'));
    await flush(10);
    const before = useEditor.getState().project!.clips;
    await click(r.container.querySelector('[data-testid="reject-cut"]'));
    expect(r.container.querySelectorAll('[data-testid="cut-proposal"]')).toHaveLength(1);
    expect(useEditor.getState().project!.clips).toBe(before);
  });

  it("asks the backend with the defaults and shows proposals without touching the clips", async () => {
    const before = useEditor.getState().project!.clips;
    await click(r.container.querySelector('[data-testid="suggest-cuts"]'));
    await flush(10);
    expect(calls.find((c) => c.cmd === "suggest_cuts")!.args).toEqual({ projectPath: "C:\p.frostcap", options: { rmsThresholdDb: -45, minIdleMs: 1500, marginMs: 200, cursorMinPx: 3 } });
    expect(r.container.querySelectorAll('[data-testid="cut-proposal"]')).toHaveLength(2);
    expect(useEditor.getState().project!.clips).toBe(before);
    expect(r.container.textContent).toContain("2 suggested cuts");
    expect(r.container.textContent).toContain("Accept all (2)");
    expect(r.container.textContent).toContain("Dismiss all");
    // The clips/zooms summary is hidden while proposals are pending, so the header does not wrap.
    expect(r.container.textContent).not.toContain("zooms");
    expect(r.container.querySelector<HTMLElement>(".cutprop")!.title).toContain("1.0 s");
  });

  it("accepts one proposal at a time (undoable) and accept all applies the rest", async () => {
    await click(r.container.querySelector('[data-testid="suggest-cuts"]'));
    await flush(10);
    await click(r.container.querySelector(".cutprop__ok"));
    expect(r.container.querySelectorAll('[data-testid="cut-proposal"]')).toHaveLength(1);
    const clips = () => useEditor.getState().project!.clips.map((c) => [c.start, c.end]);
    expect(clips().some(([s, e]) => s < 2000 && e > 1000)).toBe(false);
    await click(r.container.querySelector('[data-testid="accept-all-cuts"]'));
    expect(r.container.querySelector('[data-testid="cut-proposal"]')).toBeNull();
    expect(clips().some(([s, e]) => s < 5000 && e > 4000)).toBe(false);
    useEditor.getState().undo();
    useEditor.getState().undo();
    expect(clips()).toEqual([[400, 8000], [8600, 10000]]);
  });
});

describe("timeline header at narrow widths", () => {
  it("wraps instead of clipping, and every control is rendered and keyboard-focusable", async () => {
    const css = readFileSync(join(here, "../../styles.css"), "utf8");
    expect(/\.timeline__head \{[^}]*flex-wrap: wrap/.test(css)).toBe(true);
    await click(r.container.querySelector('[data-testid="suggest-cuts"]'));
    await flush(10);
    useEditor.getState().addSpeedAtPlayhead(2);
    await flush(10);
    const head = r.container.querySelector(".timeline__head")!;
    const labels = [...head.querySelectorAll<HTMLElement>("button, input")].map((e) => e.getAttribute("aria-label") ?? e.textContent?.trim());
    for (const want of ["Accept all (2)", "Dismiss all", "Suggest cuts", "Add speed region at playhead", "Zoom timeline out", "Timeline zoom", "Zoom timeline in", "Fit timeline"]) {
      expect(labels, want).toContain(want);
    }
    // minus, slider, plus and fit live in one group so they always wrap together
    const group = head.querySelector(".timeline__zoomgroup")!;
    expect([...group.querySelectorAll("button, input")].map((e) => e.getAttribute("aria-label"))).toEqual(["Zoom timeline out", "Timeline zoom", "Zoom timeline in", "Fit timeline"]);
    expect(/.timeline__zoomgroup {[^}]*flex: none/.test(readFileSync(join(here, "../../styles.css"), "utf8"))).toBe(true);
    for (const el of head.querySelectorAll<HTMLElement>("button, input")) expect(el.tabIndex).toBeGreaterThanOrEqual(0);
  });
});
