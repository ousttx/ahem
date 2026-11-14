# Builds whisper.cpp (whisper-server, whisper-cli, whisper-bench) for Windows x64 from the
# pinned official source. Flags follow the official Windows release build: CPU variants and the
# Vulkan backend are separate DLLs loaded at runtime (GGML_BACKEND_DL), so one build runs on any
# x64 CPU and uses Vulkan when the GPU driver provides it.
#
# Needs: Visual Studio Build Tools (C++ workload with CMake) and, unless -NoVulkan, the Vulkan SDK.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools\build-engine.ps1 [-NoVulkan]
param([switch]$NoVulkan)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Must match ENGINE_VERSION in src-tauri/src/engines/whisper_server.rs.
$Version = '1.9.5'
$Commit = 'd1be6fde11ac6e0407606b4e42fe72d34add8037'
$SourceUrl = "https://github.com/ggml-org/whisper.cpp/archive/refs/tags/v$Version.tar.gz"
# SHA-256 of the archive as downloaded on 2026-10-23 (GitHub publishes no checksum for archives).
$SourceSha256 = 'ff1a9053feb509ff9d7729703355541ae9690073a6b1c40eb692c962e0dc1720'

$App = Split-Path -Parent $PSScriptRoot
$Work = Join-Path $App 'runtimes\build'
$Archive = Join-Path $Work "downloads\whisper.cpp-v$Version.tar.gz"
$Source = Join-Path $Work "whisper.cpp-$Version"
# Short on purpose: the Vulkan shader generator's nested CMake checks exceed MAX_PATH (260)
# under a longer build directory.
$BuildDir = Join-Path $Work 'w'
# What the build directory was made for: CMake refuses a cache made from another version's
# sources, and a build without Vulkan must not pick up the Vulkan library of an earlier one.
$Variant = if ($NoVulkan) { "$Version cpu" } else { "$Version vulkan" }
$Out = Join-Path $App "runtimes\whisper-$Version-windows-x86_64"

function Get-Sha256([string]$Path) { (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower() }

# 1. Source archive, verified before it is kept: a wrong one never stays to block the next run.
if (-not ((Test-Path $Archive) -and (Get-Sha256 $Archive) -eq $SourceSha256)) {
    New-Item -ItemType Directory -Force (Split-Path $Archive) | Out-Null
    Invoke-WebRequest -Uri $SourceUrl -OutFile "$Archive.part" -UseBasicParsing
    $actual = Get-Sha256 "$Archive.part"
    if ($actual -ne $SourceSha256) {
        Remove-Item "$Archive.part"
        throw "Source archive SHA-256 mismatch: $actual"
    }
    Move-Item -Force "$Archive.part" $Archive
}
# Unpacked aside and moved into place whole: an interrupted run leaves no half a source tree.
if (-not (Test-Path $Source)) {
    $unpack = Join-Path $Work 'unpack.part'
    if (Test-Path $unpack) { Remove-Item -Recurse -Force $unpack }
    New-Item -ItemType Directory -Force $unpack | Out-Null
    tar -xzf $Archive -C $unpack
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract the source archive' }
    Move-Item (Join-Path $unpack "whisper.cpp-$Version") $Source
    Remove-Item -Recurse -Force $unpack
}

# 2. Toolchain: MSVC environment from Build Tools, Vulkan SDK from the machine environment
#    (a shell started before the SDK was installed does not see VULKAN_SDK yet).
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'Visual Studio Build Tools with the C++ workload are not installed' }
$devCmd = Join-Path $vs 'Common7\Tools\VsDevCmd.bat'

$vulkan = 'OFF'
if (-not $NoVulkan) {
    $sdk = $env:VULKAN_SDK
    if (-not $sdk) { $sdk = [Environment]::GetEnvironmentVariable('VULKAN_SDK', 'Machine') }
    if (-not $sdk -or -not (Test-Path $sdk)) { throw 'Vulkan SDK is not installed (or use -NoVulkan)' }
    $env:VULKAN_SDK = $sdk
    $env:PATH = (Join-Path $sdk 'Bin') + ';' + $env:PATH
    $vulkan = 'ON'
}

# 3. Configure and build, in a fresh build directory when it was made for another version or
#    variant.
$Stamp = Join-Path $BuildDir 'ahem-variant.txt'
if ((Test-Path $BuildDir) -and -not ((Test-Path $Stamp) -and (Get-Content $Stamp -Raw).Trim() -eq $Variant)) {
    Remove-Item -Recurse -Force $BuildDir
}
New-Item -ItemType Directory -Force $BuildDir | Out-Null
Set-Content -Path $Stamp -Value $Variant
$configure = @(
    "cmake -S `"$Source`" -B `"$BuildDir`" -G Ninja",
    '-DCMAKE_BUILD_TYPE=Release',
    '-DBUILD_SHARED_LIBS=ON',
    '-DGGML_BACKEND_DL=ON',
    '-DGGML_CPU_ALL_VARIANTS=ON',
    '-DGGML_NATIVE=OFF',
    "-DGGML_VULKAN=$vulkan",
    '-DWHISPER_SDL2=OFF',
    '-DWHISPER_BUILD_TESTS=OFF'
) -join ' '
cmd /c "call `"$devCmd`" -arch=amd64 -host_arch=amd64 -no_logo && $configure && cmake --build `"$BuildDir`" --config Release"
if ($LASTEXITCODE -ne 0) { throw "Build failed ($LASTEXITCODE)" }

# 4. Collect the programs and libraries, record what was built. Put together aside and moved
#    into place whole: an interrupted run leaves no half an engine.
$Part = "$Out.part"
if (Test-Path $Part) { Remove-Item -Recurse -Force $Part }
New-Item -ItemType Directory -Force $Part | Out-Null
Get-ChildItem (Join-Path $BuildDir 'bin') -File | Where-Object { $_.Extension -in '.exe', '.dll' } |
    Copy-Item -Destination $Part
$files = [ordered]@{}
foreach ($file in Get-ChildItem $Part -File | Sort-Object Name) { $files[$file.Name] = Get-Sha256 $file.FullName }
$manifest = [ordered]@{
    version = $Version
    commit = $Commit
    source_sha256 = $SourceSha256
    vulkan = ($vulkan -eq 'ON')
    files = $files
}
$manifest | ConvertTo-Json -Depth 3 | Out-File -Encoding utf8 (Join-Path $Part 'manifest.json')
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
Move-Item $Part $Out
Write-Host "Engine built: $Out"
