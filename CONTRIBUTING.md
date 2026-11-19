# Contributing to Frostcap

Thanks for helping. Frostcap is a free, offline, MIT-licensed screen recorder for Windows.
Every contribution has to keep it that way.

## Ground rules

1. **No network.** No telemetry, analytics, crash upload, update checks, remote fonts or
   CDNs, accounts or "phone home" of any kind. The UI's Content Security Policy enforces
   local-only resources; do not loosen it. If a feature seems to need the network, open an
   issue to discuss it first.
2. **Permissive licenses only.** New dependencies (Rust or npm, including transitive ones)
   must be MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unlicense, 0BSD, CC0-1.0 or
   Unicode-3.0. No GPL, LGPL, AGPL, MPL, SSPL, BUSL, OFL or proprietary/"source available"
   code. `deny.toml` is the source of truth for Rust; `cargo deny check` must pass. Do not
   add exceptions without a maintainer's agreement.
3. **No bundled codecs.** Video and audio encoding go through Windows Media Foundation.
   No FFmpeg, x264, openh264 or similar.
4. **Privacy-sensitive data stays opt-in.** Anything that records more about the user
   (keys, window titles, webcam, microphone) must be off by default or clearly chosen by the
   user, and documented in [docs/PRIVACY.md](docs/PRIVACY.md).
5. **You own what you submit.** By contributing you agree your contribution is licensed
   under the project's [MIT License](LICENSE). Only submit code you wrote or that is under
   a compatible license, with its notice kept. Do not paste code from GPL projects,
   Stack Overflow answers without attribution, or AI output you have not reviewed.

## Getting set up

See [Build from source](README.md#build-from-source). In short: rustup (the toolchain is
pinned in `rust-toolchain.toml`), MSVC Build Tools with the C++ workload, Node.js 20+.
On a small system drive, dot-source `env.ps1` to keep toolchains and caches in the repo.

Where things live:

- `crates/frostcap-core` - capture, audio, input, project format, auto-zoom. Keep it UI-free
  and unit-testable. Pure logic (like `autozoom`) should have tests.
- `crates/frostcap-render` - export renderer.
- `src-tauri` - thin command adapters over the core.
- `ui` - React UI. It runs in a browser with a mock backend, so most UI work does not need
  the Rust side.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/UI.md](docs/UI.md) and
[docs/BRAND.md](docs/BRAND.md) (colours, contrast rules, logo usage) before larger changes.

## Before you open a pull request

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Tests that need an interactive desktop, a GPU/WGC or the Media Foundation
# H.264 encoder are #[ignore]d; run them locally on a Windows desktop with:
cargo test --workspace -- --ignored
cargo deny check
cd ui; npm run typecheck; npm run build; cd ..
```

If you changed `Cargo.toml`, `Cargo.lock` or `ui/package-lock.json`, regenerate the license
inventory and commit it (CI fails otherwise):

```powershell
node scripts/gen-third-party.mjs
```

- Keep pull requests focused; one topic each.
- Describe how you tested, including Windows version and monitor/DPI setup for capture
  changes. Screenshots help for UI changes (no private content in them).
- Update docs when behaviour changes, especially the status table in the README and
  `docs/PRIVACY.md` for anything about recorded or stored data.
- Use clear commit messages (imperative mood: "Add region picker", "Fix audio offset").

## Reporting bugs and ideas

Use the issue templates. For crashes or capture problems include Windows version/edition
(`winver`), GPU and monitor setup. Do not attach recordings or `project.json` files that
contain private information.

Security problems: **do not open a public issue**, see [SECURITY.md](SECURITY.md).

## Code of conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
