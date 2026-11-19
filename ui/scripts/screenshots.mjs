#!/usr/bin/env node
// README screenshots: drives headless Microsoft Edge over the Chrome DevTools Protocol
// (Node's built-in fetch/WebSocket only; no puppeteer) against the Vite dev server with the mock backend.
//
//   node ui/scripts/screenshots.mjs                 # dark + light, 1600x1000, into assets/screenshots/
//   node ui/scripts/screenshots.mjs --lang de --out <dir> --width 1280 --height 800   # layout check for a locale
//
// Starts `vite` itself when nothing answers on --url and stops it afterwards.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const uiDir = path.resolve(here, "..");
const repo = path.resolve(uiDir, "..");

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]?.startsWith("--") || all[i + 1] === undefined ? "true" : all[i + 1]]] : acc), []),
);
const URL_ = args.url ?? "http://localhost:5173/";
const W = Number(args.width ?? 1600);
const H = Number(args.height ?? 1000);
const OUT = path.resolve(args.out ?? path.join(repo, "assets", "screenshots"));
const LANG = args.lang ?? null;
const PORT = Number(args.port ?? 9333);
const PROFILE = path.join(repo, ".toolchain", "edge-screenshot-profile");
const EDGE = [process.env.EDGE_PATH, "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe"].find((p) => p && fs.existsSync(p));
if (!EDGE) throw new Error("Microsoft Edge not found (set EDGE_PATH)");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const alive = async (url) => {
  try {
    return (await fetch(url, { signal: AbortSignal.timeout(1500) })).ok;
  } catch {
    return false;
  }
};
const killTree = (child) => child?.pid && spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });

async function until(fn, what, ms = 20000) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t0 > ms) throw new Error(`timed out waiting for ${what}`);
    await sleep(100);
  }
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (e) => {
      const m = JSON.parse(e.data);
      if (m.id && this.pending.has(m.id)) {
        const { resolve, reject } = this.pending.get(m.id);
        this.pending.delete(m.id);
        m.error ? reject(new Error(`${m.error.message}`)) : resolve(m.result);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expression) {
    const r = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
    return r.result.value;
  }
}

async function main() {
  let vite = null;
  if (!(await alive(URL_))) {
    const viteBin = path.join(uiDir, "node_modules", "vite", "bin", "vite.js");
    vite = spawn(process.execPath, [viteBin], { cwd: uiDir, stdio: "ignore" });
    await until(() => alive(URL_), "the Vite dev server", 60000);
  }
  fs.mkdirSync(OUT, { recursive: true });
  fs.mkdirSync(PROFILE, { recursive: true });
  const edge = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${PROFILE}`, `--window-size=${W},${H}`, "--hide-scrollbars", "--no-first-run", "--disable-gpu", "about:blank"], { stdio: "ignore" });
  try {
    const target = await until(async () => {
      try {
        const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
        return list.find((t) => t.type === "page");
      } catch {
        return null;
      }
    }, "Edge DevTools");
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((res, rej) => {
      ws.addEventListener("open", res, { once: true });
      ws.addEventListener("error", rej, { once: true });
    });
    const cdp = new Cdp(ws);
    await cdp.send("Page.enable");
    await cdp.send("Runtime.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", { width: W, height: H, deviceScaleFactor: 1, mobile: false });

    const waitFor = (selector, ms) => until(() => cdp.eval(`!!document.querySelector(${JSON.stringify(selector)})`), selector, ms);
    const click = (selector) => cdp.eval(`document.querySelector(${JSON.stringify(selector)}).click()`);
    const shot = async (name) => {
      await sleep(900); // let entrance animations and the canvas settle
      const { data } = await cdp.send("Page.captureScreenshot", { format: "png" });
      const file = path.join(OUT, `${name}.png`);
      fs.writeFileSync(file, Buffer.from(data, "base64"));
      console.log(`wrote ${file}`);
    };

    // Before any page script runs: pin the theme (and language) like a saved setting, and reduce motion noise.
    if (args.scenes === "v2") {
      const { v2Scenes } = await import("./screenshots-v2.mjs");
      await v2Scenes({ cdp, waitFor, click, shot, until, sleep, url: URL_, theme: args.theme ?? "dark", lang: LANG, scenes: args.only ?? "all", suffix: args.suffix ?? "" });
      ws.close();
      return;
    }

    const themes = args.theme ? [args.theme] : ["dark", "light"];
    let hook = null;
    for (const theme of themes) {
      if (hook) await cdp.send("Page.removeScriptToEvaluateOnNewDocument", { identifier: hook });
      const settings = { theme, ...(LANG ? { language: LANG } : {}) };
      hook = (await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: `localStorage.setItem("frostcap.settings.v1", ${JSON.stringify(JSON.stringify(settings))});` })).identifier;
      await cdp.send("Page.navigate", { url: URL_ });
      const sfx = theme === "light" ? "-light" : "";
      const tag = LANG ? `-${LANG}` : "";

      await waitFor(".srccard", 20000);
      await waitFor('[data-testid="recent-0"]', 20000);
      await shot(`recorder${sfx}${tag}`);

      await click('[data-testid="recent-0"]');
      await waitFor('[data-testid="editor-export"]', 20000);
      await waitFor(".preview canvas", 20000);
      await sleep(600);
      await shot(`editor${sfx}${tag}`);

      await click('[data-testid="editor-export"]');
      await waitFor('[data-testid="export-sheet"]', 10000);
      await shot(`export${sfx}${tag}`);
      await click('[data-testid="export-sheet"] .sheet__foot .btn'); // first footer button is Cancel/Close
      await sleep(800); // best effort: leaving the editor unmounts the sheet anyway

      await click('[data-testid="nav-settings"]');
      await waitFor("#s-hotkeys", 10000);
      await shot(`settings${sfx}${tag}`);
    }
    ws.close();
  } finally {
    killTree(edge);
    killTree(vite);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
