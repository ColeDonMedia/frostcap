# Code signing policy

**Status: Frostcap releases are not code-signed yet.** Windows SmartScreen may warn on first
run; verify downloads against `SHA256SUMS.txt` and the build provenance
([RELEASING.md](RELEASING.md#verifying-a-download)).

The project intends to apply to the [SignPath Foundation](https://signpath.org/), which offers
free code signing to open-source projects. This page is the policy that application refers to.
When signing is approved, the line below is added and this status note is removed:

> Free code signing provided by [SignPath.io](https://signpath.io), certificate by
> [SignPath Foundation](https://signpath.org).

## What is signed

Only the release executables (`Frostcap_<version>_x64-setup.exe` and
`Frostcap_<version>_x64-portable.exe`) built by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) on GitHub-hosted runners
from a `vX.Y.Z` tag of this repository. Nothing built on a personal machine is signed.

## Team roles

- **Committers and reviewers:** the maintainers listed in [`.github/CODEOWNERS`](../.github/CODEOWNERS).
- **Approvers:** the same maintainers. Each signing request is approved by hand for a tagged
  release whose CI run passed.

All maintainers use two-factor authentication on GitHub and on the signing service.

## Privacy

Frostcap makes no network requests and sends no data anywhere. See [PRIVACY.md](PRIVACY.md).
