// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { __setInvoke, fromWireProject } from "../../backend";
import type { WireProject } from "../../contract";
import { useEditor } from "../../store/editor";
import { ExportSheet } from "./ExportSheet";
import { byText, click, flush, key, render, type Rendered } from "../../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../../__fixtures__/project.v1.json"), "utf8")) as WireProject;

let calls: { cmd: string; args: Record<string, unknown> }[] = [];
let releaseExport: (() => void) | null = null;
let r: Rendered;

beforeEach(async () => {
  calls = [];
  __setInvoke(async (cmd, args = {}) => {
    calls.push({ cmd, args });
    if (cmd === "pick_save_path") return "C:\\Users\\me\\Videos\\Frostcap\\Fixture.mp4" as never;
    if (cmd === "save_project") return null as never;
    if (cmd === "export_project") {
      await new Promise<void>((res) => (releaseExport = res));
      return { jobId: args.jobId, outPath: args.outPath, bytes: 10, baked: true, preset: "raw" } as never;
    }
    return null as never;
  });
  const p = fromWireProject(structuredClone(wire), "C:\\p.frostcap");
  useEditor.setState({ path: "C:\\p.frostcap", project: p, savedProject: p, dirty: false });
  r = await render(<ExportSheet aspect="16:9" onClose={() => undefined} />);
});

afterEach(() => {
  releaseExport?.();
  releaseExport = null;
  r.unmount();
  __setInvoke(null);
});

const exportBtn = () => byText<HTMLButtonElement>(r.container, ".sheet__foot button", /^Export$/);

describe("ExportSheet", () => {
  it("aspect radiogroup uses roving tabindex and arrow keys", async () => {
    const radios = () => [...r.container.querySelectorAll<HTMLButtonElement>('[aria-labelledby="aspect-l"] [role=radio]')];
    expect(radios().map((b) => b.tabIndex)).toEqual([0, -1, -1]);
    await key(radios()[0], "ArrowRight");
    expect(radios().map((b) => b.getAttribute("aria-checked"))).toEqual(["false", "true", "false"]);
    expect(radios().map((b) => b.tabIndex)).toEqual([-1, 0, -1]);
    expect(document.activeElement).toBe(radios()[1]);
  });

  it("reserves the progress area before an export starts", () => {
    const area = r.container.querySelector(".export-status");
    expect(area).not.toBeNull();
    expect(area!.getAttribute("aria-hidden")).toBe("true");
  });

  it("asks for a save location, then sends the Rust preset object with a job id", async () => {
    await click(exportBtn());
    for (let i = 0; i < 50 && !calls.some((c) => c.cmd === "export_project"); i++) await flush(10);
    expect(calls.map((c) => c.cmd)).toContain("pick_save_path");
    const exp = calls.find((c) => c.cmd === "export_project")!;
    expect(exp.args).toMatchObject({
      path: "C:\\p.frostcap",
      outPath: "C:\\Users\\me\\Videos\\Frostcap\\Fixture.mp4",
      preset: { aspect: "16:9", width: 1920, height: 1080, fps: 60, format: "mp4" },
    });
    expect(typeof exp.args.jobId).toBe("string");
  });

  it("disables every setting while the export runs", async () => {
    await click(exportBtn());
    await flush(10);
    const controls = [...r.container.querySelectorAll<HTMLElement>(".sheet select, .sheet input, .sheet [role=radio]")];
    expect(controls.length).toBeGreaterThan(5);
    for (const c of controls) expect((c as HTMLButtonElement).disabled, c.outerHTML.slice(0, 80)).toBe(true);
    releaseExport?.();
    await flush(10);
    expect((r.container.querySelector(".sheet select") as HTMLSelectElement).disabled).toBe(false);
  });
});
