<#
  Builds the Frostcap NSIS installer and portable exe locally without writing to C:.

  The Tauri bundler downloads NSIS and the WebView2 bootstrapper (about 10 MB) into the Windows
  "Local AppData" known folder. Tauri resolves that folder with the Windows API, NOT from the
  LOCALAPPDATA variable, so redirecting the variable does not move it. This script therefore
  redirects LOCALAPPDATA/APPDATA/TEMP/TMP to .toolchain/home for this process (npm cache and
  temp files stay on the repo drive) and, after the build, deletes the real Local AppData
  "tauri" folder if the script itself created it. Nothing in the user
  or system environment is changed.

  Usage (from the repo root):  .\scripts\build-installer.ps1
  Output: target\release\bundle\nsis\Frostcap_<version>_x64-setup.exe, target\release\frostcap.exe
  The installer is NOT run.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $root 'env.ps1')

$home2 = Join-Path $root '.toolchain\home'
foreach ($d in 'local', 'roaming', 'temp') { New-Item -ItemType Directory -Force (Join-Path $home2 $d) | Out-Null }
$env:LOCALAPPDATA = Join-Path $home2 'local'
$env:APPDATA = Join-Path $home2 'roaming'
$env:TEMP = Join-Path $home2 'temp'
$env:TMP = $env:TEMP

$overlay = Join-Path $env:TEMP 'frostcap-installer.conf.json'
'{ "build": { "beforeBuildCommand": "" }, "bundle": { "active": true, "targets": ["nsis"] } }' | Set-Content -Encoding ascii $overlay

# Known-folder path (what Tauri uses), captured before the redirect matters.
$realTauri = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'tauri'
$hadTauri = Test-Path $realTauri

Push-Location $root
try {
  # The overlay empties beforeBuildCommand (its `npm ci` would delete the running Tauri CLI),
  # so install (no scripts) and build the UI here instead.
  npm --prefix ui ci --ignore-scripts
  if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
  npm --prefix ui run build
  if ($LASTEXITCODE -ne 0) { throw 'ui build failed' }
  & (Join-Path $root 'ui\node_modules\.bin\tauri.cmd') build --bundles nsis --config $overlay
  if ($LASTEXITCODE -ne 0) { throw "tauri build failed ($LASTEXITCODE)" }
  Get-ChildItem target\release\bundle\nsis\*.exe, target\release\frostcap.exe |
    ForEach-Object { '{0}  {1:N0} bytes' -f $_.FullName, $_.Length }
} finally {
  Pop-Location
  if (-not $hadTauri -and (Test-Path $realTauri)) { Remove-Item -Recurse -Force $realTauri }
}
