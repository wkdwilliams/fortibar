# Builds dist\FortiBar-windows-x64.zip: FortiBar.exe plus a bundled openconnect.
#
# Needs Rust (https://rustup.rs) and MSYS2 (https://www.msys2.org) with:
#   pacman -S mingw-w64-x86_64-openconnect unzip
#
#   .\scripts\build-windows.ps1 [-Msys2 C:\msys64]
param([string]$Msys2 = "C:\msys64")
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$env:CARGO_HOME = "$root\.cargo"   # crate downloads stay in this folder
cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$dist = "$root\dist"
$out = "$dist\FortiBar"
Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$out\openconnect" | Out-Null
Copy-Item "$root\target\release\fortibar.exe" "$out\FortiBar.exe"

$env:MSYSTEM = "MINGW64"
$env:CHERE_INVOKING = "1"
& "$Msys2\usr\bin\bash.exe" -lc 'bash "$(cygpath -u "$0")" "$(cygpath -u "$1")"' `
    "$root\scripts\bundle-openconnect.sh" "$out\openconnect"
if ($LASTEXITCODE -ne 0) { throw "bundling openconnect failed" }

Compress-Archive -Path $out -DestinationPath "$dist\FortiBar-windows-x64.zip"
Write-Host "Built $dist\FortiBar-windows-x64.zip"
