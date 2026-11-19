---
description: Run the full Frostcap verification sweep — tests, typecheck, lint, licenses, production build — before shipping.
---

Run the full pre-ship verification sweep in PowerShell after `. .\env.ps1`:
1. Full test suite: `cargo test --workspace` and `npm --prefix ui test`.
2. Typecheck: `npm --prefix ui run typecheck`.
3. Lint: `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
4. Licenses and advisories: `cargo deny check` and `node scripts/gen-third-party.mjs --check`.
5. Production build: `npm --prefix ui run build` then `cargo build --release -p frostcap`; report the exe size.
6. Smoke: `cargo run -p frostcap-core --example record -- 3` and confirm a valid MP4 is produced.

Report each step's real output. Ship only when ALL steps are green. If anything fails, fix it and re-run the full sweep.
