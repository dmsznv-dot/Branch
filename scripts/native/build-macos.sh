#!/usr/bin/env bash
# Source SDK only. Distribution is gated separately by verify-player-bundle.py.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
if [[ $# -gt 1 || ( $# -eq 1 && "$1" != --check-plan ) ]]; then
    echo 'usage: build-macos.sh [--check-plan]' >&2
    exit 2
fi
exec python3 "$repo/scripts/native/build-target.py" --target aarch64-apple-darwin "$@"
