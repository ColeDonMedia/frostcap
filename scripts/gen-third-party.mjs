#!/usr/bin/env node
// Regenerates THIRD_PARTY_LICENSES.md from the Cargo dependency graph and
// ui/package-lock.json. No dependencies beyond Node >= 18 and cargo.
//
//   node scripts/gen-third-party.mjs           # write THIRD_PARTY_LICENSES.md
//   node scripts/gen-third-party.mjs --check   # exit 1 if the file is out of date
//
// Prerequisites: the cargo registry sources must be present (any cargo build,
// `cargo fetch`, or `cargo tree` downloads them) and `npm ci` must have been run
// in ui/ so license files can be read from ui/node_modules.
//
// Classification of Rust crates (Windows target x86_64-pc-windows-msvc):
//   * "linked"    - `cargo tree -p frostcap -e normal,no-proc-macro`: compiled into frostcap.exe
//   * "workspace" - normal deps of other workspace members not (yet) linked into frostcap.exe
//   * "build"     - everything else in the graph: proc-macros, build scripts,
//                   build-time code generators and dev-dependencies. Not distributed.
// `cargo tree` is used (not `cargo metadata`'s resolve) because only cargo tree
// applies resolver-2 host/target feature separation, which is what keeps the
// MPL-2.0 codegen crates out of the linked set.

import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(ROOT, 'THIRD_PARTY_LICENSES.md');
const TARGET = 'x86_64-pc-windows-msvc';
const APP_PKG = 'frostcap';
const CHECK = process.argv.includes('--check');

