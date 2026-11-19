---
description: Fresh-context code review, UI/UX review, then hardening review, of the current diff.
---

Review the current diff with fresh subagents, in order:
1. reviewer — correctness, edge cases, contract mismatches, regressions.
2. ui-reviewer — UI/UX, brand/glass consistency, flows, accessibility, responsiveness (skip if no UI changes).
3. security-reviewer — IPC boundary, file paths, privacy, validation, permissions, supply chain.

All are read-only. Report each verdict (APPROVE or CHANGES REQUESTED) with findings. Do not ship on a CHANGES REQUESTED verdict.
