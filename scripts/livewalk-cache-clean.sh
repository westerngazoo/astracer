#!/usr/bin/env bash
set -euo pipefail

LIVEWALK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

export PATH="${HOME}/.cargo/bin:${LIVEWALK_ROOT}/target/release:${PATH}"

exec lcw cache clean --all --yes "$@"
