---
description: Run one strict TDD cycle (red → green → refactor) for a single Frostcap feature or fix.
---

Run ONE strict TDD cycle for: $ARGUMENTS

1. RED: write the failing test first (Rust `#[cfg(test)]` or vitest). Run it. Confirm it FAILS before any implementation.
2. GREEN: write the minimum code to make it pass.
3. REFACTOR: clean up without changing behavior. Re-run tests.
4. If the feature touches OS capture/encode paths, finish with a real smoke run and show its output.

Do not move on to the next feature until this cycle is green.
