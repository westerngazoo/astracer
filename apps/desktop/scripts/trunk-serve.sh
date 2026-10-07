#!/usr/bin/env bash
set -euo pipefail
# Cursor/sandbox often sets NO_COLOR=1; trunk 0.21 treats that as --no-color=1 (invalid).
unset NO_COLOR FORCE_COLOR
exec trunk serve "$@"
