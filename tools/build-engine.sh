#!/bin/sh
# Builds whisper.cpp (whisper-server, whisper-cli, whisper-bench) for macOS from the pinned
# official source, the same as tools/build-engine.ps1 does for Windows. There is no official
# macOS whisper-server. Flags follow the official Apple build (build-xcframework.sh):
# Metal with the shader library embedded, BLAS through Accelerate, no OpenMP; libraries are
# linked statically, so each program is a single file.
#
# Needs: Xcode Command Line Tools and CMake (tools/with-rust.sh adds the one of CMake.app).
# Usage: tools/with-rust.sh tools/build-engine.sh
set -eu

# Must match ENGINE_VERSION in src-tauri/src/engines/whisper_server.rs.
VERSION=1.9.5
COMMIT=d1be6fde11ac6e0407606b4e42fe72d34add8037
SOURCE_URL="https://github.com/ggml-org/whisper.cpp/archive/refs/tags/v$VERSION.tar.gz"
# SHA-256 of the archive as downloaded on 2026-10-23 (GitHub publishes no checksum for archives).
SOURCE_SHA256=ff1a9053feb509ff9d7729703355541ae9690073a6b1c40eb692c962e0dc1720
# The official script's minimum for macOS.
MACOS_MIN=13.3

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) ARCH=aarch64; CMAKE_ARCH=arm64 ;;
    Darwin-x86_64) ARCH=x86_64; CMAKE_ARCH=x86_64 ;;
    *) echo "Unsupported system: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
command -v cmake >/dev/null || { echo "CMake is not on PATH" >&2; exit 1; }

APP="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$APP/runtimes/build"
ARCHIVE="$WORK/downloads/whisper.cpp-v$VERSION.tar.gz"
SOURCE="$WORK/whisper.cpp-$VERSION"
# One per version: CMake refuses a cache made from another version's sources.
BUILD_DIR="$WORK/w-macos-$VERSION"
OUT="$APP/runtimes/whisper-$VERSION-macos-$ARCH"

sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }

# 1. Source archive, verified before it is kept: a wrong one never stays to block the next run.
if ! { [ -f "$ARCHIVE" ] && [ "$(sha256 "$ARCHIVE")" = "$SOURCE_SHA256" ]; }; then
    mkdir -p "$(dirname "$ARCHIVE")"
    curl -fL --proto '=https' -o "$ARCHIVE.part" "$SOURCE_URL"
    actual="$(sha256 "$ARCHIVE.part")"
    if [ "$actual" != "$SOURCE_SHA256" ]; then
        rm -f "$ARCHIVE.part"
        echo "Source archive SHA-256 mismatch: $actual" >&2
        exit 1
    fi
    mv -f "$ARCHIVE.part" "$ARCHIVE"
fi
# Unpacked aside and moved into place whole: an interrupted run leaves no half a source tree.
if [ ! -d "$SOURCE" ]; then
    rm -rf "$WORK/unpack.part"
    mkdir -p "$WORK/unpack.part"
    tar -xzf "$ARCHIVE" -C "$WORK/unpack.part"
    mv "$WORK/unpack.part/whisper.cpp-$VERSION" "$SOURCE"
    rm -rf "$WORK/unpack.part"
fi

# 2. Configure and build.
cmake -S "$SOURCE" -B "$BUILD_DIR" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_OSX_ARCHITECTURES="$CMAKE_ARCH" \
    -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOS_MIN" \
    -DBUILD_SHARED_LIBS=OFF \
    -DGGML_NATIVE=OFF \
    -DGGML_METAL=ON \
    -DGGML_METAL_EMBED_LIBRARY=ON \
    -DGGML_METAL_USE_BF16=ON \
    -DGGML_BLAS_DEFAULT=ON \
    -DGGML_OPENMP=OFF \
    -DWHISPER_BUILD_EXAMPLES=ON \
    -DWHISPER_BUILD_SERVER=ON \
    -DWHISPER_SDL2=OFF \
    -DWHISPER_BUILD_TESTS=OFF
cmake --build "$BUILD_DIR" --config Release -j "$(sysctl -n hw.ncpu)" \
    --target whisper-server whisper-cli whisper-bench

# 3. Collect the programs, record what was built.
# Put together aside and moved into place whole: an interrupted run leaves no half an engine.
rm -rf "$OUT.part"
mkdir -p "$OUT.part"
for program in whisper-server whisper-cli whisper-bench; do
    cp "$BUILD_DIR/bin/$program" "$OUT.part/"
done
{
    printf '{\n  "version": "%s",\n  "commit": "%s",\n  "source_sha256": "%s",\n' \
        "$VERSION" "$COMMIT" "$SOURCE_SHA256"
    printf '  "metal": true,\n  "files": {\n'
    first=1
    for file in $(ls "$OUT.part" | sort); do
        [ "$file" = manifest.json ] && continue
        [ $first -eq 1 ] || printf ',\n'
        printf '    "%s": "%s"' "$file" "$(sha256 "$OUT.part/$file")"
        first=0
    done
    printf '\n  }\n}\n'
} > "$OUT.part/manifest.json"
rm -rf "$OUT"
mv "$OUT.part" "$OUT"
echo "Engine built: $OUT"
