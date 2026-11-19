import { beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { WireProject } from "../contract";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../__fixtures__/project.v1.json"), "utf8")) as WireProject;

const { useEditor, outputDuration, sourceToOutput } = await import("./editor");
const { fromWireProject } = await import("../backend");

function fresh() {
  const p = fromWireProject(structuredClone(wire), "A");
  useEditor.setState({ path: "A", project: p, error: null, loading: false, past: [], future: [], dirty: false, savedProject: p, selection: null, failedPath: null, playhead: 1000, tool: null });
  return p;
}
const st = () => useEditor.getState();
const proj = () => st().project!;

beforeEach(() => {
  vi.useRealTimers();
  fresh();
});

describe("annotations in the editor store", () => {
  it("adds from a drag at the playhead, selects it and ends the tool", () => {
    st().setTool("rect");
    const id = st().addAnnotation("rect", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.3 })!;
    expect(proj().annotations).toHaveLength(1);
    expect(proj().annotations[0]).toMatchObject({ id, kind: "rect", start: 1000 });
    expect(proj().annotations[0].end).toBeGreaterThan(1000);
    expect(st().selection).toEqual({ kind: "annotation", id });
    expect(st().tool).toBeNull();
  });

  it("is undoable and redoable through the shared history", () => {
    st().addAnnotation("ellipse", { x: 0, y: 0 }, { x: 0.2, y: 0.2 });
    st().undo();
    expect(proj().annotations).toHaveLength(0);
    st().redo();
    expect(proj().annotations).toHaveLength(1);
  });

  it("updates properties and merges rapid slider edits into one undo step", () => {
    const id = st().addAnnotation("rect", { x: 0, y: 0 }, { x: 0.2, y: 0.2 })!;
    st().updateAnnotation(id, { strokeWidth: 8 });
    st().updateAnnotation(id, { strokeWidth: 9 });
    expect(proj().annotations[0].strokeWidth).toBe(9);
    st().undo();
    expect(proj().annotations[0].strokeWidth).toBe(4);
  });

  it("duplicates the selection and selects the copy", () => {
    const id = st().addAnnotation("text", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.2 })!;
    st().duplicateSelection();
    expect(proj().annotations).toHaveLength(2);
    expect(st().selection?.kind).toBe("annotation");
    expect(st().selection?.id).not.toBe(id);
  });

  it("deletes the selection (undoable)", () => {
    st().addAnnotation("blur", { x: 0, y: 0 }, { x: 0.2, y: 0.2 });
    st().deleteSelection();
    expect(proj().annotations).toHaveLength(0);
    expect(st().selection).toBeNull();
    st().undo();
    expect(proj().annotations).toHaveLength(1);
  });

  it("refuses to add past the 500 limit", () => {
    for (let i = 0; i < 500; i++) st().addAnnotation("rect", { x: 0, y: 0 }, { x: 0.1, y: 0.1 });
    expect(proj().annotations).toHaveLength(500);
    expect(st().addAnnotation("rect", { x: 0, y: 0 }, { x: 0.1, y: 0.1 })).toBeNull();
  });

  it("clears the selection when undo removes the selected annotation", () => {
    st().addAnnotation("rect", { x: 0, y: 0 }, { x: 0.2, y: 0.2 });
    st().undo();
    expect(st().selection).toBeNull();
  });
});

describe("speed segments in the editor store", () => {
  it("adds at the playhead, updates, deletes and keeps segments from overlapping", () => {
    expect(st().addSpeedAtPlayhead(2)).toBe(true);
    const seg = proj().speed[0];
    expect(seg).toMatchObject({ start: 1000, speed: 2 });
    expect(st().selection).toEqual({ kind: "speed", id: seg.id });
    st().setPlayhead(1500);
    expect(st().addSpeedAtPlayhead(3)).toBe(false); // inside the first segment
    st().updateSpeed(seg.id, { speed: 0.5 });
    expect(proj().speed[0].speed).toBe(0.5);
    st().deleteSelection();
    expect(proj().speed).toHaveLength(0);
  });

  it("changes the output duration and playhead time", () => {
    const before = outputDuration(proj());
    st().setPlayhead(0);
    st().addSpeedAtPlayhead(2);
    const seg = proj().speed[0];
    // only the kept part of the segment counts (cuts apply first)
    const kept = proj().clips.reduce((n, c) => n + Math.max(0, Math.min(c.end, seg.end) - Math.max(c.start, seg.start)), 0);
    expect(kept).toBeGreaterThan(0);
    expect(outputDuration(proj())).toBeCloseTo(before - kept / 2, 0);
    expect(sourceToOutput(proj(), seg.end)).toBeCloseTo(proj().clips.filter((c) => c.start < seg.end).reduce((n, c) => n + Math.max(0, Math.min(c.end, seg.end) - c.start), 0) - kept / 2, 0);
  });
});

describe("suggested cuts", () => {
  it("are proposals only until accepted; accepting applies one undoable trim", () => {
    const before = proj().clips.map((c) => [c.start, c.end]);
    st().setProposals([{ startMs: 2000, endMs: 3000 }, { startMs: 5000, endMs: 6000 }]);
    expect(proj().clips.map((c) => [c.start, c.end])).toEqual(before);
    st().acceptProposal(2000);
    expect(st().proposals).toEqual([{ startMs: 5000, endMs: 6000 }]);
    expect(proj().clips.some((c) => c.start < 3000 && c.end > 2000)).toBe(false);
    st().undo();
    expect(proj().clips.map((c) => [c.start, c.end])).toEqual(before);
  });

  it("accept all is one undo step and clears the proposals; dismiss changes nothing", () => {
    const before = proj().clips.map((c) => [c.start, c.end]);
    st().setProposals([{ startMs: 2000, endMs: 3000 }, { startMs: 5000, endMs: 6000 }]);
    st().acceptAllProposals();
    expect(st().proposals).toBeNull();
    st().undo();
    expect(proj().clips.map((c) => [c.start, c.end])).toEqual(before);
    st().setProposals([{ startMs: 2000, endMs: 3000 }]);
    st().dismissProposals();
    expect(st().proposals).toBeNull();
    expect(proj().clips.map((c) => [c.start, c.end])).toEqual(before);
  });
});

describe("review fixes", () => {
  it("addSpeedAtPlayhead survives a negative playhead (clamped to 0, no throw)", () => {
    useEditor.setState({ playhead: -500 });
    expect(st().addSpeedAtPlayhead(2)).toBe(true);
    expect(proj().speed[0].start).toBe(0);
  });

  it("accepting cuts that would remove the whole recording changes nothing and says so", async () => {
    const { useApp } = await import("./app");
    useApp.setState({ toasts: [] });
    const clips = proj().clips;
    const pastBefore = st().past.length;
    st().setProposals([{ startMs: 0, endMs: proj().media.durationMs }]);
    st().acceptAllProposals();
    expect(proj().clips).toBe(clips);
    expect(st().past.length).toBe(pastBefore);
    expect(useApp.getState().toasts.at(-1)?.text).toMatch(/whole recording/i);
    st().setProposals([{ startMs: 0, endMs: proj().media.durationMs }]);
    st().acceptProposal(0);
    expect(proj().clips).toBe(clips);
  });
});
