#!/bin/sh
# Fetches the official llama.cpp build for macOS (Metal on Apple silicon) into
# runtimes/llama-<build>-macos-<arch> and checks the pinned SHA-256.
# The app runs llama-server from there for llama.cpp models.
#
# Usage: tools/fetch-llama-cpp.sh
set -eu

# Must match ENGINE_BUILD in src-tauri/src/engines/llama_server.rs.
BUILD=b11871
# SHA-256 published by GitHub for the release assets (checked 2026-11-05).
# ARCH is Rust's std::env::consts::ARCH, which names the folder the app looks in.
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)
        NAME="llama-$BUILD-bin-macos-arm64.tar.gz"; ARCH=aarch64
        SHA256=389977376e89be4708e3e20c2f0c70b7252d15f5b848e754a26cf5fb06748d7c ;;
    Darwin-x86_64)
        NAME="llama-$BUILD-bin-macos-x64.tar.gz"; ARCH=x86_64
        SHA256=f71341bfd7234e52b69c0717ab7e5f1a044d5df1afef5caa6e94ee21fc1cc40b ;;
    *) echo "Unsupported system: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
URL="https://github.com/ggml-org/llama.cpp/releases/download/$BUILD/$NAME"

APP="$(cd "$(dirname "$0")/.." && pwd)"
ARCHIVE="$APP/runtimes/build/downloads/$NAME"
OUT="$APP/runtimes/llama-$BUILD-macos-$ARCH"

sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }

if [ -x "$OUT/llama-server" ]; then
    echo "Already in place: $OUT"
    exit 0
fi
if ! { [ -f "$ARCHIVE" ] && [ "$(sha256 "$ARCHIVE")" = "$SHA256" ]; }; then
    mkdir -p "$(dirname "$ARCHIVE")"
    curl -fL --proto '=https' -o "$ARCHIVE.part" "$URL"
    actual="$(sha256 "$ARCHIVE.part")"
    if [ "$actual" != "$SHA256" ]; then
        rm -f "$ARCHIVE.part"
        echo "llama.cpp archive SHA-256 mismatch: $actual" >&2
        exit 1
    fi
    mv -f "$ARCHIVE.part" "$ARCHIVE"
fi
# The archive keeps everything in a llama-<build>/ folder; the app looks for llama-server
# directly in $OUT. The programs find their libraries next to them (@loader_path). Unpacked
# next to $OUT first: an interrupted unpacking never leaves a folder that looks done.
rm -rf "$OUT.part"
mkdir -p "$OUT.part"
tar -xzf "$ARCHIVE" -C "$OUT.part" --strip-components 1
rm -rf "$OUT"
mv "$OUT.part" "$OUT"
echo "Fetched and verified: $OUT"
