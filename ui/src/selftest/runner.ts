/** Sequential step runner for the in-app self-test: pure logic, no I/O of its own. */

export interface StepResult {
  name: string;
  ok: boolean;
  skipped?: boolean;
  ms: number;
  detail?: Record<string, unknown>;
  error?: string;
}

export interface Report {
  ok: boolean;
  startedAt: string;
  finishedAt: string;
  steps: StepResult[];
  env: Record<string, string>;
}

export interface Step<C> {
  name: string;
  /** Runs even after an earlier failure (e.g. deleting the self-test project). */
  cleanup?: boolean;
  run: (ctx: C) => Promise<Record<string, unknown> | void>;
}

export interface RunOptions {
  now?: () => number;
  env?: Record<string, string>;
  onStep?: (r: StepResult) => void;
}

export function describeError(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && "message" in e) {
    const { code, message } = e as { code?: unknown; message: unknown };
    return code ? `${String(code)}: ${String(message)}` : String(message);
  }
  return String(e);
}

export async function runSteps<C>(steps: Step<C>[], ctx: C, opts: RunOptions = {}): Promise<Report> {
  const now = opts.now ?? (() => performance.now());
  const startedAt = new Date().toISOString();
  const results: StepResult[] = [];
  let failed = false;
  const push = (r: StepResult) => {
    results.push(r);
    opts.onStep?.(r);
  };
  for (const step of steps) {
    if (failed && !step.cleanup) {
      push({ name: step.name, ok: false, skipped: true, ms: 0 });
      continue;
    }
    const t0 = now();
    try {
      const detail = (await step.run(ctx)) ?? undefined;
      push({ name: step.name, ok: true, ms: now() - t0, ...(detail ? { detail } : {}) });
    } catch (e) {
      failed = true;
      push({ name: step.name, ok: false, ms: now() - t0, error: describeError(e) });
    }
  }
  return { ok: results.every((r) => r.ok), startedAt, finishedAt: new Date().toISOString(), steps: results, env: opts.env ?? {} };
}
