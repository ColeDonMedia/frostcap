// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { __setInvoke } from "../../backend";
import { useApp } from "../../store/app";
import { useEditor } from "../../store/editor";
import EditorView from "./EditorView";
import { byText, click, flush, render, type Rendered } from "../../test/render";

let loads = 0;
let r: Rendered;
beforeEach(async () => {
  loads = 0;
  __setInvoke(async (cmd) => {
    if (cmd === "load_project") {
      loads++;
      throw { code: "invalid_project", message: "project.json is not valid JSON" };
    }
    return null as never;
  });
  useEditor.setState({ path: null, project: null, savedProject: null, error: null, failedPath: null, dirty: false });
  useApp.setState({ view: "editor", projectPath: "C:\\bad.frostcap" });
  r = await render(<EditorView path="C:\\bad.frostcap" />);
  await flush(30);
});
afterEach(() => {
  r.unmount();
  __setInvoke(null);
});

describe("editor load error", () => {
  it("explains the error with Retry and Back", async () => {
    const alert = r.container.querySelector("[role=alert]")!;
    expect(alert.textContent).toMatch(/not valid JSON/);
    expect(byText(alert, "button", /Retry/)).toBeTruthy();
    expect(byText(alert, "button", /Back/)).toBeTruthy();
  });

  it("Retry loads again", async () => {
    await click(byText(r.container, "[role=alert] button", /Retry/));
    await flush(30);
    expect(loads).toBe(2);
  });

  it("Back returns to the recorder when no project was open", async () => {
    await click(byText(r.container, "[role=alert] button", /Back/));
    expect(useApp.getState().view).toBe("recorder");
    expect(useApp.getState().projectPath).toBeNull();
  });
});
