"""Shared resolution helpers for test-only paths that vary by host (question 217(d), round 3
task 2C): no test module hardcodes a `/Users/probe` literal for something that must exist on
whatever machine happens to run it. Every function here is a pure lookup -- inherited
environment / local filesystem in, a resolved value (or a named, visible-skip reason) out --
and none of them ever assigns back into `os.environ` (reading an inherited variable is not the
mutation question 199 forbids; the actual `env=` dict handed to a subprocess is still built
fresh by each caller, the same way `_cargo_env()` always has).

Question 194's shape, applied uniformly: when neither the environment nor an honest,
repo-relative fallback yields something real, the caller gets back `None` plus a reason naming
exactly which variable to set and why -- never a value assembled from a hardcoded default that
does not exist on this machine, left for a downstream `cargo build`/`docker run` to fail against
for a reason that has nothing to do with what the test is actually proving.
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import Optional, Tuple

REPO_ROOT = Path(__file__).resolve().parent.parent


def resolve_existing_dir_env(
    var_name: str, repo_relative_name: str, purpose: str
) -> Tuple[Optional[str], Optional[str]]:
    """Resolve an environment variable that must name an existing directory.

    Resolution order:
    1. `var_name` in the INHERITED environment (`os.environ.get`), if it names a directory
       that actually exists.
    2. `repo_relative_name` next to this repository's own root, if that (following any
       symlink) resolves to a directory that actually exists -- this worktree's own
       machine-local convention: `GMAT R2026a` and `third_party/mirrors` are real elsewhere
       on this developer's disk and merely symlinked in beside the repo, gitignored, never
       committed (see `.gitignore`); a plain clone on another machine has neither.
    3. Neither -- returns `(None, reason)`.

    Never mutates `os.environ`; the caller decides what to do with the result (typically
    `pytest.skip(reason)` when the first element is `None`).
    """
    inherited = os.environ.get(var_name)
    if inherited:
        if Path(inherited).is_dir():
            return inherited, None
        return None, (
            f"{var_name}={inherited!r} (inherited from the environment) is not a directory -- "
            f"{purpose}. Set {var_name} to an existing directory and re-run."
        )
    local = REPO_ROOT / repo_relative_name
    if local.is_dir():
        return str(local), None
    return None, (
        f"{var_name} is not set in the inherited environment, and {local} does not exist "
        f"either -- {purpose}. Set {var_name} to an existing directory (or provide {local}, "
        f"this worktree's own machine-local convention) and re-run."
    )


def resolve_gmat_root() -> Tuple[Optional[str], Optional[str]]:
    return resolve_existing_dir_env(
        "GMAT_ROOT",
        "GMAT R2026a",
        "this test's cargo build needs a GMAT install to build against (gmat-sys/build.rs)",
    )


def resolve_cfs_mirror_dir() -> Tuple[Optional[str], Optional[str]]:
    return resolve_existing_dir_env(
        "CFS_MIRROR_DIR",
        "third_party/mirrors",
        "this test's cargo build needs the vendored cFS mirror (av-kernel/build.rs et al.)",
    )


def spoore_dir() -> Path:
    """The HOST directory bind-mounted read-only into a cross-build container for the
    `spoore-cdm` (and sibling) path dependencies.

    The root `Cargo.toml`'s own `spoore-cdm = { path = "/Users/probe/code/spoore/crates/
    spoore-cdm" }` entry pins that dependency to the literal `/Users/probe/code/spoore`
    INSIDE the container, on every host -- `Cargo.toml` is out of this task's scope to change
    (it would stale the Rust SBOMs), so every cross-build's bind-mount DESTINATION stays that
    same literal, unavoidably. The HOST-side SOURCE of that bind mount need not be a
    `/Users/probe` literal, though: this resolves `AV_SPOORE_DIR` if set, else a `spoore`
    checkout beside this repository's own root (`REPO_ROOT.parent / "spoore"`).
    """
    override = os.environ.get("AV_SPOORE_DIR")
    if override:
        return Path(override)
    return REPO_ROOT.parent / "spoore"


def missing_spoore_reason() -> Optional[str]:
    """`None` iff `spoore_dir()/crates/spoore-cdm` exists; else a question-194-shaped,
    visible-skip reason naming exactly what to set or check out."""
    marker = spoore_dir() / "crates" / "spoore-cdm"
    if marker.exists():
        return None
    return (
        f"{marker} not found -- this test's cross-build bind-mounts a local `spoore` checkout "
        f"for its `spoore-cdm` path dependency (see services/proposer/Dockerfile's own header "
        f"comment). Set AV_SPOORE_DIR to a `spoore` checkout containing crates/spoore-cdm, or "
        f"check one out at {spoore_dir()} (a sibling of this repository's own root), then "
        f"re-run this test."
    )
