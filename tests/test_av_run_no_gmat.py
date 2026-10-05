"""`av-run` built with ``--no-default-features`` (no GMAT linked) runs the native demo DRM, and
refuses a GMAT-requiring one with the kernel's own typed error.

``docs/native-dynamics-plan.md`` round 5 left "``av-run --no-default-features`` builds and refuses
cleanly and is still not run by a test" open, and question 234 ruled that a GMAT-free binary which
refuses *every* DRM "proves amputation, not portability". So in that build ``run_drm``
(``crates/av-run/src/main.rs``) builds the same ``RunConfig`` as the default build minus the one
cfg-gated ``gmat`` field -- exactly as ``crates/av-kernel/tests/orbital_no_gmat_demo.rs`` does --
and calls the same ``av_kernel::drm::execute``. This file is the end-to-end proof, on the real
binary:

* the binary itself is GMAT-free: ``otool -L`` lists no GMAT library (the proof
  ``orbital_no_gmat_demo``'s own report uses), checked against a positive control -- the same
  ``otool -L`` on the default build *does* list ``libGmatBase``, so the check can fail;
* it runs the native demo DRM, ``drms/leo_1day_orbital_native.{drm,sos,system}.yaml`` (the
  golden bundle with ``dynamics_model`` swapped ``gmat.`` -> ``orbital.`` and the Keplerian
  elements replaced by the same orbit's Cartesian state, which the native model requires; it
  is the file-based twin of the bundle ``orbital_no_gmat_demo`` builds in Rust), with exit 0,
  the stderr summary line and the wire product: one trajectory per declared instance, the
  declared window and sample count, the declared initial state, the same plausibility bounds
  ``orbital_no_gmat_demo`` asserts (LEO band, osculating-SMA drift under 1 percent), and the
  segment's ``dynamics_depth == "native"`` read from the data;
* the default-build ``av-run`` on the same DRM produces bit-identical trajectory samples (both
  run the same native model, so anything else would be a defect in one of the two builds);
* ``drms/leo_1day_golden.*`` (``gmat.``-dispatched) is refused with a non-zero exit and a message
  naming the instance, the model id and the missing feature -- not a panic, not a backtrace.

Why a separate target directory: the no-default-features graph is a different feature
resolution of the same workspace, so building it into ``target/`` would relink
``target/debug/av-run`` as a GMAT-free binary, which is the binary every other test here
launches (``tests/test_cdm_run.py``) -- they would then fail for a reason unrelated to them.
``--target-dir target/no-gmat`` keeps the two builds apart (the cost is one cold build of the
no-default graph, minutes the first time). The default-build binary used for the comparison is
built into the ordinary ``target/``, as ``tests/test_cdm_run.py`` does.

How the gate runs this: a Python run that launches workspace binaries holds a cargo slot for its
whole run (question 235), and this module's own builds go through ``scripts/dev/cargo-slot``
like every cargo invocation, taking the second slot nested under the held one::

    scripts/dev/cargo-slot --hold -- .venv/bin/python -m pytest -q -rs tests/test_av_run_no_gmat.py

A build failure fails the run (never a skip). The native model reads its gravity and ephemeris
files from ``$GMAT_ROOT/data`` even though no GMAT code is linked, so an unresolvable
``GMAT_ROOT`` also fails rather than skips.
"""
from __future__ import annotations

import math
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

from altavista.pb.altavista.v1 import run_pb2
from altavista.test_env import resolve_gmat_root

REPO_ROOT = Path(__file__).resolve().parents[1]
CARGO_SLOT = REPO_ROOT / "scripts" / "dev" / "cargo-slot"
NO_GMAT_TARGET = REPO_ROOT / "target" / "no-gmat"
NO_GMAT_BIN = NO_GMAT_TARGET / "debug" / "av-run"
DEFAULT_BIN = REPO_ROOT / "target" / "debug" / "av-run"

NATIVE_DRM = REPO_ROOT / "drms" / "leo_1day_orbital_native.drm.yaml"
NATIVE_SOS = REPO_ROOT / "drms" / "leo_1day_orbital_native.sos.yaml"
NATIVE_SYSTEM = REPO_ROOT / "drms" / "leo_1day_orbital_native.system.yaml"
GOLDEN_DRM = REPO_ROOT / "drms" / "leo_1day_golden.drm.yaml"
GOLDEN_SOS = REPO_ROOT / "drms" / "leo_1day_golden.sos.yaml"
GOLDEN_SYSTEM = REPO_ROOT / "drms" / "leo_1day_golden.system.yaml"

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
BUILD_TIMEOUT_S = 3600  # a cold build of the no-default graph is minutes; bounded wall-clock
RUN_TIMEOUT_S = 300
RUN_ID = "test-av-run-no-gmat"

# Constants of `crates/av-kernel/tests/orbital_no_gmat_demo.rs`'s own plausibility checks.
SMA_M = 6_878_000.0
LEO_BAND_M = 200_000.0
SMA_DRIFT_MAX = 1e-2
MU_JGM2 = 3.986004415e14


