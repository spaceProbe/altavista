"""F3 (``docs/feasibility-plan.md``): the emitted YAML round-trips through the real
``av-sweep`` loader.

Unlike ``tests/test_feasibility_hash.py`` (which shells out to the ``sweep_hash`` *example*,
GMAT-free), this file drives the real ``av-sweep`` *binary* itself in study mode -- its own
first action is ``av_sweep::parse_sweep_yaml`` followed by ``av_sweep::verify_sweep_hash``
(``crates/av-sweep/src/bin/av-sweep/study.rs``, line ~105-106), before it does anything else
-- so a genuine one-point, one-draw study against the real demo DRM proves both "the file
parses" and "its declared hash verifies against the message it parsed to", the two things
this package's own brief calls "must round-trip". A load or hash-verify failure here means
``av-sweep`` exits non-zero and ``run_study`` raises, surfacing it as a real test failure
(never swallowed).

Marked ``@pytest.mark.slow`` (this repository's existing convention, e.g.
``tests/test_dynamics_service_rs.py``) because it needs a real GMAT run, not because it is a
pure round-trip check -- kept to a single point x single draw specifically so this GMAT cost
stays small (~8-25s measured, drms/demo_two_instance_sweep.drm.yaml's own header comment: one
sample of this DRM took 8.18s in a prior debug-build measurement).
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from altavista.feasibility import (
    Provenance,
    SweepAxis,
    SweepDeclaration,
    emit_sweep_yaml,
    find_av_sweep_binary,
    run_study,
)
from altavista.feasibility.errors import FeasibilityRunError
from altavista.feasibility.paths import cargo_env

REPO_ROOT = Path(__file__).resolve().parents[1]
DRM_PATH = REPO_ROOT / "drms" / "demo_two_instance_sweep.drm.yaml"
SOS_PATH = REPO_ROOT / "drms" / "demo_two_instance.sos.yaml"
SYSTEM_PATH = REPO_ROOT / "drms" / "demo_two_instance.system.yaml"
CTRL_SYSTEM_PATH = REPO_ROOT / "drms" / "demo_two_instance_ctrl.system.yaml"


@pytest.fixture(scope="module")
def av_sweep_bin() -> Path:
    """Builds the real ``av-sweep`` binary once for the module -- a build failure is a real
    failure of this task, mirroring ``tests/test_cdm_run.py``'s own ``av_run_bin`` fixture."""
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-sweep", "--bin", "av-sweep"],
        cwd=str(REPO_ROOT), env=cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-sweep --bin av-sweep failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return find_av_sweep_binary(REPO_ROOT)


def _minimal_sweep() -> SweepDeclaration:
    """One point (single explicit value on each axis), one draw -- the smallest real study
    over the demo fixture that still exercises both axis kinds (parameter + event)."""
    return SweepDeclaration(
        id="demo_two_instance_sweep",
        drm_id="demo_two_instance_sweep_drm",
        axes=[
            SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0]),
        ],
        monte_carlo_draws=1,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT", tool="test_feasibility_yaml_roundtrip"),
    )


@pytest.mark.slow
def test_emitted_yaml_round_trips_through_the_real_av_sweep_loader(av_sweep_bin, tmp_path):
    """Fails against a yaml_io/hashing bug that produces a document the real Rust loader
    rejects (wrong field name, wrong type, a hash that does not verify) -- av-sweep would
    exit non-zero immediately (before any GMAT work), and run_study would raise
    FeasibilityRunError, which this test does NOT catch -- an uncaught raise is exactly the
    "visible test failure, not swallowed" this task's brief requires.
    """
    sweep = _minimal_sweep()
    sweep_yaml = tmp_path / "roundtrip.sweep.yaml"
    digest = emit_sweep_yaml(sweep, sweep_yaml)
    assert len(digest) == 64

    out_dir = tmp_path / "out"
    results = run_study(
        sweep_yaml, DRM_PATH, SOS_PATH, [SYSTEM_PATH, CTRL_SYSTEM_PATH], out_dir,
        workers=1, av_sweep_bin=av_sweep_bin, timeout=180)

    assert results.sweep_hash == digest
    assert len(results.samples) == 1, "one point x one draw must produce exactly one sample"
    assert results.samples[0].error == "", f"the single sample failed: {results.samples[0].error!r}"


@pytest.mark.slow
def test_a_tampered_hash_is_refused_before_any_gmat_work(av_sweep_bin, tmp_path):
    """Proof that verify_sweep_hash is genuinely exercised, not merely parse_sweep_yaml: a
    sweep file whose declared hash does not match its own content must be refused by the
    real av-sweep binary -- fails against a wrong implementation that never checks the hash
    at all (would proceed to attempt the GMAT run, and either succeed wrongly or fail with
    an unrelated error) or against a run_study that swallows the failure instead of raising.
    """
    sweep = _minimal_sweep()
    sweep_yaml = tmp_path / "tampered.sweep.yaml"
    emit_sweep_yaml(sweep, sweep_yaml)
    # Corrupt the declared hash field in place, everything else left untouched.
    text = sweep_yaml.read_text()
    tampered = text.replace(sweep.hash, "0" * 64)
    assert tampered != text, "the hash substitution must actually have changed the file"
    sweep_yaml.write_text(tampered)

    out_dir = tmp_path / "out"
    with pytest.raises(FeasibilityRunError, match="(?i)hash"):
        run_study(sweep_yaml, DRM_PATH, SOS_PATH, [SYSTEM_PATH, CTRL_SYSTEM_PATH], out_dir,
                  workers=1, av_sweep_bin=av_sweep_bin, timeout=60)
