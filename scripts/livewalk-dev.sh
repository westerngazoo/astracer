#!/usr/bin/env bash
set -euo pipefail

LIVEWALK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export LIVEWALK_ROOT

unset NO_COLOR FORCE_COLOR

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

export PATH="${HOME}/.cargo/bin:${LIVEWALK_ROOT}/target/release:${PATH}"

TAURI_DIR="${LIVEWALK_ROOT}/apps/desktop/src-tauri"
FRONTEND_DIR="${LIVEWALK_ROOT}/apps/desktop/frontend"
LCW_BIN="${LIVEWALK_ROOT}/target/release/lcw"

REBUILD_CLI=0
MODE="dev"
CLI_ARGS=()

semantic_enabled() {
  [[ "${LIVEWALK_SEMANTIC:-}" == "1" ]]
}

cli_cargo_features() {
  if semantic_enabled; then
    printf '%s' semantic
  fi
}

ensure_wasm_target() {
  rustup target add wasm32-unknown-unknown
}

build_lcw_cli() {
  local -a cmd=(cargo build -p lcw-cli --bin lcw --release --locked)
  local feats
  feats="$(cli_cargo_features)"
  if [[ -n "${feats}" ]]; then
    cmd+=(--features "${feats}")
  fi
  (cd "${LIVEWALK_ROOT}" && "${cmd[@]}")
}

ensure_lcw_cli() {
  if [[ "${REBUILD_CLI}" == "1" ]] || [[ ! -x "${LCW_BIN}" ]]; then
    build_lcw_cli
  fi
}

run_dev() {
  ensure_wasm_target
  ensure_lcw_cli
  local -a tauri_extra=()
  if semantic_enabled; then
    tauri_extra=(-- --features semantic)
  fi
  cd "${TAURI_DIR}"
  exec cargo tauri dev "${tauri_extra[@]}"
}

run_cli() {
  ensure_lcw_cli
  exec "${LCW_BIN}" "${CLI_ARGS[@]}"
}

run_build_all() {
  ensure_wasm_target
  (cd "${LIVEWALK_ROOT}" && cargo build --workspace --locked)
  REBUILD_CLI=1
  ensure_lcw_cli
  (cd "${FRONTEND_DIR}" && trunk build)
}

run_rebuild() {
  ensure_wasm_target
  REBUILD_CLI=1
  build_lcw_cli
  (cd "${LIVEWALK_ROOT}" && cargo build --workspace --locked)
  (cd "${FRONTEND_DIR}" && trunk build)
}

usage() {
  cat <<'EOF'
Usage: livewalk-dev.sh [--rebuild-cli] [dev|cli|build-all|rebuild] [args...]

  dev          Run the Tauri desktop app (default).
  cli          Run the release lcw binary (e.g. cli analyze /path/to/repo).
  build-all    Build workspace, release lcw, and trunk-build the frontend.
  rebuild      Force rebuild lcw release, workspace, and frontend bundle.

Environment:
  LIVEWALK_SEMANTIC=1   Enable the semantic Cargo feature (tauri dev / lcw build).

Flags:
  --rebuild-cli         Rebuild the release lcw binary even if it exists.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rebuild-cli)
      REBUILD_CLI=1
      shift
      ;;
    dev | build-all | rebuild)
      MODE="$1"
      shift
      break
      ;;
    cli)
      MODE="cli"
      shift
      CLI_ARGS=("$@")
      break
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      if [[ "${MODE}" == "dev" ]]; then
        usage >&2
        exit 2
      fi
      break
      ;;
  esac
done

case "${MODE}" in
  dev)
    run_dev
    ;;
  cli)
    run_cli
    ;;
  build-all)
    run_build_all
    ;;
  rebuild)
    run_rebuild
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
