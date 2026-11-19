// Editor v2 scenes for ui/scripts/screenshots.mjs (--scenes v2): annotations, webcam effects,
// background image picker, suggest cuts, batch export. Dark theme, mock backend.
// Needs the dev-only `window.__editor` store hook installed by src/main.tsx.

export async function v2Scenes({ cdp, waitFor, click, shot, until, sleep, url, theme = "dark", lang = null, scenes = "all", suffix = "" }) {
  const want = (n) => scenes === "all" || scenes.split(",").includes(n);
  const snap = (n) => shot(n + suffix);
  const ed = (code) => cdp.eval(`(async () => { const e = window.__editor; ${code} })()`);
  const dragOnCanvas = (x1, y1, x2, y2) =>
    cdp.eval(`(() => {
      const c = document.querySelector(".preview canvas"); const r = c.getBoundingClientRect();
      const ev = (t, fx, fy) => c.dispatchEvent(new PointerEvent(t, { bubbles: true, clientX: r.left + fx * r.width, clientY: r.top + fy * r.height, pointerId: 1, button: 0 }));
      ev("pointerdown", ${x1}, ${y1}); ev("pointermove", ${x2}, ${y2}); ev("pointerup", ${x2}, ${y2});
    })()`);
  // Tabs are addressed by position so this also works in other languages.
  const TAB_INDEX = { Zoom: 0, Annotate: 1, Cursor: 2, Frame: 3, Webcam: 4, Audio: 5, Captions: 6 };
  const tab = (name) => cdp.eval(`document.querySelectorAll("[role=tab]")[${TAB_INDEX[name]}].click()`);

  await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: `localStorage.setItem("frostcap.settings.v1", ${JSON.stringify(JSON.stringify({ theme, ...(lang ? { language: lang } : {}) }))});` });
  await cdp.send("Page.navigate", { url });
  await waitFor('[data-testid="recent-0"]', 30000);
  await click('[data-testid="recent-0"]');
  await waitFor('[data-testid="editor-export"]', 20000);
  await until(() => cdp.eval("!!window.__editor"), "the dev editor hook", 20000);
  await ed("e.getState().setPlayhead(1800);");

  // 1. annotations drawn on the preview with the tools
  if (want("annotations") || want("webcam") || want("frames")) {
  await tab("Annotate");
  const drawn = [
    ["rect", 0.3, 0.28, 0.55, 0.45],
    ["arrow", 0.75, 0.2, 0.58, 0.38],
    ["text", 0.62, 0.62, 0.9, 0.72],
    ["blur", 0.34, 0.6, 0.5, 0.7],
  ];
  for (const [i, [kind, x1, y1, x2, y2]] of drawn.entries()) {
    await click(`[data-testid="tool-${kind}"]`);
    await dragOnCanvas(x1, y1, x2, y2);
    await ed(`e.getState().updateAnnotation(e.getState().selection.id, { start: 300 + ${i} * 900, end: 9000, fadeMs: 0 });`);
  }
  await ed(`const p = e.getState().project; const t = p.annotations.find((x) => x.kind === "text");
    e.getState().updateAnnotation(t.id, { text: "Click Export", color: "#ffffff" });
    e.getState().select({ kind: "annotation", id: p.annotations.find((x) => x.kind === "rect").id });`);
  await snap("annotations");
  }

  // 2. webcam effects (the demo shows the placeholder camera)
  if (want("webcam")) {
  await ed(`e.getState().setStyle("webcam", { enabled: true, borderWidth: 6, borderColor: "#4ed2fb", mirror: true, shadow: 0.6,
    chromaKey: { enabled: true, color: "#00ff00", similarity: 0.4, smoothness: 0.1, spill: 0.5 },
    background: { kind: "color", color: "#1151ef", gradient: "linear-gradient(135deg,#a1c4fd,#c2e9fb)", blur: 0.5 } }); e.getState().select(null);`);
  await tab("Webcam");
  await cdp.eval('document.querySelector(".tabpanel").scrollTop = 560');
  await snap("webcam");
  }

  // 3. background image picker (the mock backend answers pick_image and import_background)
  if (want("background")) {
  await tab("Frame");
  await cdp.eval('[...document.querySelectorAll(".tabpanel button")].find((b) => /Choose image/.test(b.textContent)).click()');
  await sleep(700);
  await cdp.eval("document.querySelector('.tabpanel').scrollTop = 9999");
  await snap("background-image");
  }

  // 4. suggest cuts: proposals on the timeline, nothing applied yet
  if (want("suggest")) {
  await click('[data-testid="suggest-cuts"]');
  await waitFor('[data-testid="cut-proposal"]', 10000);
  await snap("suggest-cuts");
  await click('[data-testid="accept-all-cuts"]');
  }

  // 5. batch export with per-job results
  if (want("batch")) {
  await click('[data-testid="editor-export"]');
  await waitFor('[data-testid="export-sheet"]', 10000);
  await cdp.eval('document.querySelector("[data-testid=export-sheet] [role=switch]").click()');
  await waitFor('[data-testid="batch-panel"]', 5000);
  await cdp.eval('[...document.querySelectorAll("[data-testid=batch-panel] input[type=checkbox]")].filter((i) => !i.checked).slice(0, 1).forEach((i) => i.click())');
  await click('[data-testid="batch-start"]');
  await waitFor('[data-testid="batch-job"]', 10000);
  await sleep(900);
  await snap("batch-export");
  }

  // 6. device frames at tall and square previews (no clipping)
  if (want("frames")) {
    for (const [frame, aspect, name] of [["laptop", "9:16", "frame-laptop-9x16"], ["phone", "1:1", "frame-phone-1x1"], ["laptop", "1:1", "frame-laptop-1x1"], ["phone", "9:16", "frame-phone-9x16"]]) {
      await ed(`e.getState().apply((p) => ({ ...p, style: { ...p.style, frame: ${JSON.stringify(frame)} } })); e.getState().select(null);`);
      await cdp.eval(`[...document.querySelectorAll(".toolbar [role=radio]")].find((b) => b.textContent.trim() === ${JSON.stringify(aspect)}).click()`);
      await sleep(500);
      await snap(name);
    }
  }
}
