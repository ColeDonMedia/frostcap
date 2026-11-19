# Releasing Frostcap

Releases are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml) when a
`vX.Y.Z` tag is pushed. The workflow creates a **draft** GitHub Release; a maintainer checks it
and publishes it by hand.

## 1. Prepare

1. Make sure `main` is green in CI.
2. Pick the version (semver; while < 1.0, bump the minor for features, patch for fixes;
   pre-releases like `0.2.0-beta.1` are published as GitHub pre-releases).
3. Bump the version in **all** of these (the release workflow fails if the first two differ
   from the tag):
   - `Cargo.toml` -> `[workspace.package] version`
   - `src-tauri/tauri.conf.json` -> `version`
   - `ui/package.json` -> `version` (then `npm install --package-lock-only` in `ui/` to update
     `ui/package-lock.json`)
4. Run `cargo update -w` (workspace versions in `Cargo.lock`), then:

   ```powershell
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   cargo deny check
   node scripts/gen-third-party.mjs        # commit THIRD_PARTY_LICENSES.md if it changed
   cd ui; npm ci; npm run build; cd ..
   ```

5. Re-check the release-sensitive items:
   - `cargo tree -e normal,no-proc-macro -i cssparser@<ver>` (and `selectors`, `dtoa-short`,
     `cssparser-macros`) prints nothing, i.e. the MPL-2.0 crates are still build-time only.
   - Keystroke capture still defaults to Off and [PRIVACY.md](PRIVACY.md) matches the app.
   - The README status table matches what actually works.
   - Unmaintained transitive advisories: `cargo deny check advisories` with
     `unmaintained = "all"` temporarily, and decide on each.
6. Update the changelog / release notes (what changed, known issues).
7. Commit ("Release vX.Y.Z"), push, wait for CI.

## 2. Tag and build

```powershell
git tag -a vX.Y.Z -m "Frostcap X.Y.Z"
git push origin vX.Y.Z
```

The workflow has four jobs, each with only the permissions it needs:

1. **build** (`contents: read`): validates the tag (`^v\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`,
   passed only through an environment variable), checks it matches `tauri.conf.json` and
   `Cargo.toml`, installs UI packages with `npm ci --ignore-scripts` (no npm cache), builds
   the UI, runs the Rust tests and the license-inventory check, builds the **NSIS installer**
   with the Tauri CLI (`tauri.conf.json` keeps `bundle.active = false` for normal builds; the
   workflow merges the overlay
   `{"build":{"beforeBuildCommand":""},"bundle":{"active":true,"targets":["nsis"]}}` via
   `tauri build --config ... -- --locked`, so the already-built UI is not reinstalled and
   cargo refuses a stale `Cargo.lock`), copies `target/release/frostcap.exe` as the **portable exe**,
   writes `SHA256SUMS.txt` and uploads all three as a workflow artefact.
2. **deny** (`contents: read`): `cargo deny check` (licenses, advisories, bans, sources).
3. **attest** (`id-token: write`, `attestations: write`): signs a SLSA build-provenance
   attestation for the two executables and `SHA256SUMS.txt`.
4. **publish** (`contents: write`): downloads the artefact (it never checks out or runs
   repository code), re-verifies the checksums and creates the **draft** release.

All checkouts use `persist-credentials: false`.

To rebuild an existing tag, run the workflow manually (Actions -> Release -> Run workflow) with
the tag name.

Local equivalent (from the repo root, after building the UI):

```powershell
'{ "build": { "beforeBuildCommand": "" }, "bundle": { "active": true, "targets": ["nsis"] } }' | Set-Content $env:TEMP\release.conf.json
ui\node_modules\.bin\tauri.cmd build --bundles nsis --config $env:TEMP\release.conf.json -- --locked
# -> target\release\bundle\nsis\Frostcap_X.Y.Z_x64-setup.exe and target\release\frostcap.exe
```

