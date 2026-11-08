#!/usr/bin/env bash
set -euo pipefail

LIVEWALK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

export PATH="${HOME}/.cargo/bin:${LIVEWALK_ROOT}/target/release:${PATH}"

usage() {
  echo "usage: $(basename "$0") list" >&2
  echo "       $(basename "$0") clean [--legacy-only] [--older-than DAYS]" >&2
  exit 1
}

cmd="${1:-}"
shift || true

case "${cmd}" in
  list)
    exec lcw cache list
    ;;
  clean)
    extra=()
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --legacy-only)
          extra+=(--legacy-only)
          shift
          ;;
        --older-than)
          [[ $# -ge 2 ]] || usage
          extra+=(--older-than "$2")
          shift 2
          ;;
        *)
          usage
          ;;
      esac
    done
    lcw cache clean "${extra[@]}"
    read -r -p "Delete matching Livewalk cache buckets? [y/N] " ans
    case "${ans}" in
      y|Y|yes|YES)
        exec lcw cache clean --yes "${extra[@]}"
        ;;
      *)
        echo "cancelled"
        exit 0
        ;;
    esac
    ;;
  *)
    usage
    ;;
esac
