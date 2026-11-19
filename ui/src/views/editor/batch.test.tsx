// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { __setInvoke, fromWireProject } from "../../backend";
import type { WireProject } from "../../contract";
import { useEditor } from "../../store/editor";
import { ExportSheet } from "./ExportSheet";
import { click, flush, render, type Rendered } from "../../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../../__fixtures__/project.v1.json"), "utf8")) as WireProject;
const BS = String.fromCharCode(92);
const DIR = `C:${BS}Users${BS}me${BS}Videos${BS}Frostcap`;

let r: Rendered;
let calls: { cmd: string; args: Record<string, unknown> }[] = [];

async function open(formats: string[]) {
  calls = [];
  __setInvoke(async (cmd, args = {}) => {
    calls.push({ cmd, args });
    if (cmd === "encoder_capabilities") return { h264: true, vp9: false, av1: formats.includes("av1"), formats } as never;
    if (cmd === "default_output_dir") return DIR as never;
    if (cmd === "save_project") return null as never;
    if (cmd === "export_batch") {
      const jobs = args.jobs as { outPath: string; preset: unknown }[];
      return {
        jobId: args.jobId,
        results: jobs.map((j, index) => (index === 1 ? { index, ok: false, outPath: j.outPath, error: { code: "internal", message: "no AV1 encoder" } } : { index, ok: true, outPath: j.outPath, bytes: 4_812_345, output: j.preset })),
      } as never;
    }
    return null as never;
  });
  const p = fromWireProject(structuredClone(wire), `C:${BS}p.frostcap`);
  useEditor.setState({ path: `C:${BS}p.frostcap`, project: p, savedProject: p, dirty: false });
  r = await render(<ExportSheet aspect="16:9" onClose={() => undefined} />);
  await flush(10);
}
afterEach(() => {
  r.unmount();
  __setInvoke(null);
});

const formatLabels = () => [...r.container.querySelectorAll("[aria-label=Format] [role=radio]")].map((e) => e.textContent);

describe("AV1 option", () => {
  it("is hidden unless encoder_capabilities lists it", async () => {
    await open(["mp4", "gif"]);
    expect(formatLabels()).toEqual(["MP4 (H.264)", "GIF"]);
  });
  it("is offered when listed", async () => {
    await open(["mp4", "gif", "av1"]);
    expect(formatLabels()).toEqual(["MP4 (H.264)", "GIF", "AV1 (MP4)"]);
  });
});

describe("batch export", () => {
  it("sends one job per aspect x format inside the default output folder and shows each result", async () => {
    await open(["mp4", "gif", "av1"]);
    await click(r.container.querySelector('[role=switch][aria-label="Export several versions at once"]'));
    expect(r.container.querySelector('[data-testid="batch-panel"]')).not.toBeNull();
    const box = (label: string) => [...r.container.querySelectorAll<HTMLElement>('[data-testid="batch-panel"] [role=checkbox]')].find((i) => i.textContent?.includes(label))!;
    await click(box("AV1"));
    await click(box("1:1"));
    // 16:9, 9:16, 1:1 x mp4, av1 = 6 jobs
    await click(r.container.querySelector('[data-testid="batch-start"]'));
    await flush(20);
    const call = calls.find((c) => c.cmd === "export_batch")!;
    const jobs = call.args.jobs as { outPath: string; preset: { aspect: string; format: string } }[];
    expect(jobs).toHaveLength(6);
    expect(new Set(jobs.map((j) => j.outPath)).size).toBe(6);
    expect(jobs.every((j) => j.outPath.startsWith(DIR + BS) && j.outPath.endsWith(".mp4"))).toBe(true);
    expect(jobs.map((j) => j.preset.format)).toContain("av1");
    expect(call.args.projectPath).toBe(`C:${BS}p.frostcap`);
    const rows = [...r.container.querySelectorAll('[data-testid="batch-job"]')];
    expect(rows).toHaveLength(6);
    expect(rows[0].textContent).toContain("Done · 4.8 MB");
    expect(rows[1].textContent).toContain("Failed: no AV1 encoder");
    expect(r.container.textContent).toContain("5 of 6 exported");
  });

  it("refuses more than 8 files and asks for at least one choice", async () => {
    await open(["mp4", "gif", "av1"]);
    await click(r.container.querySelector('[role=switch][aria-label="Export several versions at once"]'));
    const boxes = [...r.container.querySelectorAll<HTMLElement>('[data-testid="batch-panel"] [role=checkbox]')];
    for (const b of boxes) if (b.getAttribute("aria-checked") !== "true") await click(b); // 3 aspects x 3 formats = 9
    expect(r.container.textContent).toContain("at most 8 files");
    expect(r.container.querySelector<HTMLButtonElement>('[data-testid="batch-start"]')!.disabled).toBe(true);
  });
});

