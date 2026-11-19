# Project
Frostcap: free MIT Windows screen recorder/editor. Tauri v2 + Rust (crates/frostcap-core capture, crates/frostcap-render wgpu export, src-tauri commands) + React 19/TypeScript/Vite UI (ui/), glassmorphism design. Windows 11 first.

# Hard rules
- Dependencies must be permissive (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0). No GPL/LGPL/AGPL/MPL/OFL/CC-BY in shipped code, no FFmpeg. `cargo deny check` must pass.
- No AI, no cloud, no telemetry, no network calls. No watermarks or paywalls.
- Lean: small exe, minimal features on crates, no heavy npm deps.
- Video encode/decode only through Windows Media Foundation (no bundled codecs).
- C: is nearly full. Toolchain, caches and build output live on E: (see env.ps1). Never write caches to C:.
- Rust (core/src-tauri DTOs) is the source of truth for the UI <-> backend contract; see docs/CONTRACT.md and fixtures/.
- Brand: Frost Cyan #4ED2FB, Glacier Blue #389AF5, Arctic Blue #0752F1, Deep Frost Blue #1151EF, Snow White #FCFDFC; logo assets/brand/logo.png (docs/BRAND.md).
- Never commit recordings (*.frostcap, *.mp4, *.wav). Commit only when the user asks.

# Commands (PowerShell; first run `. .\env.ps1`)
- Dev UI (browser, mock backend): `npm --prefix ui run dev` (http://localhost:5173)
- Dev app: `npm --prefix ui run tauri dev`
- Rust tests: `cargo test --workspace` (desktop/GPU/Media Foundation tests: `cargo test --workspace -- --ignored` on a Windows desktop)
- UI tests: `npm --prefix ui test`
- Typecheck: `npm --prefix ui run typecheck`
- Lint: `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`
- Licenses/advisories: `cargo deny check`, `node scripts/gen-third-party.mjs --check`
- Build: `npm --prefix ui run build` then `cargo build --release -p frostcap`

# Workflow — STRICT TDD, always in this order
1. RED: write a failing test first. Run it. Confirm it FAILS.
2. GREEN: write the minimum code to make it pass.
3. REFACTOR: clean up without changing behavior. Re-run tests.
- Never write implementation without a failing test first.
- Never write more implementation than the current test requires.
- Never delete or weaken a test to make it pass.
- Hardware/OS paths (capture, encoders, windows) that can't be unit tested: test the pure logic around them and add a real smoke run (examples/) with its output in the report.
- After every feature: run the FULL suite (Rust + UI).
- Before shipping: `/full-scan` all green.

# Reviews (fresh subagents, read-only)
- After each feature: reviewer (correctness) and ui-reviewer (UI/UX) for UI changes.
- Before shipping: security-reviewer hardening review.
- Never accept "done" — accept a passing test run, a diff, or a log.

# Context hygiene
- /clear between unrelated tasks. /compact when a session gets long.
- Push noisy research into the explorer subagent; keep main context clean.
- Parallel implementers must own disjoint files.
