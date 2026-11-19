import { afterEach, describe, expect, it, vi } from "vitest";
import { __setInvoke } from "../backend";
import { maybeRunSelfTest } from "./index";
import type { Report } from "./runner";

afterEach(() => __setInvoke(null));

const report: Report = { ok: true, startedAt: "a", finishedAt: "b", steps: [], env: {} };

describe("maybeRunSelfTest", () => {
  it("does nothing when the backend has no launch_options command", async () => {
    __setInvoke(async () => {
      throw { code: "internal", message: "unknown command launch_options" };
    });
    const run = vi.fn(async () => report);
    expect(await maybeRunSelfTest(run)).toBe(false);
    expect(run).not.toHaveBeenCalled();
  });

  it("does nothing when selftest is false", async () => {
    __setInvoke((async () => ({ selftest: false })) as never);
    const run = vi.fn(async () => report);
    expect(await maybeRunSelfTest(run)).toBe(false);
    expect(run).not.toHaveBeenCalled();
  });

  it("runs, logs the report and hands it to write_selftest_report", async () => {
    const calls: [string, unknown][] = [];
    __setInvoke(async (cmd, args) => {
      calls.push([cmd, args]);
      return (cmd === "launch_options" ? { selftest: true } : null) as never;
    });
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    expect(await maybeRunSelfTest(async () => report)).toBe(true);
    expect(calls).toEqual([["launch_options", undefined], ["write_selftest_report", { report: JSON.stringify(report) }]]);
    expect(log.mock.calls.flat().join(" ")).toContain("[selftest]");
    log.mockRestore();
  });

  it("still finishes when the report cannot be written", async () => {
    __setInvoke(async (cmd) => {
      if (cmd === "launch_options") return { selftest: true } as never;
      throw { code: "internal", message: "no such command" };
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    expect(await maybeRunSelfTest(async () => report)).toBe(true);
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
    log.mockRestore();
  });

  it("reports a crash in the runner as a failed report instead of throwing", async () => {
    const written: unknown[] = [];
    __setInvoke(async (cmd, args) => {
      if (cmd === "launch_options") return { selftest: true } as never;
      written.push(args);
      return null as never;
    });
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    expect(
      await maybeRunSelfTest(async () => {
        throw new Error("audio context blocked");
      }),
    ).toBe(true);
    expect(JSON.stringify(written)).toContain("audio context blocked");
    log.mockRestore();
  });
});