def _env() -> dict:
    gmat_root, reason = resolve_gmat_root()
    if gmat_root is None:
        pytest.fail(f"GMAT_ROOT does not resolve ({reason}); the native model reads its data files from it")
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    env["GMAT_ROOT"] = gmat_root
    env.setdefault("CARGO_BUILD_JOBS", "4")
    return env


def _cargo_slot_build(*extra: str) -> None:
    proc = subprocess.run(
        [sys.executable, str(CARGO_SLOT), "build", "-p", "av-run", *extra],
        cwd=str(REPO_ROOT), env=_env(), capture_output=True, text=True, timeout=BUILD_TIMEOUT_S)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-run {' '.join(extra)} failed (rc={proc.returncode}):\n"
                    f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")


@pytest.fixture(scope="module")
def no_gmat_bin() -> Path:
    _cargo_slot_build("--no-default-features", "--target-dir", str(NO_GMAT_TARGET))
    assert NO_GMAT_BIN.is_file(), f"expected {NO_GMAT_BIN} after a successful build"
    return NO_GMAT_BIN


@pytest.fixture(scope="module")
def default_bin() -> Path:
    _cargo_slot_build()
    assert DEFAULT_BIN.is_file(), f"expected {DEFAULT_BIN} after a successful build"
    return DEFAULT_BIN


def _run_av_run(binary: Path, drm: Path, sos: Path, system: Path, out: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(binary), "--drm", str(drm), "--sos", str(sos), "--system", str(system),
         "--run-id", RUN_ID, "--out", str(out)],
        cwd=str(REPO_ROOT), env=_env(), capture_output=True, text=True, timeout=RUN_TIMEOUT_S)


def _decode(path: Path) -> run_pb2.RunProducts:
    products = run_pb2.RunProducts()
    products.ParseFromString(path.read_bytes())
    return products


@pytest.fixture(scope="module")
def native_run(no_gmat_bin, tmp_path_factory):
    out = tmp_path_factory.mktemp("no_gmat_native") / "native.bin"
    proc = _run_av_run(no_gmat_bin, NATIVE_DRM, NATIVE_SOS, NATIVE_SYSTEM, out)
    return proc, out


@pytest.fixture(scope="module")
def default_run(default_bin, tmp_path_factory):
    out = tmp_path_factory.mktemp("default_native") / "native.bin"
    proc = _run_av_run(default_bin, NATIVE_DRM, NATIVE_SOS, NATIVE_SYSTEM, out)
    assert proc.returncode == 0, f"default-build av-run failed on the native DRM:\n{proc.stderr}"
    return _decode(out)


def _yaml(path: Path) -> dict:
    return yaml.safe_load(path.read_text())


