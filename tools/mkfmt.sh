#!/bin/sh
# Compatibility entry point; the Python implementation avoids GNU-only
# `timeout` and `stat` assumptions and works on both macOS and Linux.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec python3 "$ROOT/tools/mkfmt.py" "$@"
