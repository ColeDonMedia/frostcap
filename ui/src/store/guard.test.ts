import { beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { WireProject } from "../contract";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../__fixtures__/project.v1.json"), "utf8")) as WireProject;

const saveProject = vi.fn();
vi.mock("../backend", async (orig) => {
  const real = await orig<typeof import("../backend")>();
  return { ...real, backend: { ...real.backend, saveProject: (p: string, x: unknown) => saveProject(p, x) } };
});

const { useEditor } = await import("./editor");
const { useApp } = await import("./app");
const { useGuard, openProjectGuarded, requestClose, beforeUnload } = await import("./guard");
const { fromWireProject } = await import("../backend");

function editorWith(dirty: boolean) {
  const p = fromWireProject(structuredClone(wire), "A");
  useEditor.setState({ path: "A", project: p, savedProject: p, dirty: false, past: [], future: [] });
  if (dirty) useEditor.getState().setStyle("cursor", { size: 3 });
}

beforeEach(() => {
  saveProject.mockReset();
  useApp.setState({ projectPath: "A", view: "editor" });
  useGuard.setState({ pending: null });
});

describe("unsaved changes guard", () => {
  it("opens directly when nothing is dirty", async () => {
    editorWith(false);
    await openProjectGuarded("B");
    expect(useApp.getState().projectPath).toBe("B");
    expect(useGuard.getState().pending).toBeNull();
  });

  it("asks first when the editor is dirty; Cancel keeps everything", async () => {
    editorWith(true);
    await openProjectGuarded("B");
    expect(useApp.getState().projectPath).toBe("A");
    expect(useGuard.getState().pending).toMatchObject({ kind: "open", path: "B" });
    await useGuard.getState().resolve("cancel");
    expect(useApp.getState().projectPath).toBe("A");
    expect(useEditor.getState().dirty).toBe(true);
    expect(useGuard.getState().pending).toBeNull();
  });

  it("Discard opens the other project without saving", async () => {
    editorWith(true);
    await openProjectGuarded("B");
    await useGuard.getState().resolve("discard");
    expect(saveProject).not.toHaveBeenCalled();
    expect(useApp.getState().projectPath).toBe("B");
  });

  it("Save saves first, then opens; a failed save stays put", async () => {
    editorWith(true);
    saveProject.mockRejectedValueOnce({ code: "disk_full", message: "full" });
    await openProjectGuarded("B");
    await useGuard.getState().resolve("save");
    expect(useApp.getState().projectPath).toBe("A");
    expect(useGuard.getState().pending).not.toBeNull();
    saveProject.mockResolvedValueOnce(undefined);
    await useGuard.getState().resolve("save");
    expect(saveProject).toHaveBeenCalledTimes(2);
    expect(useApp.getState().projectPath).toBe("B");
  });

  it("re-opening the same project never asks", async () => {
    editorWith(true);
    await openProjectGuarded("A");
    expect(useGuard.getState().pending).toBeNull();
  });

  it("window close asks when dirty and runs the close callback after a choice", async () => {
    editorWith(true);
    const close = vi.fn();
    expect(requestClose(close)).toBe(false); // prevented
    expect(useGuard.getState().pending).toMatchObject({ kind: "close" });
    await useGuard.getState().resolve("discard");
    expect(close).toHaveBeenCalledOnce();
    editorWith(false);
    expect(requestClose(close)).toBe(true); // allowed immediately
  });

  it("browser beforeunload is prevented only while dirty", () => {
    editorWith(true);
    const e = { preventDefault: vi.fn(), returnValue: undefined as unknown };
    beforeUnload(e as unknown as BeforeUnloadEvent);
    expect(e.preventDefault).toHaveBeenCalled();
    editorWith(false);
    const e2 = { preventDefault: vi.fn() };
    beforeUnload(e2 as unknown as BeforeUnloadEvent);
    expect(e2.preventDefault).not.toHaveBeenCalled();
  });
});
