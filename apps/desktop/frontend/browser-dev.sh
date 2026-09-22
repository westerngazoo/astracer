#!/usr/bin/env sh
#
# Browser dev mode, in one command: build the wasm UI, analyze a repository into
# a graph fixture, and serve both. No Tauri shell, no webview toolchain, no GPU
# hardware needed — the frontend falls back to `transport::FixtureTransport`
# whenever `window.__TAURI__` is absent.
#
#   ./browser-dev.sh [repository-to-analyze] [port]
#
# Defaults to analyzing this repository on port 8765.
#
# The bundle is built into `target/browser-dev/` rather than `dist/`, on purpose:
# `dist/` belongs to the Tauri build (`generate_context!` embeds it), and a
# browser session that builds into it would fight a `tauri dev` over the same
# directory. Serving a directory this script has just built also means a stale
# or placeholder page can never be what you end up looking at.

set -eu

FRONTEND_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$FRONTEND_DIR/../../.." && pwd)
TARGET_REPO=${1:-$REPO_ROOT}
PORT=${2:-8765}
OUT="$FRONTEND_DIR/target/browser-dev"
LCW="$REPO_ROOT/target/debug/lcw"

# Both prerequisites are checked up front. Without the wasm target `trunk`
# starts happily and then fails deep inside a cargo build, where the real cause
# ("the wasm32-unknown-unknown target may not be installed") is one line among
# several screens of E0463 from every dependency in the tree.

if ! command -v trunk >/dev/null 2>&1; then
    echo "error: 'trunk' is not installed. Install it with:" >&2
    echo "    cargo install trunk --locked" >&2
    exit 1
fi

# Only meaningful under rustup; a distro or nix toolchain manages targets its
# own way, so there the build is left to speak for itself.
if command -v rustup >/dev/null 2>&1 &&
    ! rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown; then
    echo "error: the wasm32-unknown-unknown target is not installed for the active" >&2
    echo "toolchain ($(rustup show active-toolchain 2>/dev/null || echo unknown))." >&2
    echo "Install it with:" >&2
    echo "    rustup target add wasm32-unknown-unknown" >&2
    exit 1
fi

if [ ! -d "$TARGET_REPO" ]; then
    echo "error: no such directory: $TARGET_REPO" >&2
    exit 1
fi

echo "==> building the analyzer"
cargo build --manifest-path "$REPO_ROOT/Cargo.toml" -p lcw-cli --features viewer

echo "==> building the wasm UI into $OUT"
( cd "$FRONTEND_DIR" && trunk build --release --dist "$OUT" )

echo "==> analyzing $TARGET_REPO"
"$LCW" analyze "$TARGET_REPO" --format view -o "$OUT/fixture.json"

echo
echo "    open http://127.0.0.1:$PORT/"
echo "    type 'fixture.json' in the path box, press Analyze, then Entry"
echo
exec python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$OUT"
