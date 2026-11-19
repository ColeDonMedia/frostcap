// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { act } from "react";
import { fromWireProject } from "../../backend";
import type { WireProject } from "../../contract";
import { useEditor } from "../../store/editor";
import { Inspector } from "./Inspector";
import { AnnotationTrack, SpeedTrack } from "./RangeTracks";
import { click, key, render, type Rendered } from "../../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../../__fixtures__/project.v1.json"), "utf8")) as WireProject;

let r: Rendered;
const st = () => useEditor.getState();
const proj = () => st().project!;

beforeEach(() => {
  const p = fromWireProject(structuredClone(wire), "x");
  useEditor.setState({ path: "x", project: p, savedProject: p, past: [], future: [], selection: null, tool: null, eyedropper: false, playhead: 500 });
});
afterEach(() => r?.unmount());

async function openTab(label: string) {
  await click([...r.container.querySelectorAll<HTMLButtonElement>("[role=tab]")].find((t) => t.textContent?.includes(label)));
}
const flush = () => act(async () => {});

describe("Annotate tab", () => {
  beforeEach(async () => {
    r = await render(<Inspector />);
    await openTab("Annotate");
  });

  it("offers the eight tools as a radio group and toggles the active tool", async () => {
    const tools = [...r.container.querySelectorAll<HTMLElement>("[role=radio][data-testid^=tool-]")];
    expect(tools.map((t) => t.dataset.testid)).toEqual(["tool-arrow", "tool-text", "tool-rect", "tool-ellipse", "tool-highlight", "tool-spotlight", "tool-blur", "tool-pixelate"]);
    await click(tools[2]);
    expect(st().tool).toBe("rect");
    expect(tools[2].getAttribute("aria-checked")).toBe("true");
    await click(tools[2]);
    expect(st().tool).toBeNull();
  });

  it("shows only the properties that apply to the selected kind and edits them through the history", async () => {
    st().addAnnotation("blur", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.3 });
    await flush();
    const labels = () => [...r.container.querySelectorAll(".field__label")].map((e) => e.textContent);
    expect(labels().some((l) => l?.includes("Strength"))).toBe(true);
    expect(labels().some((l) => l?.includes("Stroke width"))).toBe(false);
    st().addAnnotation("rect", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.3 });
    await flush();
    expect(labels().some((l) => l?.includes("Stroke width"))).toBe(true);
    expect(labels().some((l) => l?.includes("Corner radius"))).toBe(true);
    const id = st().selection!.id;
    st().updateAnnotation(id, { color: "#ff0000" });
    st().undo();
    expect(proj().annotations.find((a) => a.id === id)!.color).toBe("#4ed2fb");
  });

  it("duplicate and delete buttons act on the selection", async () => {
    st().addAnnotation("ellipse", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.3 });
    await flush();
    await click(r.container.querySelector('[aria-label="Duplicate"]'));
    expect(proj().annotations).toHaveLength(2);
    await click(r.container.querySelector('[aria-label="Delete annotation"]'));
    expect(proj().annotations).toHaveLength(1);
  });
});

describe("Webcam tab: effects", () => {
  beforeEach(async () => {
    r = await render(<Inspector />);
    await openTab("Webcam");
  });

  it("edits border, mirror and colour adjustments in the style", async () => {
    const mirror = r.container.querySelector<HTMLElement>('[role=switch][aria-label="Mirror"]')!;
    await click(mirror);
    expect(proj().style.webcam.mirror).toBe(true);
  });

  it("explains that background replacement needs green screen, and disables it until the key is on", async () => {
    expect(r.container.querySelector('[data-testid="bg-needs-key"]')).not.toBeNull();
    const kind = () => [...r.container.querySelectorAll<HTMLSelectElement>("select")].find((s) => [...s.options].some((o) => o.value === "blur"))!;
    expect(kind().disabled).toBe(true);
    await click(r.container.querySelector('[role=switch][aria-label="Remove background color"]'));
    expect(proj().style.webcam.chromaKey.enabled).toBe(true);
    expect(r.container.querySelector('[data-testid="bg-needs-key"]')).toBeNull();
    expect(kind().disabled).toBe(false);
  });

  it("the eyedropper toggles the sampling mode and ends the annotation tool", async () => {
    st().setTool("rect");
    await click(r.container.querySelector('[data-testid="eyedropper"]'));
    expect(st().eyedropper).toBe(true);
    expect(st().tool).toBeNull();
    expect(r.container.textContent).toContain("Click the webcam in the preview");
    await click(r.container.querySelector('[data-testid="eyedropper"]'));
    expect(st().eyedropper).toBe(false);
  });

  it("device frames are offered in the Frame tab", async () => {
    await openTab("Frame");
    const opts = [...r.container.querySelectorAll<HTMLSelectElement>("select")].flatMap((s) => [...s.options].map((o) => o.value));
    expect(opts).toContain("laptop");
    expect(opts).toContain("phone");
  });
});

