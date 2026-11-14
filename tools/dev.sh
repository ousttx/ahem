#!/bin/sh
# Starts the app in development mode (with the project-local Rust toolchain if there is one).
set -e
cd "$(dirname "$0")/.."
exec tools/with-rust.sh npm run tauri dev
