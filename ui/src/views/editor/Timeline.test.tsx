// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { fromWireProject } from "../../backend";
import type { WireProject } from "../../contract";
import { useEditor } from "../../store/editor";
import { Timeline } from "./Timeline";
import { flush, render, type Rendered } from "../../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../../__fixtures__/project.v1.json"), "utf8")) as WireProject;

let r: Rendered;
beforeEach(async () => {
  const p = fromWireProject(structuredClone(wire), "x"); // clips [400,8000] [8600,10000]
  useEditor.setState({ path: "x", project: p, savedProject: p, playhead: 0, selection: null, playing: false });
  r = await render(<Timeline />);
  await act(async () => useEditor.setState({ pxPerSec: 100 })); // 100 px per second, content left = 0
  await flush();
});
afterEach(() => r.unmount());

async function pointer(el: Element, type: string, clientX: number) {
  await act(async () => {
    el.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, clientX, pointerId: 1, button: 0 }));
  });
}

describe("timeline hint", () => {
  it("is short and not a live region (no chatter on every selection)", async () => {
    const hint = r.container.querySelector(".timeline__hint")!;
    expect(hint.getAttribute("aria-live")).toBeNull();
    const zoom = r.container.querySelector(".zseg")!;
    await pointer(zoom, "pointerdown", 100);
    await pointer(zoom, "pointerup", 100);
    expect(hint.textContent!.length).toBeLessThanOrEqual(40);
  });
});

describe("timeline clips", () => {
  it("has no interactive controls nested inside clip buttons", () => {
    for (const clip of r.container.querySelectorAll("[role=button]")) {
      expect(clip.querySelector("[role=slider], [role=button], button, [tabindex='0']")).toBeNull();
    }
    // trim handles still exist as sliders
    expect(r.container.querySelectorAll('.track [role=slider][aria-label^="Trim"]').length).toBe(4);
  });

  it("clicking a clip body selects it and moves the playhead there (so S splits at the click)", async () => {
    const clip = r.container.querySelector(".clip")!;
    await pointer(clip, "pointerdown", 300);
    await pointer(clip, "pointerup", 300);
    const st = useEditor.getState();
    expect(st.selection).toEqual({ kind: "clip", id: st.project!.clips[0].id });
    expect(st.playhead).toBe(3000);
    expect(st.splitAtPlayhead()).toBe(true);
    expect(useEditor.getState().project!.clips.map((c) => [c.start, c.end])).toEqual([
      [400, 3000],
      [3000, 8000],
      [8600, 10000],
    ]);
  });

  it("dragging across a clip scrubs once past a small threshold", async () => {
    const clip = r.container.querySelector(".clip")!;
    const content = r.container.querySelector(".timeline__content")!;
    await pointer(clip, "pointerdown", 300);
    await pointer(content, "pointermove", 302); // below threshold: no scrub yet
    expect(useEditor.getState().playhead).toBe(0);
    await pointer(content, "pointermove", 450);
    expect(useEditor.getState().playhead).toBeCloseTo(4500, 0);
    await pointer(content, "pointerup", 450);
  });
});
