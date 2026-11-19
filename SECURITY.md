# Security policy

## Supported versions

Frostcap is pre-release. Until 1.0, only the **latest published release** (and the `main`
branch) receives security fixes. There are no backports to older versions.

| Version | Supported |
|---|---|
| Latest release | Yes |
| `main` | Yes (fixed in the next release) |
| Anything older | No - please update |

## Reporting a vulnerability

Please report vulnerabilities **privately** through GitHub:

1. Open <https://github.com/ColeDonMedia/frostcap/security/advisories/new> (or the
   repository's **Security** tab, then **Report a vulnerability**). This is GitHub's private
   vulnerability reporting.
2. Fill in the form.
3. Describe the issue, affected version, and steps or a proof of concept.

Do not open a public issue, discussion or pull request for a vulnerability before it is fixed.

> Private vulnerability reporting is enabled for this repository, so no email address is needed.

What to expect: an acknowledgement within about a week, an assessment, and a fix in a new
release when the report is confirmed. This is a volunteer project, so there are no
guaranteed response times and no bug bounty. With your permission you will be credited in
the advisory.

## Scope

In scope, for example:

- Code execution or privilege issues from opening a crafted `.frostcap` project or file
- The web UI escaping its sandbox (IPC commands, asset-protocol scope, CSP bypass)
- Recorded data (keys, window titles, audio) being written somewhere unexpected or captured
  when the user turned it off
- Any network traffic from Frostcap (it should make none)
- Vulnerable dependencies that are actually reachable in `frostcap.exe`

Out of scope: vulnerabilities in Windows, WebView2 or GPU drivers themselves (report them
to the vendor), SmartScreen warnings on unsigned builds, and issues that need an attacker
who already controls your user account.

## Supply chain

- Dependencies are checked against the RustSec advisory database (`cargo deny check
  advisories`) in CI, and Dependabot proposes updates.
- Release artefacts come with SHA-256 checksums; see
  [docs/RELEASING.md](docs/RELEASING.md#verifying-a-download).
