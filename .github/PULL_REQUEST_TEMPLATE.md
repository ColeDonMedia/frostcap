## What and why

<!-- What does this change and why? Link issues with "Fixes #123". -->

## How it was tested

<!-- Commands run, manual steps (which Windows version / monitor setup), screenshots for UI changes. -->

## Checklist

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] `npm run typecheck` and `npm run build` pass in `ui/`
- [ ] No network access, telemetry, accounts or analytics added
- [ ] New dependencies are permissively licensed; `cargo deny check` passes
- [ ] If dependencies changed: ran `node scripts/gen-third-party.mjs` and committed `THIRD_PARTY_LICENSES.md`
- [ ] If recorded data or storage changed: updated `docs/PRIVACY.md`
- [ ] Docs updated (README status table, `docs/ARCHITECTURE.md`, `docs/UI.md`) where relevant