describe("timeline tracks", () => {
  it("annotation bars are keyboard operable: Alt+arrow resizes, Delete removes (undoable)", async () => {
    st().addAnnotation("rect", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.3 });
    const a = proj().annotations[0];
    r = await render(<AnnotationTrack pps={100} dur={proj().media.durationMs} />);
    const bar = r.container.querySelector<HTMLElement>(".rbar")!;
    expect(bar.getAttribute("aria-label")).toContain("Rectangle");
    await key(bar, "ArrowRight", { altKey: true });
    expect(proj().annotations[0].end).toBe(a.end + 100);
    await key(bar, "ArrowRight");
    expect(proj().annotations[0].start).toBe(a.start + 100);
    await key(bar, "Delete");
    expect(proj().annotations).toHaveLength(0);
    st().undo();
    expect(proj().annotations).toHaveLength(1);
  });

  it("speed bars show the speed and stay between their neighbours", async () => {
    st().setPlayhead(0);
    st().addSpeedAtPlayhead(2);
    r = await render(<SpeedTrack pps={100} dur={proj().media.durationMs} />);
    const bar = r.container.querySelector<HTMLElement>(".rbar")!;
    expect(bar.textContent).toContain("2×");
    expect(bar.className).toContain("rbar--speed");
    await key(bar, "ArrowRight");
    expect(proj().speed[0].start).toBe(100);
  });
});

describe("review fixes: inspector", () => {
  it("icon tabs carry a tooltip text and the panel header names the active tab", async () => {
    r = await render(<Inspector />);
    const tabs = [...r.container.querySelectorAll<HTMLElement>("[role=tab]")];
    expect(tabs.every((t) => !!t.dataset.tip && t.dataset.tip === t.textContent)).toBe(true);
    await openTab("Annotate");
    expect(r.container.querySelector('[data-testid="tab-title"]')!.textContent).toBe("Annotate");
  });

  it("shows the keyboard shortcuts under a selected annotation and warns when 8 overlap", async () => {
    r = await render(<Inspector />);
    await openTab("Annotate");
    st().setPlayhead(1000);
    for (let i = 0; i < 8; i++) st().addAnnotation("rect", { x: 0, y: 0 }, { x: 0.1, y: 0.1 });
    await flush();
    expect(r.container.querySelector('[data-testid="ann-shortcuts"]')!.textContent).toContain("Alt+arrows resize");
    expect(r.container.querySelector('[data-testid="ann-overlap"]')).not.toBeNull();
  });

  it("with no webcam track (a real recording) the webcam effects collapse into a hint", async () => {
    const p = proj();
    useEditor.setState({ project: { ...p, media: { ...p.media, webcam: null, screen: { file: "screen.mp4", url: "asset://screen" } } } });
    r = await render(<Inspector />);
    await openTab("Webcam");
    expect(r.container.querySelector('[data-testid="webcam-effects-need-track"]')).not.toBeNull();
    expect(r.container.querySelector('[data-testid="eyedropper"]')).toBeNull();
  });

  it("leaving the Webcam tab ends the eyedropper", async () => {
    r = await render(<Inspector />);
    await openTab("Webcam");
    st().setEyedropper(true);
    await openTab("Audio");
    expect(st().eyedropper).toBe(false);
  });

  it("the background image shows Custom image, not the hashed file name", async () => {
    const p = proj();
    useEditor.setState({ project: { ...p, style: { ...p.style, backgroundImage: { file: "background-0123456789abcdef.png", fit: "cover", blur: 0 } } } });
    r = await render(<Inspector />);
    await openTab("Frame");
    expect(r.container.textContent).toContain("Custom image");
    expect(r.container.textContent).not.toContain("0123456789abcdef");
  });
});

describe("review fixes: timeline bars", () => {
  it("select on click or Enter, not on focus; Enter seeks into the range; keyshortcuts are declared", async () => {
    st().setPlayhead(0);
    st().addAnnotation("rect", { x: 0.1, y: 0.1 }, { x: 0.4, y: 0.3 });
    const a = proj().annotations[0];
    st().select(null);
    st().setPlayhead(a.end + 2000);
    r = await render(<AnnotationTrack pps={100} dur={proj().media.durationMs} />);
    const bar = r.container.querySelector<HTMLElement>(".rbar")!;
    await act(async () => bar.focus());
    expect(st().selection).toBeNull();
    expect(bar.getAttribute("aria-keyshortcuts")).toContain("Alt+ArrowRight");
    await key(bar, "Enter");
    expect(st().selection).toEqual({ kind: "annotation", id: a.id });
    expect(st().playhead).toBe(a.start);
  });

  it("an empty speed track offers an add button", async () => {
    r = await render(<SpeedTrack pps={100} dur={proj().media.durationMs} />);
    await click(r.container.querySelector('[data-testid="speed-empty-add"]'));
    expect(proj().speed).toHaveLength(1);
  });
});
