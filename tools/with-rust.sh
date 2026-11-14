#!/bin/sh
# Runs a command with the project-local Rust toolchain in ../.rust when there is one, else with
# the Rust already installed (rustup). The system PATH and shell profiles are not changed.
# Example: tools/with-rust.sh npm run tauri dev
set -e
RUST_ROOT="$(cd "$(dirname "$0")/.." && pwd)/.rust"
if [ -d "$RUST_ROOT/cargo/bin" ]; then
    export RUSTUP_HOME="$RUST_ROOT/rustup"
    export CARGO_HOME="$RUST_ROOT/cargo"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
# libopus (Opus in file transcription) builds with CMake: without one on PATH, take the CMake
# of the official macOS app.
if ! command -v cmake >/dev/null && [ -x /Applications/CMake.app/Contents/bin/cmake ]; then
    export PATH="/Applications/CMake.app/Contents/bin:$PATH"
fi
exec "$@"
