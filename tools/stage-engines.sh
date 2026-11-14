#!/bin/sh
# Collects what the installer carries beside the app into src-tauri/engines/: whisper-server and
# llama-server with the libraries it loads. Run it after tools/build-engine and
# tools/fetch-llama-cpp, before building the installer (see docs/ARCHITECTURE.md, Updates).
# Usage: tools/stage-engines.sh
set -eu

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) ARCH=aarch64 ;;
    Darwin-x86_64) ARCH=x86_64 ;;
    *) echo "Unsupported system: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
APP="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$APP/src-tauri/engines"

# The one build of the engine for this system: a folder of runtimes/ that matches.
only() {
    set -- "$APP"/runtimes/$1
    [ "$#" -eq 1 ] && [ -d "$1" ] || { echo "Expected one folder $1 (run the tools/ scripts first)" >&2; exit 1; }
    echo "$1"
}
WHISPER="$(only "whisper-*-macos-$ARCH")"
LLAMA="$(only "llama-*-macos-$ARCH")"

rm -rf "$OUT"
mkdir -p "$OUT/whisper" "$OUT/llama"
cp "$WHISPER/whisper-server" "$OUT/whisper/"

# llama-server and every library it names, and theirs, by the name it asks for.
cp "$LLAMA/llama-server" "$OUT/llama/"
[ -f "$LLAMA/LICENSE" ] && cp "$LLAMA/LICENSE" "$OUT/llama/LICENSE"
queue="llama-server"
while [ -n "$queue" ]; do
    current="${queue%%
*}"
    case "$queue" in *"
"*) queue="${queue#*
}" ;; *) queue="" ;; esac
    for lib in $(otool -L "$OUT/llama/$current" | awk '/@rpath\//{sub(".*@rpath/","",$1); print $1}'); do
        if [ ! -e "$OUT/llama/$lib" ]; then
            cp -L "$LLAMA/$lib" "$OUT/llama/$lib"
            queue="$queue
$lib"
            queue="${queue#
}"
        fi
    done
done

# The bundle is signed ad hoc; so are the programs it carries, as they were built.
for file in "$OUT"/whisper/whisper-server "$OUT"/llama/llama-server "$OUT"/llama/*.dylib; do
    codesign --force --sign - "$file" 2>/dev/null
done
echo "Staged in $OUT:"
du -sh "$OUT/whisper" "$OUT/llama"
