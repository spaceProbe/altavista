#!/bin/sh
# scripts/kit/install.sh -- D3's second half (docs/p5-plan.md, P5 track round 2 task 3b): the
# zero-egress installer. Thin POSIX entry point, no logic of its own beyond locating a Python
# interpreter and this script's own directory (to find install.py beside it) -- every actual
# decision (verify, refuse, copy, pip install --no-index) lives in install.py, in the same
# "shell script that execs into Python" shape scripts/kit/build.sh already established for the
# builder side of this same pair of tools.
#
# Usage:
#   scripts/kit/install.sh <kit-dir> <target-dir>
#
# <kit-dir> is an existing kit (KIT_MANIFEST plus its content, kit_format 2 -- the shape
# scripts/kit/build_kit.py writes). <target-dir> must not exist yet, or must exist and be empty
# -- this installer refuses to write into a non-empty directory rather than silently merging
# into (or clobbering) whatever is already there.
#
# Deliberately NOT bash, NOT zsh -- POSIX /bin/sh only, and nothing here assumes macOS. This
# script (and install.py beside it) is run identically on the host that built the kit and inside
# a bare Linux container that has never seen this worktree: it never inspects git, never assumes
# a worktree's own symlink layout, and never assumes this repository exists at all beyond the two
# files (install.sh, install.py) it was invoked from. See install.py's own module doc for the
# full "what this refuses, and why" list and for why this reaches the network never, not even
# once -- unlike scripts/kit/build.sh's own "network once, at build time" steps (question 154).
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

# A Python interpreter is the one thing this installer needs beyond a shell and the kit itself
# (task 3b's own hard requirement) -- prefer python3, fall back to a bare `python` (some minimal
# images, e.g. this proof's own python:3.13-slim, only ever provide python3, but a "python"
# alias is checked too rather than assuming one name universally). $PYTHON lets a caller pin an
# exact interpreter (e.g. a specific venv's own binary) without this script guessing.
if [ -n "${PYTHON:-}" ]; then
    PY="${PYTHON}"
elif command -v python3 >/dev/null 2>&1; then
    PY=python3
elif command -v python >/dev/null 2>&1; then
    PY=python
else
    echo "scripts/kit/install.sh: refusing -- no python3/python interpreter found on PATH." \
         "This installer needs a shell and a Python interpreter and nothing else; install" \
         "Python 3 (no other dependency, no network fetch) and re-run." >&2
    exit 1
fi

exec "${PY}" "${SCRIPT_DIR}/install.py" "$@"