describe("batch export: review fixes", () => {
  const start = async () => {
    await click(r.container.querySelector('[role=switch][aria-label="Export several versions at once"]'));
    await click(r.container.querySelector('[data-testid="batch-start"]'));
    await flush(20);
  };
  const outPaths = () => (calls.filter((c) => c.cmd === "export_batch").at(-1)!.args.jobs as { outPath: string }[]).map((j) => j.outPath);

  it("a second run never reuses a file name written earlier in the session", async () => {
    await open(["mp4", "gif"]);
    await start();
    const first = outPaths();
    r.unmount();
    await open(["mp4", "gif"]);
    await start();
    const second = outPaths();
    // job 0 succeeded the first time (job 1 always fails in this mock, so it never wrote a file): its name must not be reused
    expect(second).not.toContain(first[0]);
    expect(/-\d+\.mp4$/.test(second[0])).toBe(true);
  });

  it("does not stay stuck on running when the progress listener cannot be set up", async () => {
    const { backend } = await import("../../backend");
    const spy = vi.spyOn(backend, "onExportProgress").mockRejectedValue(new Error("no events"));
    await open(["mp4", "gif"]);
    await start();
    spy.mockRestore();
    expect(r.container.textContent).not.toContain("Cancel export");
    expect(r.container.querySelectorAll('[data-testid="batch-job"]').length).toBeGreaterThan(0);
    expect(r.container.textContent).toContain("Failed:");
  });

  it("retries only the failed jobs and offers Show in folder for the finished ones", async () => {
    await open(["mp4", "gif"]);
    await start();
    expect(r.container.querySelector('[data-testid="batch-start"]')).toBeNull();
    const firstJobs = calls.find((c) => c.cmd === "export_batch")!.args.jobs as { outPath: string }[];
    await click(r.container.querySelector('[data-testid="batch-retry"]'));
    await flush(20);
    const batches = calls.filter((c) => c.cmd === "export_batch");
    expect(batches).toHaveLength(2);
    expect((batches[1].args.jobs as { outPath: string }[]).map((j) => j.outPath)).toEqual([firstJobs[1].outPath]);
    await click(r.container.querySelector('[data-testid="batch-reveal"]'));
    expect(calls.at(-1)).toMatchObject({ cmd: "reveal_in_folder", args: { path: firstJobs[0].outPath } });
  });
});

describe("export sheet: batch mode and AV1 hint", () => {
  it("explains why AV1 is missing when the encoder lacks it, and not otherwise", async () => {
    await open(["mp4", "gif"]);
    expect(r.container.querySelector('[data-testid="av1-hint"]')?.textContent).toContain("AV1 needs a hardware AV1 encoder or the AV1 Video Extension");
    r.unmount();
    await open(["mp4", "gif", "av1"]);
    expect(r.container.querySelector('[data-testid="av1-hint"]')).toBeNull();
  });

  it("batch mode disables the single aspect cards and format control, and offers Close instead of Cancel", async () => {
    await open(["mp4", "gif"]);
    await click(r.container.querySelector('[role=switch][aria-label="Export several versions at once"]'));
    const cards = [...r.container.querySelectorAll<HTMLButtonElement>(".aspects [role=radio]")];
    expect(cards.every((c) => c.disabled)).toBe(true);
    const fmt = [...r.container.querySelectorAll<HTMLButtonElement>("[aria-label=Format] [role=radio]")];
    expect(fmt.every((c) => c.disabled)).toBe(true);
    const foot = [...r.container.querySelectorAll(".sheet__foot")].at(-1)!.textContent;
    expect(foot).toContain("Close");
    expect(foot).not.toContain("Cancel");
  });
});
