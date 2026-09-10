"""Locating the repository root and the ``rustup``/``cargo`` toolchain from Python.

Both :mod:`altavista.feasibility.hashing` (shelling out to ``cargo run -p av-sweep --example
sweep_hash``) and :mod:`altavista.feasibility.runner` (finding the built ``av-sweep`` binary)
need to know where the repository's Cargo workspace lives. This module has that one job so
neither of those files has to guess twice.
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

from .errors import FeasibilityError

# This file lives at <repo_root>/altavista/feasibility/paths.py -- two parents up is the
# repository root in every checkout this package has ever been used from (this task's own
# worktree included). Used as the starting point for the upward search below rather than
# trusted blindly: _MARKER (a directory this repo's Cargo workspace root always has) is
# checked before this guess is accepted, so a package relocated to a different layout fails
# loudly instead of silently pointing at the wrong tree.
_PACKAGE_DIR = Path(__file__).resolve().parent
_GUESSED_ROOT = _PACKAGE_DIR.parent.parent

# A path that exists only under the real workspace root -- crates/av-sweep is this track's
# own crate, so its presence is a strong, specific signal (stronger than e.g. a bare
# Cargo.toml, which a vendored dependency could also carry).
_MARKER = Path("crates") / "av-sweep" / "Cargo.toml"

# Matches tests/test_cdm_run.py's own RUSTUP_PATH_PREFIX -- this environment's rustup/cargo
# are not on the default PATH a subprocess inherits, and both call sites in this package need
# the identical prefix, so it is declared once, here, rather than copied twice.
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"


def discover_repo_root(start: Optional[Path] = None) -> Path:
    """The Cargo workspace root (the directory containing ``crates/av-sweep/Cargo.toml``).

    Tries ``start`` (default: this package's own guessed root, two parents up) first, then
    walks upward from it. Raises :class:`FeasibilityError` -- never returns a guess that
    does not actually check out -- if no ancestor carries the marker.
    """
    candidate = (start or _GUESSED_ROOT).resolve()
    for directory in (candidate, *candidate.parents):
        if (directory / _MARKER).is_file():
            return directory
    raise FeasibilityError(
        f"could not find the Cargo workspace root (no ancestor of {candidate} carries "
        f"{_MARKER}); pass repo_root= explicitly if this package has been relocated")


def cargo_env(base_env: Optional[dict] = None) -> dict:
    """A copy of ``base_env`` (default ``os.environ``) with ``rustup``/``cargo`` on ``PATH``
    -- mirrors ``tests/test_cdm_run.py``'s own ``_cargo_env`` helper exactly, so a subprocess
    launched from this package finds the same toolchain the test suite already relies on.
    """
    env = dict(base_env if base_env is not None else os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env
