import { beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { WireProject } from "../contract";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../__fixtures__/project.v1.json"), "utf8")) as WireProject;

const loadProject = vi.fn();
const saveProject = vi.fn();
vi.mock("../backend", async (orig) => {
  const real = await orig<typeof import("../backend")>();
  return { ...real, backend: { ...real.backend, loadProject: (p: string) => loadProject(p), saveProject: (p: string, x: unknown) => saveProject(p, x) } };
});

const { useEditor } = await import("./editor");
const { fromWireProject } = await import("../backend");

function fresh(path = "A") {
  const p = fromWireProject(structuredClone(wire), path);
  useEditor.setState({ path, project: p, error: null, loading: false, past: [], future: [], dirty: false, savedProject: p, selection: null, failedPath: null });
  return p;
}

beforeEach(() => {
  loadProject.mockReset();
  saveProject.mockReset();
});

describe("load", () => {
  it("keeps the current path and project when loading another project fails", async () => {
    const a = fresh("A");
    loadProject.mockRejectedValueOnce({ code: "invalid_project", message: "bad json" });
    await useEditor.getState().load("B");
    const st = useEditor.getState();
    expect(st.path).toBe("A");
    expect(st.project).toBe(a);
    expect(st.error).toMatch(/bad json/);
    expect(st.failedPath).toBe("B");
  });

  it("sets path only after a successful load and clears the error", async () => {
    fresh("A");
    loadProject.mockResolvedValueOnce(fromWireProject(structuredClone(wire), "B"));
    await useEditor.getState().load("B");
    const st = useEditor.getState();
    expect(st.path).toBe("B");
    expect(st.error).toBeNull();
    expect(st.failedPath).toBeNull();
    expect(st.dirty).toBe(false);
  });

  it("retry loads the failed path again", async () => {
    fresh("A");
    loadProject.mockRejectedValueOnce("boom");
    await useEditor.getState().load("B");
    loadProject.mockResolvedValueOnce(fromWireProject(structuredClone(wire), "B"));
    await useEditor.getState().retry();
    expect(useEditor.getState().path).toBe("B");
    expect(loadProject).toHaveBeenCalledTimes(2);
  });
});

describe("undo toasts", () => {
  it("deleting a zoom segment offers Undo that restores it", async () => {
    const { useApp } = await import("./app");
    useApp.setState({ toasts: [] });
    const p = fresh();
    const z = p.zoomSegments[0];
    useEditor.getState().select({ kind: "zoom", id: z.id });
    useEditor.getState().deleteSelection();
    expect(useEditor.getState().project!.zoomSegments).toHaveLength(p.zoomSegments.length - 1);
    const t = useApp.getState().toasts.at(-1)!;
    expect(t.text).toMatch(/deleted/i);
    expect(t.action?.label).toBe("Undo");
    t.action!.run();
    expect(useEditor.getState().project!.zoomSegments.map((s) => s.id)).toContain(z.id);
  });

  it("notifyUndoable does nothing if the project changed since", async () => {
    const { useApp } = await import("./app");
    useApp.setState({ toasts: [] });
    fresh();
    useEditor.getState().setStyle("cursor", { size: 2 }, "a");
    useEditor.getState().notifyUndoable("Applied");
    useEditor.getState().setStyle("cursor", { size: 3 }, "b");
    useApp.getState().toasts.at(-1)!.action!.run();
    expect(useEditor.getState().project!.style.cursor.size).toBe(3);
  });
});

describe("dirty tracks the saved snapshot", () => {
  it("undo back to the saved state clears dirty; redo sets it again", () => {
    fresh();
    useEditor.getState().setStyle("cursor", { size: 2 });
    expect(useEditor.getState().dirty).toBe(true);
    useEditor.getState().undo();
    expect(useEditor.getState().dirty).toBe(false);
    useEditor.getState().redo();
    expect(useEditor.getState().dirty).toBe(true);
  });

  it("saving makes the current project the saved snapshot", async () => {
    fresh();
    saveProject.mockResolvedValueOnce(undefined);
    useEditor.getState().setStyle("cursor", { size: 2 });
    await useEditor.getState().save();
    expect(useEditor.getState().dirty).toBe(false);
    useEditor.getState().undo();
    expect(useEditor.getState().dirty).toBe(true);
  });

  it("a failed save keeps dirty and rethrows", async () => {
    fresh();
    saveProject.mockRejectedValueOnce({ code: "disk_full", message: "full" });
    useEditor.getState().setStyle("cursor", { size: 2 });
    await expect(useEditor.getState().save()).rejects.toBeTruthy();
    expect(useEditor.getState().dirty).toBe(true);
  });
});
