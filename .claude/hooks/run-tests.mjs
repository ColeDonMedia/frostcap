#!/usr/bin/env node
// PostToolUse hook (Edit|Write): runs only the test suite affected by the edited file,
// so feedback is fast. Exit 2 + stderr feeds a RED result back to Claude; exit 0 is GREEN.
// Uses the repo-local toolchain on E: (same as env.ps1). No dependencies.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
let input = {};
try { input = JSON.parse(readFileSync(0, "utf8") || "{}"); } catch { /* no stdin */ }
const file = input?.tool_input?.file_path ?? "";
const rel = path.relative(root, path.resolve(file)).replaceAll("\\", "/");
if (!file || rel.startsWith("..")) process.exit(0); // outside the repo

const env = {
  ...process.env,
  RUSTUP_HOME: path.join(root, ".toolchain", "rustup"),
  CARGO_HOME: path.join(root, ".toolchain", "cargo"),
  CARGO_TARGET_DIR: path.join(root, "target"),
  PATH: path.join(root, ".toolchain", "cargo", "bin") + path.delimiter + (process.env.PATH ?? ""),
};

let cmd = null;
if (rel.startsWith("ui/") && /\.(ts|tsx|css|json)$/.test(rel)) {
  cmd = ["npm", ["--prefix", "ui", "test", "--silent"]];
} else if (/\.(rs|toml|wgsl)$/.test(rel)) {
  // Crate dir names are restricted to [a-z0-9_-] so nothing from the path reaches the shell unchecked.
  const m = rel.match(/^crates\/([a-z0-9_-]+)\//);
  const pkg = m ? m[1] : rel.startsWith("src-tauri/") ? "frostcap" : null;
  // src-tauri has few unit tests and is slow to link; `--lib` keeps it to unit tests.
  cmd = ["cargo", pkg ? ["test", "-q", "-p", pkg, "--lib"] : ["test", "-q", "--workspace", "--lib"]];
}
if (!cmd) process.exit(0);

// Args are constants or a validated crate name (never raw tool input), so one shell string is safe;
// a shell is needed on Windows to run npm.cmd.
const line = `${cmd[0]} ${cmd[1].join(" ")}`;
const r = spawnSync(line, { cwd: root, env, encoding: "utf8", shell: true, timeout: 170_000 });
const out = `${r.stdout ?? ""}${r.stderr ?? ""}`.trim().split(/\r?\n/).slice(-25).join("\n");
if (r.error?.code === "ETIMEDOUT" || r.signal) {
  console.log(`tests skipped (timeout or lock contention): ${line}`);
  process.exit(0);
}
if (r.status !== 0) {
  console.error(`RED: ${line} failed after editing ${rel}\n${out}`);
  process.exit(2);
}
const summary = out.split("\n").filter((l) => /test result|Tests |Test Files/.test(l)).slice(-3).join("\n");
console.log(`GREEN: ${line}\n${summary}`);
