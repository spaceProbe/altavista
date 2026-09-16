#!/usr/bin/env bash
# scripts/kit/build.sh -- thin entry point for scripts/kit/build_kit.py (D3 first half). No
# logic of its own beyond locating this worktree and passing arguments through; see
# build_kit.py's own module doc for what the builder actually does and why.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." >/dev/null 2>&1 && pwd)"

if ! git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "scripts/kit/build.sh: ${REPO_ROOT} is not inside a git worktree -- refusing to run" \
         "outside the worktree this script ships in" >&2
    exit 1
fi

# git's own idea of this worktree's top level must be the directory this script was found under
# -- refuses a copy of this file dropped somewhere else (or run against a symlink farm) rather
# than silently operating on whatever repo happens to contain the current working directory.
RESOLVED_TOPLEVEL="$(git -C "${REPO_ROOT}" rev-parse --show-toplevel)"
if [ "${RESOLVED_TOPLEVEL}" != "${REPO_ROOT}" ]; then
    echo "scripts/kit/build.sh: computed repo root ${REPO_ROOT} does not match git's own" \
         "toplevel ${RESOLVED_TOPLEVEL} -- refusing (run this script from within its own" \
         "worktree, don't copy it elsewhere)" >&2
    exit 1
fi

PYTHON="${REPO_ROOT}/.venv/bin/python"
if [ ! -x "${PYTHON}" ]; then
    echo "scripts/kit/build.sh: ${PYTHON} not found or not executable -- this worktree's own" \
         "venv is required (see the task's own Environment section)" >&2
    exit 1
fi

exec "${PYTHON}" "${SCRIPT_DIR}/build_kit.py" "$@"
