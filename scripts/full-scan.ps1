# Full pre-ship verification sweep (see .claude/commands/full-scan.md).
# Usage: pwsh -File scripts/full-scan.ps1   (writes a summary at the end; exit 1 if any step fails)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $root 'env.ps1')
Set-Location $root
$results = [ordered]@{}
function Step($name, [scriptblock]$cmd) {
  Write-Host "`n===== $name" -ForegroundColor Cyan
  & $cmd
  $results[$name] = ($LASTEXITCODE -eq 0)
  Write-Host "----- $name exit $LASTEXITCODE"
}
Step 'rust tests'          { cargo test --workspace 2>&1 | Select-String 'test result|FAILED|panicked|error(\[|:)' }
Step 'rust desktop tests'  { cargo test --workspace -- --ignored 2>&1 | Select-String 'test result|FAILED|panicked|error(\[|:)' }
Step 'ui tests'            { npm --prefix ui test --silent 2>&1 | Select-String 'Test Files|Tests |FAIL' }
Step 'typecheck'           { npm --prefix ui run typecheck --silent }
Step 'fmt'                 { cargo fmt --all --check }
Step 'clippy'              { cargo clippy --workspace --all-targets -- -D warnings 2>&1 | Select-String 'warning|error|Finished' }
Step 'cargo deny'          { cargo deny check 2>&1 | Select-String ' ok|FAILED|error\[' }
Step 'third-party check'   { node scripts/gen-third-party.mjs --check }
Step 'ui build'            { npm --prefix ui run build --silent 2>&1 | Select-String 'gzip|error|built in' }
Step 'release build'       { cargo build --release -p frostcap 2>&1 | Select-String 'warning|error|Finished' }
if (Test-Path target\release\frostcap.exe) { Write-Host ("frostcap.exe bytes: " + (Get-Item target\release\frostcap.exe).Length) }
Write-Host "`n===== SUMMARY"
$failed = 0
foreach ($k in $results.Keys) { $ok = $results[$k]; if (-not $ok) { $failed++ }; Write-Host ("{0,-20} {1}" -f $k, $(if ($ok) { 'PASS' } else { 'FAIL' })) }
exit [int]($failed -gt 0)
