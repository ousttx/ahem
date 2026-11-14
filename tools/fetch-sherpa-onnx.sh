#!/bin/sh
# Fetches the official prebuilt sherpa-onnx static libraries for macOS into runtimes/sherpa-onnx
# and checks the pinned SHA-256. The build reads them from there through
# SHERPA_ONNX_ARCHIVE_DIR (.cargo/config.toml) instead of downloading them unchecked.
# The archive name is the one sherpa-onnx-sys expects for a static macOS build.
#
# Usage: tools/fetch-sherpa-onnx.sh
set -eu

# Must match the `sherpa-onnx` crate version in src-tauri/Cargo.toml.
VERSION=2.0.0
# SHA-256 published by GitHub for the release assets (checked 2026-11-05).
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)
        NAME="sherpa-onnx-v$VERSION-osx-arm64-static-lib.tar.bz2"
        SHA256=5e5bdce1d7fc834857278d8bb26f878de6a55c7085e589ed1e41a00baca643ec ;;
    Darwin-x86_64)
        NAME="sherpa-onnx-v$VERSION-osx-x64-static-lib.tar.bz2"
        SHA256=a6096bb89cb92e071f5fca4a9a869f7251587b707dd52852adc1a8ad6ace9051 ;;
    *) echo "Unsupported system: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/v$VERSION/$NAME"

APP="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$APP/runtimes/sherpa-onnx"
ARCHIVE="$DIR/$NAME"

sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }

if [ -f "$ARCHIVE" ] && [ "$(sha256 "$ARCHIVE")" = "$SHA256" ]; then
    echo "Already in place: $ARCHIVE"
    exit 0
fi
mkdir -p "$DIR"
curl -fL --proto '=https' -o "$ARCHIVE.part" "$URL"
actual="$(sha256 "$ARCHIVE.part")"
if [ "$actual" != "$SHA256" ]; then
    rm -f "$ARCHIVE.part"
    echo "sherpa-onnx archive SHA-256 mismatch: $actual" >&2
    exit 1
fi
mv -f "$ARCHIVE.part" "$ARCHIVE"
echo "Fetched and verified: $ARCHIVE"
