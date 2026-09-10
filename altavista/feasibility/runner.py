"""Launching ``av-sweep`` and loading its results (F3, ``docs/feasibility-plan.md``).

``av-sweep`` is the process-parallel study executor built in F1/F2 (``crates/av-sweep``, see
that crate's own module doc comments in ``src/bin/av-sweep/main.rs``) -- study mode is the
parent this module drives; it never touches GMAT itself (only its per-sample child processes
do), so running it from Python needs no GMAT binding of any kind, only a subprocess call and
a ``sweep_results.pb`` read back afterward.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path
from typing import List, Optional, Sequence

from altavista.pb.altavista.v1 import run_pb2

from .errors import FeasibilityBinaryNotFoundError, FeasibilityRunError
from .paths import discover_repo_root


def find_av_sweep_binary(repo_root: Optional[Path] = None) -> Path:
    """The built ``av-sweep`` binary: ``target/debug/av-sweep`` or ``target/release/av-sweep``
    under the discovered repo root (``debug`` preferred -- the profile ``cargo build -p
    av-sweep --bin av-sweep`` produces by default, matching ``tests/test_cdm_run.py``'s own
    ``av_run_bin`` fixture convention for the sibling ``av-run`` binary), falling back to
    ``PATH`` (``shutil.which``) for an installed build outside this checkout.

    Deliberately does **not** build the binary itself -- building can take minutes and touch
    the network on a cold cache; a caller that wants it built does so explicitly (``cargo
    build -p av-sweep --bin av-sweep``, the same command a test's own module-scoped fixture
    runs once), so this function's own cost stays a handful of ``Path.is_file()`` checks.
    Raises :class:`FeasibilityBinaryNotFoundError` -- never silently falls back to some other
    binary named ``av-sweep`` found by accident elsewhere -- naming exactly where it looked.
    """
    root = repo_root or discover_repo_root()
    candidates = [root / "target" / "debug" / "av-sweep", root / "target" / "release" / "av-sweep"]
    for c in candidates:
        if c.is_file():
            return c
    found = shutil.which("av-sweep")
    if found:
        return Path(found)
    raise FeasibilityBinaryNotFoundError(
        f"no av-sweep binary found at {[str(c) for c in candidates]} or on PATH; build it "
        f"with `cargo build -p av-sweep --bin av-sweep` (cwd {root})")


def load_sweep_results(path: Path) -> run_pb2.SweepResults:
    """Decodes a ``sweep_results.pb`` file (``av-sweep``'s own binary-protobuf output,
    ``ParseFromString`` -- no bespoke envelope) into a real ``altavista.v1.SweepResults``."""
    results = run_pb2.SweepResults()
    results.ParseFromString(Path(path).read_bytes())
    return results


def run_study(sweep_yaml: Path, drm: Path, sos: Path, systems: Sequence[Path], out_dir: Path,
              *, workers: int, av_sweep_bin: Optional[Path] = None,
              gmat_startup: Optional[str] = None, store_dir: Optional[Path] = None,
              timeout: Optional[float] = None) -> run_pb2.SweepResults:
    """Runs ``av-sweep`` in study mode: ``--sweep --drm --sos --system... --out-dir --workers
    [--gmat-startup] [--store-dir]`` (``crates/av-sweep/src/bin/av-sweep/cli.rs``'s own
    ``StudyArgs``), then loads and returns the ``sweep_results.pb`` it wrote under
    ``out_dir``.

    ``av-sweep`` itself exits **0** even when every sample failed (its own contract: "a
    failed sample is recorded, never fatal to the study" -- ``docs/feasibility-plan.md``
    question 191) -- so a non-zero exit here means a genuine *structural* refusal (a
    tampered/mismatched hash, an unreadable DRM, ``workers == 0``, and the like), raised as
    :class:`FeasibilityRunError` with the child's full stdout/stderr, never swallowed. A
    zero exit with no ``sweep_results.pb`` on disk (should not happen, but "an exit code is
    not evidence" applies here too) is raised the same way rather than assumed to mean
    success.
    """
    binary = Path(av_sweep_bin) if av_sweep_bin is not None else find_av_sweep_binary()
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    cmd: List[str] = [str(binary), "--sweep", str(sweep_yaml), "--drm", str(drm), "--sos", str(sos)]
    for s in systems:
        cmd += ["--system", str(s)]
    cmd += ["--out-dir", str(out_dir), "--workers", str(workers)]
    if gmat_startup:
        cmd += ["--gmat-startup", str(gmat_startup)]
    if store_dir is not None:
        cmd += ["--store-dir", str(store_dir)]

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        raise FeasibilityRunError(f"{' '.join(cmd)} timed out after {timeout}s") from exc
    if proc.returncode != 0:
        raise FeasibilityRunError(
            f"av-sweep failed (rc={proc.returncode})\ncmd: {' '.join(cmd)}\n--- stdout ---\n"
            f"{proc.stdout}\n--- stderr ---\n{proc.stderr}")

    results_path = out_dir / "sweep_results.pb"
    if not results_path.is_file():
        raise FeasibilityRunError(
            f"av-sweep exited 0 but did not write {results_path}\ncmd: {' '.join(cmd)}\n"
            f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return load_sweep_results(results_path)
