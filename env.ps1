# Dot-source this in PowerShell to use the repo-local Rust toolchain (keeps C: clean):
#   . .\env.ps1
$root = $PSScriptRoot
$env:RUSTUP_HOME = Join-Path $root '.toolchain\rustup'
$env:CARGO_HOME  = Join-Path $root '.toolchain\cargo'
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
$env:PATH = (Join-Path $env:CARGO_HOME 'bin') + ';' + $env:PATH
