#!/usr/bin/env bash
# Thin wrapper around `generate.py` -- run from anywhere; regenerates altavista/pb in place.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
exec "$REPO_ROOT/.venv/bin/python" "$REPO_ROOT/altavista/pb/generate.py"
