import { describe, expect, it } from "vitest";
import { runSteps, type Step } from "./runner";

const clock = () => {
  let t = 1000;
  return () => (t += 10);
};

describe("runSteps", () => {
  it("runs steps in order, shares the context and reports each result with timing", async () => {
    const order: string[] = [];
    const steps: Step<{ n: number }>[] = [
      { name: "a", run: async (c) => { order.push("a"); c.n = 1; return { saw: c.n }; } },
      { name: "b", run: async (c) => { order.push("b"); return { saw: c.n }; } },
    ];
    const rep = await runSteps(steps, { n: 0 }, { now: clock() });
    expect(order).toEqual(["a", "b"]);
    expect(rep.ok).toBe(true);
    expect(rep.steps.map((s) => [s.name, s.ok, s.detail])).toEqual([["a", true, { saw: 1 }], ["b", true, { saw: 1 }]]);
    expect(rep.steps[0].ms).toBeGreaterThan(0);
  });

  it("stops at the first failure, marks the rest skipped, but still runs cleanup steps", async () => {
    const ran: string[] = [];
    const steps: Step<object>[] = [
      { name: "one", run: async () => { ran.push("one"); } },
      { name: "boom", run: async () => { throw new Error("no loopback"); } },
      { name: "never", run: async () => { ran.push("never"); } },
      { name: "cleanup", cleanup: true, run: async () => { ran.push("cleanup"); } },
    ];
    const rep = await runSteps(steps, {}, { now: clock() });
    expect(ran).toEqual(["one", "cleanup"]);
    expect(rep.ok).toBe(false);
    expect(rep.steps.map((s) => [s.name, s.ok, !!s.skipped])).toEqual([["one", true, false], ["boom", false, false], ["never", false, true], ["cleanup", true, false]]);
    expect(rep.steps[1].error).toBe("no loopback");
  });

  it("serializes non-Error rejections like backend {code,message} objects", async () => {
    const rep = await runSteps([{ name: "x", run: async () => { throw { code: "io", message: "disk" }; } }], {}, { now: clock() });
    expect(rep.steps[0].error).toBe("io: disk");
  });

  it("a failing cleanup fails the report but does not hide earlier results", async () => {
    const rep = await runSteps<object>([{ name: "ok", run: async () => {} }, { name: "c", cleanup: true, run: async () => { throw new Error("locked"); } }], {}, { now: clock() });
    expect(rep.ok).toBe(false);
    expect(rep.steps[0].ok).toBe(true);
  });

  it("reports progress as each step finishes", async () => {
    const seen: string[] = [];
    await runSteps([{ name: "p", run: async () => {} }], {}, { now: clock(), onStep: (s) => seen.push(s.name) });
    expect(seen).toEqual(["p"]);
  });
});