def _linked_libraries(binary: Path) -> list:
    otool = shutil.which("otool")
    if otool is None:
        pytest.skip("otool is not installed; this proof reads the binary's own load commands")
    proc = subprocess.run([otool, "-L", str(binary)], capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, proc.stderr
    # First line is the binary's own path (which itself contains "no-gmat"); the rest are libraries.
    return [line.split()[0] for line in proc.stdout.splitlines()[1:] if line.strip()]


# --------------------------------------------------------------------------- the binary itself
def test_binary_links_no_gmat_library(no_gmat_bin, default_bin):
    libs = _linked_libraries(no_gmat_bin)
    assert libs, "otool listed no libraries at all; the check would pass vacuously"
    assert not [lib for lib in libs if "gmat" in lib.lower()], f"GMAT library linked into the no-gmat binary: {libs}"
    # Positive control: the same check on the default build must see GMAT, or it proves nothing.
    default_libs = _linked_libraries(default_bin)
    assert any("libgmat" in lib.lower() for lib in default_libs), f"expected the default build to link GMAT: {default_libs}"


# --------------------------------------------------------------------------- the native DRM runs
def test_native_demo_drm_runs_with_summary_line(native_run):
    proc, out = native_run
    assert proc.returncode == 0, f"rc={proc.returncode}\nstderr:\n{proc.stderr}"
    drm_hash = _yaml(NATIVE_DRM)["hash"]
    summary = re.search(r'^av-run: run_id="([^"]*)" config_hash=([0-9a-f]{64}) trajectories=(\d+) events=(\d+)$', proc.stderr, re.MULTILINE)
    assert summary, f"no summary line in stderr:\n{proc.stderr}"
    run_id, config_hash, n_traj, n_events = summary.groups()
    assert run_id == RUN_ID
    assert config_hash == drm_hash, "the summary must carry the DRM's own declared hash"
    assert int(n_traj) == len(_yaml(NATIVE_SOS)["instances"])
    assert int(n_events) == 2, "no faults or maneuvers declared: exactly the run start/end lifecycle events"
    assert re.search(rf"^av-run: wrote \d+ byte\(s\) to {re.escape(str(out))}$", proc.stderr, re.MULTILINE), proc.stderr


def test_native_demo_products_match_the_declarations_and_the_kernel_tests_expectations(native_run):
    proc, out = native_run
    assert proc.returncode == 0, proc.stderr
    products = _decode(out)
    drm, sos, system = _yaml(NATIVE_DRM), _yaml(NATIVE_SOS), _yaml(NATIVE_SYSTEM)
    assert products.run_id == RUN_ID
    assert products.provenance.config_hash == drm["hash"]

    declared = [inst["name"] for inst in sos["instances"]]
    assert sorted(products.trajectories.keys()) == sorted(declared), "one trajectory per declared instance"

    start, end = drm["scenario"]["start_tai_ns"], drm["scenario"]["end_tai_ns"]
    step_s = drm["options"]["sample_interval_s"]
    want_samples = int(round((end - start) / 1e9 / step_s)) + 1  # both boundary samples included
    for name in declared:
        traj = products.trajectories[name]
        assert traj.config_hash == drm["hash"]
        assert traj.frame_id == "EarthMJ2000Eq"
        assert len(traj.samples) == want_samples, f"{name}: {len(traj.samples)} samples, want {want_samples}"
        assert traj.samples[0].tai_ns == start and traj.samples[-1].tai_ns == end
        assert [s.dynamics_depth for s in traj.segments] == ["native"], "the data itself must say no GMAT dynamics ran"

        # Declared initial state: spacecraft.X..VZ are km / km/s, the trajectory is m / m/s.
        params = {p["name"]: p.get("value") for p in system["parameters"]}
        x0 = [params[f"spacecraft.{c}"] * 1000.0 for c in ("X", "Y", "Z", "VX", "VY", "VZ")]
        assert list(traj.samples[0].mean) == pytest.approx(x0, rel=1e-12, abs=1e-9)

        # The same plausibility bounds crates/av-kernel/tests/orbital_no_gmat_demo.rs asserts.
        radii = []
        for s in traj.samples:
            assert len(s.mean) == 6 and all(math.isfinite(v) for v in s.mean), s.mean
            radii.append(math.sqrt(sum(v * v for v in s.mean[:3])))
        assert abs(min(radii) - SMA_M) < LEO_BAND_M and abs(max(radii) - SMA_M) < LEO_BAND_M, (min(radii), max(radii))

        def sma(sample) -> float:
            r = math.sqrt(sum(v * v for v in sample.mean[:3]))
            v2 = sum(v * v for v in sample.mean[3:])
            return -MU_JGM2 / (2.0 * (v2 / 2.0 - MU_JGM2 / r))
        drift = abs(sma(traj.samples[-1]) - sma(traj.samples[0])) / sma(traj.samples[0])
        assert drift < SMA_DRIFT_MAX, f"osculating SMA drifted {drift:.3e} over the day"
        # The state really moved (a stub returning the initial state would pass every bound above).
        assert list(traj.samples[-1].mean) != list(traj.samples[0].mean)


def test_default_build_gives_bit_identical_trajectories_on_the_same_drm(native_run, default_run):
    proc, out = native_run
    assert proc.returncode == 0, proc.stderr
    no_gmat = _decode(out)
    assert sorted(no_gmat.trajectories) == sorted(default_run.trajectories)
    for name, traj in no_gmat.trajectories.items():
        other = default_run.trajectories[name]
        assert len(traj.samples) == len(other.samples)
        assert [s.SerializeToString() for s in traj.samples] == [s.SerializeToString() for s in other.samples], (
            f"{name}: the two builds ran the same native model but disagree; final states "
            f"{list(traj.samples[-1].mean)} vs {list(other.samples[-1].mean)}")


# --------------------------------------------------------------------------- GMAT is refused
def test_gmat_requiring_drm_is_refused_with_the_kernels_typed_error(no_gmat_bin, tmp_path):
    out = tmp_path / "golden.bin"
    proc = _run_av_run(no_gmat_bin, GOLDEN_DRM, GOLDEN_SOS, GOLDEN_SYSTEM, out)
    stderr = proc.stderr
    model_id = _yaml(GOLDEN_SYSTEM)["dynamics_model"]
    assert model_id.startswith("gmat.")
    assert proc.returncode == 1, f"want the clean error exit (1), got rc={proc.returncode} (101 would be a panic):\n{stderr}"
    assert "panicked" not in stderr and "backtrace" not in stderr.lower(), stderr
    assert stderr.startswith("av-run: DRM execution failed: "), stderr
    assert f'dynamics_model "{model_id}"' in stderr, "the refusal must name the model"
    assert 'instance "leo"' in stderr, "the refusal must name the instance"
    assert '"gmat" cargo feature is off' in stderr and "--no-default-features" in stderr, "the refusal must name the missing feature"
    assert not out.exists(), "a refused run must not write a product"
