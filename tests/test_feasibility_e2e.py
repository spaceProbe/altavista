"""F3 (``docs/feasibility-plan.md``): end-to-end test of the real
``demo_two_instance_sweep`` study through ``altavista.feasibility``'s own Python authoring
path -- declare the sweep in Python, emit its YAML (with a real, Rust-computed hash), launch
the real ``av-sweep`` binary, and load the resulting ``SweepResults`` back. This is the same
path ``tests/fixtures/demo_two_instance_sweep.sweepresults.bin`` (``tests/test_cdm_sweep.py``'s
own fixture) was produced through -- this test does not read that committed fixture at all,
it re-runs the real study independently and checks the result against the same expected
table, so a regression in the authoring path OR in ``av-sweep`` itself (rare, since that
crate has its own extensive Rust test suite, but not impossible after a merge from
``develop``) would show up here even if the committed fixture were stale.

Marked ``@pytest.mark.slow`` (this repository's existing convention) -- a real 4-point x
2-draw (8-sample) GMAT study, measured at ~31s with ``--workers 2`` in this environment
(``docs/feasibility-plan.md``'s own estimate was ~93s; see this test's own module-scoped
fixture for the exact measured wall time saved to the scratchpad log this task's brief
requires).
"""
from __future__ import annotations

import subprocess
import time
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
from altavista.feasibility.paths import cargo_env

REPO_ROOT = Path(__file__).resolve().parents[1]
DRM_PATH = REPO_ROOT / "drms" / "demo_two_instance_sweep.drm.yaml"
SOS_PATH = REPO_ROOT / "drms" / "demo_two_instance.sos.yaml"
SYSTEM_PATH = REPO_ROOT / "drms" / "demo_two_instance.system.yaml"
CTRL_SYSTEM_PATH = REPO_ROOT / "drms" / "demo_two_instance_ctrl.system.yaml"

# The expected table this task's manager recorded from the previous round's own measurement
# (docs/feasibility-plan.md's F2b update) -- written down BEFORE this test's own run, per
# this task's "hypotheses before measuring" rule. demo_mvr_rmag_at_end, meters, at each of
# the 4 points (DragArea, dv_x), for draws 0 and 1.
EXPECTED_DEMO_MVR_RMAG = {
    0: (6895836.508, 6895853.128),  # (DragArea=5, dv_x=10)
    1: (6943487.876, 6945526.236),  # (DragArea=5, dv_x=30)
    2: (6896092.128, 6895472.136),  # (DragArea=25, dv_x=10)
    3: (6944981.977, 6947523.490),  # (DragArea=25, dv_x=30)
}


@pytest.fixture(scope="module")
def av_sweep_bin() -> Path:
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-sweep", "--bin", "av-sweep"],
        cwd=str(REPO_ROOT), env=cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-sweep --bin av-sweep failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return find_av_sweep_binary(REPO_ROOT)


def _the_real_fixture_sweep() -> SweepDeclaration:
    """The exact sweep drms/demo_two_instance_sweep.sweep.yaml hand-authors -- see that
    file's own header comment for the full rationale (DragArea over Cd, the dv_x bracket
    around the commanded 20.0 m/s burn)."""
    return SweepDeclaration(
        id="demo_two_instance_sweep",
        drm_id="demo_two_instance_sweep_drm",
        axes=[
            SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 25.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 30.0]),
        ],
        monte_carlo_draws=2,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT", tool="av-sweep F1b fixture authoring"),
    )


@pytest.mark.slow
def test_the_real_demo_two_instance_sweep_study_end_to_end(av_sweep_bin, tmp_path):
    """Declares the sweep in Python, emits+hashes its YAML, runs it, and checks the result
    shape and the demo_mvr_rmag_at_end values against the expected table above -- fails
    against: a wrong grid ordering (would mismatch point-to-axis-value assignment and every
    expected value would land on the wrong point), a broken seed derivation (draw 0 and
    draw 1 would collide or the run would not reproduce this task's expected numbers), or a
    broken Python->YAML->av-sweep pipeline anywhere along the way (the study would not even
    complete).
    """
    sweep = _the_real_fixture_sweep()
    sweep_yaml = tmp_path / "e2e.sweep.yaml"
    digest = emit_sweep_yaml(sweep, sweep_yaml, repo_root=REPO_ROOT)
    assert digest == "e03787532ba919ceb42300fede6a8371c08bcfa81ddb00fe9ffd65f0ba5e0236", (
        "the Python-authored sweep must hash identically to the committed "
        "drms/demo_two_instance_sweep.sweep.yaml fixture")

    out_dir = tmp_path / "out"
    t0 = time.time()
    results = run_study(
        sweep_yaml, DRM_PATH, SOS_PATH, [SYSTEM_PATH, CTRL_SYSTEM_PATH], out_dir,
        workers=2, av_sweep_bin=av_sweep_bin, timeout=600)
    elapsed = time.time() - t0

    scratch_log = Path("/private/tmp/claude-501/-Users-probe-code-AltaVista"
                        "/ed238b37-d349-42f1-956b-22807101027f/scratchpad/f3a/e2e_test_run_wall_time.txt")
    scratch_log.parent.mkdir(parents=True, exist_ok=True)
    scratch_log.write_text(f"test_the_real_demo_two_instance_sweep_study_end_to_end: {elapsed:.1f}s\n")

    assert results.sweep_id == "demo_two_instance_sweep"
    assert results.sweep_hash == digest
    assert len(results.samples) == 8, "4 points x 2 draws = 8 samples"
    assert all(s.error == "" for s in results.samples), [s.error for s in results.samples if s.error]

    by_point_draw = {(s.point_index, s.draw_index): s.scores["demo_mvr_rmag_at_end"].value
                      for s in results.samples}
    for point_index, (draw0_expected, draw1_expected) in EXPECTED_DEMO_MVR_RMAG.items():
        measured0 = by_point_draw[(point_index, 0)]
        measured1 = by_point_draw[(point_index, 1)]
        # 1 cm tolerance: the expected table itself is only recorded to 3 decimal places
        # (mm); GMAT's own numerics are deterministic given the identical seed, so this is
        # not compensating for run-to-run noise, only for the expected table's own
        # recorded precision.
        assert measured0 == pytest.approx(draw0_expected, abs=1e-2), (
            f"point {point_index} draw 0: measured {measured0}, expected {draw0_expected}")
        assert measured1 == pytest.approx(draw1_expected, abs=1e-2), (
            f"point {point_index} draw 1: measured {measured1}, expected {draw1_expected}")

    # Draw-to-draw dispersion is real (the Gates execution error on burn1): two draws at the
    # same point must not be numerically identical.
    for point_index in range(4):
        assert by_point_draw[(point_index, 0)] != by_point_draw[(point_index, 1)]

    assert len(results.aggregates) == 12, "3 scores x 4 points"
    for a in results.aggregates:
        assert a.draws == 2
