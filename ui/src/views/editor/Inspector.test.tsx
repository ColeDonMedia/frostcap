// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { fromWireProject } from "../../backend";
import type { WireProject } from "../../contract";
import { useEditor } from "../../store/editor";
import { Inspector } from "./Inspector";
import { click, render, type Rendered } from "../../test/render";

const here = dirname(fileURLToPath(import.meta.url));
const wire = JSON.parse(readFileSync(join(here, "../../__fixtures__/project.v1.json"), "utf8")) as WireProject;

let r: Rendered;
beforeEach(async () => {
  const p = fromWireProject(structuredClone(wire), "x");
  useEditor.setState({ path: "x", project: p, savedProject: p });
  r = await render(<Inspector />);
});
afterEach(() => r.unmount());

async function openTab(label: string) {
  await click([...r.container.querySelectorAll<HTMLButtonElement>("[role=tab]")].find((t) => t.textContent?.includes(label)));
}

describe("Audio tab", () => {
  it("mute maps the track gain to 0 and unmute restores the previous gain", async () => {
    useEditor.getState().setStyle("audio", { systemGain: 1.5 });
    await openTab("Audio");
    const mute = r.container.querySelector<HTMLButtonElement>('[role=switch][aria-label="Mute system audio"]')!;
    expect(mute.getAttribute("aria-checked")).toBe("false");
    await click(mute);
    expect(useEditor.getState().project!.style.audio.systemGain).toBe(0);
    expect(mute.getAttribute("aria-checked")).toBe("true");
    await click(mute);
    expect(useEditor.getState().project!.style.audio.systemGain).toBe(1.5);
  });

  it("gain sliders go up to the backend limit of 400 %", async () => {
    await openTab("Audio");
    const slider = [...r.container.querySelectorAll<HTMLInputElement>("input[type=range]")].find((s) => s.closest(".field")?.textContent?.includes("Microphone"))!;
    expect(slider.max).toBe("4");
  });
});

describe("Cursor tab", () => {
  it("has a smoothing switch; the amount is kept (and disabled) while it is off", async () => {
    await openTab("Cursor");
    const sw = r.container.querySelector<HTMLButtonElement>('[role=switch][aria-label="Smooth cursor motion"]')!;
    expect(sw.getAttribute("aria-checked")).toBe("true");
    await click(sw);
    expect(useEditor.getState().project!.style.cursor.smooth).toBe(false);
    const amount = [...r.container.querySelectorAll<HTMLInputElement>("input[type=range]")].find((s) => s.closest(".field")?.textContent?.includes("Smoothing"))!;
    expect(amount.disabled).toBe(true);
  });
});

describe("Inspector tabs", () => {
  it("every aria-controls points at an element that exists", () => {
    const tabs = [...r.container.querySelectorAll("[role=tab]")];
    expect(tabs.length).toBe(7);
    for (const t of tabs) {
      const id = t.getAttribute("aria-controls");
      if (id) expect(document.getElementById(id), id).not.toBeNull();
    }
  });
});
