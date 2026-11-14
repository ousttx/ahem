# Fetches the official llama.cpp build for Windows x64 (CPU variants and Vulkan, chosen at
# runtime) into runtimes\llama-<build>-windows-x86_64 and checks the pinned SHA-256.
# The app runs llama-server from there for llama.cpp models.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools\fetch-llama-cpp.ps1

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Must match ENGINE_BUILD in src-tauri\src\engines\llama_server.rs.
$Build = 'b11871'
$Name = "llama-$Build-bin-win-vulkan-x64.zip"
$Url = "https://github.com/ggml-org/llama.cpp/releases/download/$Build/$Name"
# SHA-256 published by GitHub for the release asset (checked 2026-10-25).
$Sha256 = '34a0366b9d9d22d09a58dd2958570f2549568067f3f15de771fc1909e888dd80'

$App = Split-Path -Parent $PSScriptRoot
$Archive = Join-Path $App "runtimes\build\downloads\$Name"
$Out = Join-Path $App "runtimes\llama-$Build-windows-x86_64"

function Get-Sha256([string]$Path) { (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower() }

if (Test-Path (Join-Path $Out 'llama-server.exe')) {
    Write-Host "Already in place: $Out"
    exit 0
}
if (-not ((Test-Path $Archive) -and (Get-Sha256 $Archive) -eq $Sha256)) {
    New-Item -ItemType Directory -Force (Split-Path $Archive) | Out-Null
    Invoke-WebRequest -Uri $Url -OutFile "$Archive.part" -UseBasicParsing
    $actual = Get-Sha256 "$Archive.part"
    if ($actual -ne $Sha256) {
        Remove-Item "$Archive.part"
        throw "llama.cpp archive SHA-256 mismatch: $actual"
    }
    Move-Item -Force "$Archive.part" $Archive
}
# Unpacked next to $Out first: an interrupted unpacking never leaves a folder that looks done.
$Part = "$Out.part"
if (Test-Path $Part) { Remove-Item -Recurse -Force $Part }
Expand-Archive -Path $Archive -DestinationPath $Part -Force
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
Move-Item $Part $Out
Write-Host "Fetched and verified: $Out"
