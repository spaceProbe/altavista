"""The canonical ``ParameterSweep`` hash, from Python (F3, ``docs/feasibility-plan.md``).

``ParameterSweep.hash`` is SHA-256 over the message's own canonical protobuf encoding with
``hash`` cleared first (``crates/av-sweep/src/hash.rs::canonical_sweep_hash`` -- ``prost``'s
canonical encoding, the same scheme ``av_kernel::drm::hash`` uses for the DRM/SOS/system
messages). Two ways to get that value from Python were both real options:

1. **Shell out to the existing Rust tool** (``cargo run -p av-sweep --example sweep_hash``,
   ``crates/av-sweep/examples/sweep_hash.rs``) -- the route this module actually takes.
2. **Recompute it in Python** from the generated ``altavista.pb`` bindings -- ``protobuf``'s
   Python serializer does not guarantee the same byte-for-byte field ordering ``prost``'s
   canonical encoding does (this task's own brief: "a canonical encoding must match prost's
   field ordering; verify, do not assume"), so this route would need its own from-scratch
   canonical encoder (fixed field order, no default-value elision it doesn't already do) --
   real work, duplicating logic ``av_sweep::hash`` already has, for a hash whose only
   consumer today (this package) already has to shell out to the Rust binary anyway to
   *run* the study (:mod:`altavista.feasibility.runner`). Given that, route 1 is not merely
   cheaper -- it is the only one of the two that cannot silently drift from
   ``canonical_sweep_hash`` itself, since it *is* ``canonical_sweep_hash``, called through the
   one tool this repository already ships for exactly this ("write hash: \"\", run this,
   paste the digest back", mirroring ``crates/av-kernel/examples/drm_hash.rs``'s own
   convention, per ``sweep_hash.rs``'s own module doc comment).

**Honest disclosure of what this buys, and what it does not.** Because route 1 *is* the Rust
implementation, ``test_feasibility_hash.py``'s hash-agreement test (comparing this module's
output against ``drms/demo_two_instance_sweep.sweep.yaml``'s own committed hash) cannot by
itself prove two *independent* implementations agree -- there is only one implementation,
called from two languages. What it does prove, and is worth proving: that this module invokes
the tool correctly (right crate, right example, right argument, right cwd, right ``PATH`` for
``cargo``/``rustup``), parses its stdout correctly (a bare 64-character hex digest, no
trailing text kept), and that the whole round trip from a Python-authored :class:`~altavista.
feasibility.declare.SweepDeclaration` through :mod:`~altavista.feasibility.yaml_io` produces a
document ``canonical_sweep_hash`` accepts and hashes to the exact value already committed
alongside that fixture. A change to ``canonical_sweep_hash`` itself is caught by
``crates/av-sweep``'s own Rust tests, not by this module -- this module's job is agreement
with whatever that function currently computes, not an independent audit of it.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Optional

from .errors import FeasibilityHashError
from .paths import cargo_env, discover_repo_root

_HEX_DIGITS = set("0123456789abcdef")


def compute_sweep_hash(sweep_yaml_path: Path, *, repo_root: Optional[Path] = None,
                        timeout: float = 300.0) -> str:
    """The canonical hash of the ``ParameterSweep`` YAML at ``sweep_yaml_path``, via ``cargo
    run -p av-sweep --example sweep_hash -- <path>`` (see this module's own docstring for why
    this route was chosen). Raises :class:`FeasibilityHashError` -- never returns a
    placeholder or a truncated/garbled digest -- if the subprocess fails or its stdout is not
    a bare 64-character lowercase hex string.

    This is also, incidentally, a real exercise of ``av_sweep::parse_sweep_yaml`` (the
    example's first step, before hashing) -- a YAML file this function accepts has been
    loaded by the real Rust loader, not merely believed to be loadable.
    """
    root = repo_root or discover_repo_root()
    cmd = ["cargo", "run", "-q", "-p", "av-sweep", "--example", "sweep_hash", "--", str(sweep_yaml_path)]
    try:
        proc = subprocess.run(cmd, cwd=str(root), env=cargo_env(), capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        raise FeasibilityHashError(f"{' '.join(cmd)} timed out after {timeout}s") from exc
    if proc.returncode != 0:
        raise FeasibilityHashError(
            f"{' '.join(cmd)} failed (rc={proc.returncode})\n--- stdout ---\n{proc.stdout}"
            f"\n--- stderr ---\n{proc.stderr}")
    digest = proc.stdout.strip()
    if len(digest) != 64 or any(c not in _HEX_DIGITS for c in digest.lower()) or digest.lower() != digest:
        raise FeasibilityHashError(
            f"{' '.join(cmd)} did not print a bare 64-character lowercase hex digest on "
            f"stdout, got {digest!r}\n--- stderr ---\n{proc.stderr}")
    return digest


def emit_sweep_yaml(sweep, path: Path, *, repo_root: Optional[Path] = None,
                     timeout: float = 300.0) -> str:
    """Writes ``path`` with a freshly-computed, correct ``hash:`` field, and returns the
    computed hash (also stamped onto ``sweep.hash``, so the in-memory declaration matches
    what was written).

    Two passes, deliberately: :func:`~altavista.feasibility.yaml_io.to_yaml` with
    ``hash=""`` is written to a scratch file first (``canonical_sweep_hash`` computes the
    hash over the message *with its own hash field cleared* -- an empty string is exactly
    that clear value, not a placeholder that needs special-casing), hashed via
    :func:`compute_sweep_hash`, then the *real* document -- identical in every other field --
    is written to ``path`` with that digest in its ``hash:`` field. The scratch file is
    always removed, success or failure, so a caller's directory never accumulates
    ``*.hashing.tmp`` litter.
    """
    from . import yaml_io  # local import: avoids a cycle (yaml_io does not import this module)

    path = Path(path)
    scratch = path.with_name(path.name + ".hashing.tmp")
    scratch.write_text(yaml_io.to_yaml(sweep, hash=""))
    try:
        digest = compute_sweep_hash(scratch, repo_root=repo_root, timeout=timeout)
    finally:
        scratch.unlink(missing_ok=True)

    path.write_text(yaml_io.to_yaml(sweep, hash=digest))
    sweep.hash = digest
    return digest
