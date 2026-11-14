# Fetches the official prebuilt sherpa-onnx libraries for Windows x64 into runtimes\sherpa-onnx
# and checks the pinned SHA-256. The build reads them from there through
# SHERPA_ONNX_ARCHIVE_DIR (.cargo\config.toml) instead of downloading them unchecked.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools\fetch-sherpa-onnx.ps1

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Must match the `sherpa-onnx` crate version in src-tauri\Cargo.toml.
$Version = '2.0.0'
$Name = "sherpa-onnx-v$Version-win-x64-static-MT-Release-lib.tar.bz2"
$Url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$Version/$Name"
# SHA-256 published by GitHub for the release asset (checked 2026-10-24).
$Sha256 = '13cbef1926eea3bc01c8eb59bbfa3806f02f904a76664c215c4e20989866704a'

$App = Split-Path -Parent $PSScriptRoot
$Dir = Join-Path $App 'runtimes\sherpa-onnx'
$Archive = Join-Path $Dir $Name

function Get-Sha256([string]$Path) { (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower() }

if ((Test-Path $Archive) -and (Get-Sha256 $Archive) -eq $Sha256) {
    Write-Host "Already in place: $Archive"
    exit 0
}
New-Item -ItemType Directory -Force $Dir | Out-Null
Invoke-WebRequest -Uri $Url -OutFile "$Archive.part" -UseBasicParsing
$actual = Get-Sha256 "$Archive.part"
if ($actual -ne $Sha256) {
    Remove-Item "$Archive.part"
    throw "sherpa-onnx archive SHA-256 mismatch: $actual"
}
Move-Item -Force "$Archive.part" $Archive
Write-Host "Fetched and verified: $Archive"