function run(cmd, args) {
  return execFileSync(cmd, args, { cwd: ROOT, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
}

// ---------------------------------------------------------------- Rust ----
const meta = JSON.parse(run('cargo', ['metadata', '--format-version', '1', '--filter-platform', TARGET]));
const wsIds = new Set(meta.workspace_members);
const byKey = new Map(); // "name@version" -> package
for (const p of meta.packages) byKey.set(`${p.name}@${p.version}`, p);

function treeSet(args) {
  const out = run('cargo', ['tree', ...args, '--target', TARGET, '--prefix', 'none', '--format', '{p}']);
  const set = new Set();
  for (const line of out.split(/\r?\n/)) {
    const m = /^(\S+) v(\S+)/.exec(line.trim());
    if (m) set.add(`${m[1]}@${m[2]}`);
  }
  return set;
}

const linked = treeSet(['-p', APP_PKG, '-e', 'normal,no-proc-macro']);
const wsNormal = treeSet(['--workspace', '-e', 'normal,no-proc-macro']);
const all = treeSet(['--workspace', '-e', 'all']);

const isWs = (k) => wsIds.has(byKey.get(k)?.id);
const firstParty = [...all].filter(isWs).sort();
const sortKeys = (a) => a.sort((x, y) => x.localeCompare(y, 'en'));
const linkedList = sortKeys([...linked].filter((k) => !isWs(k)));
const wsOnlyList = sortKeys([...wsNormal].filter((k) => !isWs(k) && !linked.has(k)));
const buildList = sortKeys([...all].filter((k) => !isWs(k) && !linked.has(k) && !wsNormal.has(k)));

const LICENSE_RE = /^(licen[cs]e|copying|copyright|notice|unlicense)([-_.].*)?$/i;

function licenseFiles(dir, declared) {
  const files = new Set();
  if (declared) {
    const f = resolve(dir, declared);
    if (existsSync(f)) files.add(f);
  }
  let entries = [];
  try { entries = readdirSync(dir); } catch { return []; }
  for (const e of entries) {
    const full = join(dir, e);
    let st;
    try { st = statSync(full); } catch { continue; }
    if (st.isFile() && LICENSE_RE.test(e)) files.add(full);
    // e.g. LICENSES/MIT.txt (REUSE layout) or license/ directories
    if (st.isDirectory() && /^licen[cs]es?$/i.test(e)) {
      for (const s of readdirSync(full)) {
        const sf = join(full, s);
        if (statSync(sf).isFile()) files.add(sf);
      }
    }
  }
  return [...files].sort();
}

const norm = (t) => t.replace(/^﻿/, '').replace(/\r\n?/g, '\n').split('\n').map((l) => l.replace(/\s+$/, '')).join('\n').trim();

// Text groups shared by Rust and npm entries.
const texts = new Map(); // hash -> { text, users: Set<string> }
function addText(text, user) {
  const t = norm(text);
  if (!t) return null;
  const h = createHash('sha256').update(t).digest('hex');
  if (!texts.has(h)) texts.set(h, { text: t, users: new Set() });
  texts.get(h).users.add(user);
  return h;
}
const missing = []; // { user, license, authors }

function rustEntry(k) {
  const p = byKey.get(k);
  if (!p) return { key: k, name: k.split('@')[0], version: k.split('@')[1], license: '(unknown)', repo: '', files: [] };
  const dir = dirname(p.manifest_path);
  const files = licenseFiles(dir, p.license_file);
  const user = `${p.name} ${p.version}`;
  for (const f of files) addText(readFileSync(f, 'utf8'), user);
  if (files.length === 0) missing.push({ user, license: p.license ?? p.license_file ?? '(none declared)', authors: p.authors ?? [] });
  return { key: k, name: p.name, version: p.version, license: p.license ?? `see ${p.license_file}`, repo: p.repository ?? '', files };
}

const linkedE = linkedList.map(rustEntry);
const wsOnlyE = wsOnlyList.map(rustEntry);
const buildE = buildList.map(rustEntry);

// ----------------------------------------------------------------- npm ----
const UI = join(ROOT, 'ui');
const lock = JSON.parse(readFileSync(join(UI, 'package-lock.json'), 'utf8'));
const npmProd = [];
const npmDev = [];
for (const [path, info] of Object.entries(lock.packages ?? {})) {
  if (!path.startsWith('node_modules/')) continue;
  const name = path.slice(path.lastIndexOf('node_modules/') + 'node_modules/'.length);
  const dir = join(UI, path);
  let license = info.license ?? '';
  let repo = '';
  if (existsSync(join(dir, 'package.json'))) {
    try {
      const pj = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
      if (!license) license = typeof pj.license === 'string' ? pj.license : pj.license?.type ?? '';
      repo = typeof pj.repository === 'string' ? pj.repository : pj.repository?.url ?? '';
    } catch { /* ignore */ }
  }
  repo = repo.replace(/^git\+/, '').replace(/\.git$/, '')
    .replace(/^(?:github:)?([\w.-]+\/[\w.-]+)$/, 'https://github.com/$1')
    .replace(/^(?:git|ssh):\/\/(?:git@)?/, 'https://')
    .replace(/^git@github\.com:/, 'https://github.com/');
  const e = { name, version: info.version ?? '', license: license || '(unknown)', repo, dir, optional: !!info.optional };
  // devOptional = optional in the prod tree *or* dev: treat as production to be safe.
  if (info.dev) npmDev.push(e); else npmProd.push(e);
}
const byName = (a, b) => (a.name + '@' + a.version).localeCompare(b.name + '@' + b.version, 'en');
npmProd.sort(byName);
npmDev.sort(byName);
let npmProdMissingNodeModules = 0;
for (const e of [...npmProd, ...npmDev]) {
  const user = `npm:${e.name} ${e.version}`;
  if (!existsSync(e.dir)) {
    // Platform-specific optional packages (e.g. other-OS esbuild/rollup binaries) are not installed.
    if (!npmDev.includes(e)) npmProdMissingNodeModules++;
    e.installed = false;
    continue;
  }
  e.installed = true;
  // Dev packages are listed (section 5) but their texts are not reproduced: they are not distributed.
  if (npmDev.includes(e)) continue;
  const files = licenseFiles(e.dir, null);
  for (const f of files) addText(readFileSync(f, 'utf8'), user);
  if (files.length === 0) missing.push({ user, license: e.license, authors: [] });
}

// ------------------------------------------------- npm license policy ----
// Production (bundled) npm packages must be under one of these licenses. Mirrors the
// spirit of deny.toml for Rust; dev-only packages are not checked (not distributed).
const NPM_ALLOW = new Set(['MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', '0BSD', 'Zlib', 'Unlicense', 'CC0-1.0']);

// Minimal SPDX expression evaluator: OR / AND / parentheses. Legacy "A/B" is read as
// "A OR B". `WITH` exceptions and anything unrecognised are not allowed.
function spdxAllowed(expr) {
  const toks = String(expr ?? '').replace(/\//g, ' OR ').replace(/[()]/g, ' $& ').trim().split(/\s+/).filter(Boolean);
  if (toks.length === 0) return false;
  let i = 0;
  const primary = () => {
    const t = toks[i++];
    if (t === '(') { const v = orExpr(); if (toks[i++] !== ')') throw new Error('unbalanced'); return v; }
    if (t === undefined || ['AND', 'OR', 'WITH', ')'].includes(t)) throw new Error('syntax');
    if (toks[i] === 'WITH') { i += 2; return false; }
    return NPM_ALLOW.has(t);
  };
  const andExpr = () => { let v = primary(); while (toks[i] === 'AND') { i++; const r = primary(); v = v && r; } return v; };
  const orExpr = () => { let v = andExpr(); while (toks[i] === 'OR') { i++; const r = andExpr(); v = v || r; } return v; };
  try { const v = orExpr(); return i === toks.length && v; } catch { return false; }
}
const npmViolations = npmProd.filter((e) => !spdxAllowed(e.license));

// -------------------------------------------------------------- output ----
const L = [];
const w = (s = '') => L.push(s);
const esc = (s) => String(s).replace(/\|/g, '\\|');
const repoCell = (r) => (r ? `<${r}>` : '');
const MPL = (lic) => /MPL/.test(lic);

function countBy(list) {
  const m = new Map();
  for (const e of list) m.set(e.license, (m.get(e.license) ?? 0) + 1);
  return [...m.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0], 'en'));
}
function table(list, flag) {
  w('| Crate / package | Version | License (SPDX) | Source |');
  w('|---|---|---|---|');
  for (const e of list) w(`| ${esc(e.name)}${flag?.(e) ?? ''} | ${esc(e.version)} | ${esc(e.license)} | ${repoCell(e.repo)} |`);
  w();
}
function summary(list) {
  w(countBy(list).map(([l, n]) => `${esc(l)}: ${n}`).join(' · '));
  w();
}

w('# Third-party licenses');
w();
w('<!-- GENERATED FILE: do not edit by hand. Regenerate with `node scripts/gen-third-party.mjs`. CI fails if it is stale. -->');
w();
w('Frostcap itself is MIT-licensed (see [LICENSE](LICENSE)). This file lists every third-party');
w('component in the build graph, its declared license, and the license texts and copyright');
w('notices shipped with each package. It is generated from `cargo tree` / `cargo metadata`');
w(`(target \`${TARGET}\`) and \`ui/package-lock.json\`. License texts are de-duplicated: identical`);
w('texts are printed once with the list of packages that ship them.');
w();
w('## Notes');
w();
w('- **What is distributed.** `frostcap.exe` contains the crates in section 1 and the bundled web UI');
w('  (`ui/dist`, embedded at build time), which contains the npm packages in section 4. Nothing else');
w('  from this list is shipped.');
w('- **MIT `option-ext` shim.** The MPL-2.0 crate `option-ext` (pulled in by `tauri -> dirs -> dirs-sys`)');
w('  is replaced via `[patch.crates-io]` with `crates/option-ext-shim`, an independently written');
w('  MIT crate in this repository. It appears below as a first-party crate, not as the MPL original.');
w('- **Build-time-only MPL-2.0 crates.** `cssparser`, `cssparser-macros`, `selectors` and `dtoa-short`');
w('  (MPL-2.0) are reached only through `tauri-macros` (a proc-macro) -> `tauri-codegen` -> `tauri-utils` ->');
w('  `dom_query`. They run inside the compiler while `tauri::generate_context!` expands and are **not');
w('  linked into frostcap.exe**. Verified with `cargo tree -e normal,no-proc-macro -i <crate>` (prints');
w('  nothing) and `cargo tree -e normal -i <crate>` (the only path goes through `tauri-macros (proc-macro)`).');
w('  They are allowed in `deny.toml` only as named exceptions. Re-check before every release.');
w('- **Unicode-3.0.** Crates under the Unicode License v3 (ICU4X data and tables via `url`/`idna`,');
w('  `unicode-ident`) only require that the notice is kept; their notices are reproduced in section 6.');
w('- **caniuse-lite (CC-BY-4.0)** is a transitive *dev* dependency (Vite/Babel -> browserslist). It is');
w('  a build tool data file, is not bundled into `ui/dist` and is not distributed.');
w('- **Not bundled:** Microsoft Edge WebView2 Runtime, Media Foundation and the H.264/AAC encoders are');
w('  operating-system components used at run time; see [docs/LEGAL.md](docs/LEGAL.md).');
w('- Licenses are as declared by each package; they were not individually audited.');
w();

w('## Summary');
w();
w(`- Rust crates linked into frostcap.exe: **${linkedE.length}**`);
w(`- Rust crates used only by other workspace members (not yet in frostcap.exe): **${wsOnlyE.length}**`);
w(`- Rust build-time-only crates (proc-macros, build scripts, codegen, dev): **${buildE.length}**`);
w(`- npm production packages (bundled into the UI): **${npmProd.length}**`);
w(`- npm development packages (build tools, not distributed): **${npmDev.length}**`);
w();

w('## First-party workspace crates (MIT)');
w();
for (const k of firstParty) {
  const p = byKey.get(k);
  w(`- \`${p.name}\` ${p.version} (${p.license ?? 'MIT'}) - ${p.description ?? ''}`.trimEnd());
}
w();

w('## 1. Rust crates linked into frostcap.exe');
w();
summary(linkedE);
table(linkedE);

w('## 2. Rust crates used by other workspace members only');
w();
w('Dependencies of workspace crates (for example `frostcap-render` and the examples) that are not');
w('currently part of `frostcap.exe`. They will move to section 1 when the app starts linking them.');
w();
if (wsOnlyE.length) { summary(wsOnlyE); table(wsOnlyE); } else { w('_None._'); w(); }

w('## 3. Rust build-time-only crates (not distributed)');
w();
w('Proc-macros, build-script dependencies, code generators and dev-dependencies. Entries marked');
w('**(MPL-2.0, build-time only)** are the narrow exceptions described in the notes.');
w();
summary(buildE);
table(buildE, (e) => (MPL(e.license) ? ' **(MPL-2.0, build-time only)**' : ''));

w('## 4. npm production packages (bundled into ui/dist)');
w();
w('Taken from `ui/package-lock.json` (entries not marked `dev`). Type-only packages such as');
w('`@types/*` and `csstype` can appear here through peer dependencies; they emit no runtime code.');
w();
summary(npmProd);
table(npmProd,(e) => (e.installed ? '' : ' _(optional, not installed on this platform)_'));

w('## 5. npm development packages (not distributed)');
w();
w('Type checker, bundler, test runner, Tauri CLI and their dependencies. Used only to build and');
w('test the UI; their license texts are not reproduced here because they are not distributed.');
w();
summary(npmDev);
table(npmDev, (e) => (e.installed === false ? ' _(optional, not installed on this platform)_' : e.name === 'caniuse-lite' ? ' **(CC-BY-4.0 data, dev only)**' : ''));

w('## 6. License texts and notices');
w();
if (missing.length) {
  w('### Packages that do not ship a license file');
  w();
  w('These packages declare a license in their manifest but include no license file in the published');
  w('package. The standard text of each declared license is available from <https://spdx.org/licenses/>');
  w('and, for MIT/Apache-2.0, is also reproduced in the texts below. Copyright: the listed authors.');
  w();
  w('| Package | Declared license | Authors |');
  w('|---|---|---|');
  for (const m of missing.sort((a, b) => a.user.localeCompare(b.user, 'en'))) {
    w(`| ${esc(m.user.replace(/^npm:/, 'npm: '))} | ${esc(m.license)} | ${esc(m.authors.join(', ').replace(/<[^>]*>/g, '').trim())} |`);
  }
  w();
}
const groups = [...texts.values()].sort((a, b) => {
  const ua = [...a.users].sort()[0];
  const ub = [...b.users].sort()[0];
  return ua.localeCompare(ub, 'en') || a.text.localeCompare(b.text, 'en');
});
let i = 0;
for (const g of groups) {
  i++;
  const users = [...g.users].sort((a, b) => a.localeCompare(b, 'en')).map((u) => u.replace(/^npm:/, 'npm: '));
  w(`### Text ${i}`);
  w();
  w(`Used by: ${users.join(', ')}`);
  w();
  let fence = '~~~~';
  while (g.text.includes(fence)) fence += '~';
  w(fence + 'text');
  w(g.text);
  w(fence);
  w();
}

const output = L.join('\n').replace(/\n{3,}/g, '\n\n').trimEnd() + '\n';

if (npmViolations.length) {
  console.error('npm production packages with a license outside the allowlist (' + [...NPM_ALLOW].join(', ') + '):');
  for (const e of npmViolations) console.error(`  ${e.name}@${e.version}: ${e.license}`);
  if (CHECK) process.exit(1);
  console.error('(THIRD_PARTY_LICENSES.md was still written; --check will fail until this is resolved.)');
}

if (CHECK) {
  const current = existsSync(OUT) ? readFileSync(OUT, 'utf8').replace(/\r\n/g, '\n') : '';
  if (current !== output) {
    console.error('THIRD_PARTY_LICENSES.md is out of date. Run `node scripts/gen-third-party.mjs` and commit the result.');
    process.exit(1);
  }
  console.log('THIRD_PARTY_LICENSES.md is up to date.');
} else {
  writeFileSync(OUT, output, 'utf8');
  console.log(`Wrote ${OUT}: ${linkedE.length} linked, ${wsOnlyE.length} workspace-only, ${buildE.length} build-time crates; ${npmProd.length} npm prod, ${npmDev.length} npm dev; ${groups.length} distinct license texts; ${missing.length} without license files.`);
}