The Tauri CLI downloads NSIS and its plugins from GitHub the first time it bundles.

**npm install scripts are never run.** `ui/.npmrc` sets `ignore-scripts=true`, every
`npm ci` in CI, the release workflow and `tauri.conf.json`'s `beforeBuildCommand` passes
`--ignore-scripts`, and the release overlay empties `beforeBuildCommand`. The build does not
need install scripts (esbuild, rollup and the Tauri CLI use prebuilt platform packages).
Explicit `npm run <script>` / `npm test` still work; only automatic `pre*`/`post*` hooks and
dependency lifecycle scripts are skipped, so do not rely on `prebuild`-style hooks in
`ui/package.json`.

> Not yet exercised: as of this writing the release workflow has not been run end to end.
> Expect to fix small things (installer file name, icon set, NSIS options such as the
> WebView2 install mode) on the first release. Installer settings belong in
> `tauri.conf.json` under `bundle.windows.nsis`.

## 3. Code signing

Releases are currently **unsigned**. Options, from cheapest to most work:

| Option | Cost | Notes |
|---|---|---|
| **Unsigned** (current) | Free | Windows SmartScreen shows "Windows protected your PC" until the file builds reputation; users click "More info" -> "Run anyway". Publish checksums so users can verify downloads. |
| **SignPath Foundation** (free code signing for open-source projects) | Free if accepted | Signing happens on SignPath's service from a CI build; the project applies and must meet their criteria. Integrates with GitHub Actions. |
| **Azure Trusted Signing** (Microsoft "Artifact Signing") | Low monthly fee | Microsoft-managed certificates, signs via an Azure account and a GitHub Action / `signtool` plug-in. Eligibility has been limited by region and organisation/individual type. |
| Traditional OV/EV certificate | Paid yearly | Hardware token or cloud HSM required for new certificates. |

**The maintainers have not verified the current eligibility rules, pricing or terms of
SignPath or Azure Trusted Signing.** Check them before applying. Whichever is chosen, sign
both the installer and the portable exe (and ideally `frostcap.exe` before it is packed into
the installer), then compute checksums **after** signing. Tauri supports a custom
`bundle.windows.signCommand` for this.

Never store a signing key or certificate password in the repository; use the signing
service's GitHub integration or encrypted Actions secrets scoped to the release environment.

## 4. Publish

1. Open the draft release, check the assets (installer, portable exe, `SHA256SUMS.txt`).
2. Download both executables on a clean Windows 11 machine (and Windows 10 if supported),
   verify the checksums, install, record a short clip with mic + system audio, export it.
3. Paste the release notes, then **Publish release**.

## Verifying a download

Each release has a `SHA256SUMS.txt` listing the SHA-256 of every file. In PowerShell, in the
folder where you downloaded the files:

```powershell
Get-FileHash .\Frostcap_X.Y.Z_x64-setup.exe -Algorithm SHA256
Get-Content .\SHA256SUMS.txt
```

The hash printed by `Get-FileHash` must match the line for that file (case does not matter).
Or check all files at once:

```powershell
Get-Content .\SHA256SUMS.txt | ForEach-Object {
  $hash, $name = $_ -split '\s+', 2
  $ok = (Get-FileHash $name -Algorithm SHA256).Hash -eq $hash
  '{0}  {1}' -f ($(if ($ok) { 'OK  ' } else { 'FAIL' })), $name
}
```

In `cmd.exe`: `certutil -hashfile Frostcap_X.Y.Z_x64-setup.exe SHA256`.

To check that a file was built by this repository's release workflow (build provenance),
with the free GitHub CLI:

```powershell
gh attestation verify .\Frostcap_X.Y.Z_x64-setup.exe --repo <owner>/<repo>
```

A checksum only proves the file matches what was uploaded to the release. Download checksums
and files from the official GitHub Releases page only. Once releases are signed, also check the
signature: right-click the file -> Properties -> Digital Signatures.
