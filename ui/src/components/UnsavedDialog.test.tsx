// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { useGuard } from "../store/guard";
import { useEditor } from "../store/editor";
import { UnsavedDialog } from "./UnsavedDialog";
import { byText, click, flush, key, render, type Rendered } from "../test/render";

let r: Rendered | null = null;
afterEach(() => {
  r?.unmount();
  r = null;
  useGuard.setState({ pending: null });
});

describe("UnsavedDialog", () => {
  it("is absent without a pending action", async () => {
    r = await render(<UnsavedDialog />);
    expect(r.container.querySelector("[role=alertdialog]")).toBeNull();
  });

  it("offers Save / Discard / Cancel, focuses Save, and Esc cancels", async () => {
    useEditor.setState({ project: { name: "Demo" } as never });
    const resolve = vi.fn(async () => undefined);
    useGuard.setState({ pending: { kind: "open", path: "B" }, resolve });
    r = await render(<UnsavedDialog />);
    await flush(10);
    const dlg = r.container.querySelector("[role=alertdialog]")!;
    expect(dlg.textContent).toMatch(/unsaved changes/i);
    expect(dlg.textContent).toContain("Demo");
    expect(document.activeElement?.textContent).toMatch(/^Save/);
    await click(byText(dlg, "button", /^Discard/));
    expect(resolve).toHaveBeenLastCalledWith("discard");
    await key(dlg, "Escape");
    expect(resolve).toHaveBeenLastCalledWith("cancel");
    await click(byText(dlg, "button", /^Save/));
    expect(resolve).toHaveBeenLastCalledWith("save");
  });
});
