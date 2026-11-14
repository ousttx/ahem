# Collects what the installer carries beside the app into src-tauri\engines: whisper-server and
# llama-server with their libraries. Run it after tools\build-engine.ps1 and
# tools\fetch-llama-cpp.ps1, before building the installer (see docs\ARCHITECTURE.md, Updates).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools\stage-engines.ps1
$ErrorActionPreference = 'Stop'
$App = Split-Path -Parent $PSScriptRoot
$Out = Join-Path $App 'src-tauri\engines'

# The one build of the engine for this system: a folder of runtimes that matches.
function Find-Only($Pattern) {
    $found = @(Get-ChildItem -Directory (Join-Path $App 'runtimes') -Filter $Pattern)
    if ($found.Count -ne 1) { throw "Expected one folder $Pattern in runtimes (run the tools scripts first)" }
    $found[0].FullName
}
$Whisper = Find-Only 'whisper-*-windows-x86_64'
$Llama = Find-Only 'llama-*-windows-x86_64'

if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
New-Item -ItemType Directory -Force (Join-Path $Out 'whisper'), (Join-Path $Out 'llama') | Out-Null

# whisper-server and its backend libraries; not the command-line tools beside them.
Get-ChildItem $Whisper -File |
    Where-Object { $_.Name -notin 'whisper-cli.exe', 'whisper-bench.exe', 'manifest.json' } |
    Copy-Item -Destination (Join-Path $Out 'whisper')
# llama-server, the libraries beside it and the license.
Get-ChildItem $Llama -File |
    Where-Object { $_.Name -eq 'llama-server.exe' -or $_.Extension -eq '.dll' -or $_.Name -like 'LICENSE*' } |
    Copy-Item -Destination (Join-Path $Out 'llama')
if (-not (Test-Path (Join-Path $Out 'whisper\whisper-server.exe'))) { throw 'whisper-server.exe is missing' }
if (-not (Test-Path (Join-Path $Out 'llama\llama-server.exe'))) { throw 'llama-server.exe is missing' }
Write-Host "Staged in $Out"
