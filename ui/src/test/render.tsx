/**
 * Minimal DOM test helper (happy-dom + React 19 act). Use with
 * `// @vitest-environment happy-dom` at the top of the test file.
 */
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { LazyMotion, MotionConfig, domAnimation } from "motion/react";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// happy-dom >= 21 implements the Web Animations API; motion then uses it, and
// unmounting mid-animation rejects `animation.finished` with an unhandled
// AbortError. Tests don't need real animations: hide WAAPI so motion uses its
// JS fallback (reducedMotion="always" below makes it instant anyway).
for (const proto of [globalThis.Element?.prototype, globalThis.HTMLElement?.prototype]) {
  if (proto && Object.prototype.hasOwnProperty.call(proto, "animate")) delete (proto as { animate?: unknown }).animate;
}

export interface Rendered {
  container: HTMLElement;
  root: Root;
  unmount: () => void;
  rerender: (ui: ReactNode) => Promise<void>;
}

const wrap = (ui: ReactNode) => (
  <LazyMotion features={domAnimation}>
    <MotionConfig reducedMotion="always">{ui}</MotionConfig>
  </LazyMotion>
);

export async function render(ui: ReactNode): Promise<Rendered> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => root.render(wrap(ui)));
  return {
    container,
    root,
    unmount: () => {
      act(() => root.unmount());
      container.remove();
    },
    rerender: async (next) => {
      await act(async () => root.render(wrap(next)));
    },
  };
}

export async function click(el: Element | null | undefined) {
  if (!el) throw new Error("click: element not found");
  await act(async () => {
    (el as HTMLElement).click();
  });
}

export async function key(el: Element | null | undefined, k: string, init: KeyboardEventInit = {}) {
  if (!el) throw new Error("key: element not found");
  await act(async () => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...init }));
  });
}

export async function flush(ms = 0) {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

export function byText<T extends Element = HTMLElement>(root: ParentNode, selector: string, text: string | RegExp): T | undefined {
  return [...root.querySelectorAll<T>(selector)].find((e) => (typeof text === "string" ? e.textContent?.trim() === text : text.test(e.textContent ?? "")));
}
