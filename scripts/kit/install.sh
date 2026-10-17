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
#   (or, more commonly now -- see below: <kit-dir>/installer/install.sh <kit-dir> <target-dir>)
#
# Images are built by a separate, host-side step of this same installer (question 250(a)): the
# files install above runs inside the enclave and never gets the Docker socket. On the Docker host:
#   <kit-dir>/installer/install.sh --build-images <kit-dir> [--image-tag COMPONENT=REF ...]
#   <kit-dir>/installer/install.sh --probe-offline-build <kit-dir>
# (install.py hands both flags to image_build.py; its module doc is the specification).
#
# <kit-dir> is an existing kit (KIT_MANIFEST plus its content, kit_format 5 -- the shape
# scripts/kit/build_kit.py writes). <target-dir> must not exist yet, or must exist and be empty
# -- this installer refuses to write into a non-empty directory rather than silently merging
# into (or clobbering) whatever is already there.
#
# Round 3 (question 217(c)): scripts/kit/build_kit.py now copies this file, install.py, and the
# local modules install.py imports into every kit's own <kit-dir>/installer/ directory, so a real
# install runs the COPY that shipped with the kit, never this repository's own scripts/kit/ --
# see install.py's own module doc for what that bundling does, and does not, prove. This file's
# path-relative "$SCRIPT_DIR" resolution (below) needs no change either way: it finds install.py
# beside wherever it itself was invoked from, in this repository or inside a kit alike.
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
# (task 3b's own hard requirement). It must be Python >= 3.11: the bundled modules read TOML with the
# standard library's `tomllib` (sbom.py), which older interpreters lack -- measured on this host, where
# a bare `python3` on PATH is /usr/bin/python3 3.9.6 and the install died in a ModuleNotFoundError
# traceback. So: the first of python3.13, python3.12, python3.11, python3, python on PATH that
# is new enough (the minimal images this proof runs in, e.g. python:3.13-slim, only provide python3).
# $PYTHON lets a caller pin an exact interpreter (e.g. a specific venv's own binary) without this
# script guessing; it is used as given.
if [ -n "${PYTHON:-}" ]; then
    PY="${PYTHON}"
else
    PY=""
    for candidate in python3.13 python3.12 python3.11 python3 python; do
        if command -v "${candidate}" >/dev/null 2>&1 &&
           "${candidate}" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
            PY="${candidate}"
            break
        fi
    done
    if [ -z "${PY}" ]; then
        echo "scripts/kit/install.sh: refusing -- no Python >= 3.11 interpreter (python3.13, python3.12," \
             "python3.11, python3 or python) found on PATH. This installer needs a shell and Python 3.11" \
             "or newer and nothing else (no other dependency, no network fetch); install it, or set" \
             "PYTHON to one, and re-run." >&2
        exit 1
    fi
fi

# PYTHONDONTWRITEBYTECODE: an installer run from inside a kit must not add `__pycache__/` files to it
# (the kit would no longer verify against its own KIT_MANIFEST); install.py also sets
# sys.dont_write_bytecode itself, this covers the interpreter's own start-up imports.
PYTHONDONTWRITEBYTECODE=1 exec "${PY}" "${SCRIPT_DIR}/install.py" "$@"
