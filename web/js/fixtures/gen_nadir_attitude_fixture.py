"""Generates ``nadir_attitude_fixture.json``: real-GMAT ``NadirPointing`` attitude
ground truth for ``web/js/attitude_slerp_check.mjs`` (invoked by
tests/test_viewer_jitter.py), M7.1's required test that measures how far a slerp of
*coarsely* sampled attitude quaternions drifts from the *finely* sampled ground truth
(docs/open-questions.md question 88 / docs/adr/005-simulation-kernel.md sec 3: "a test
proves unit norm and continuity" -- this fixture is the GMAT half of that test's other
requirement, "a fixture from GMAT's NadirPointing attitude at fine sampling compared
against slerp of coarse samples -- state the measured error").

Ground truth: ``altavista.scenario.Scenario`` with ``Attitude="NadirPointing"``, the exact
mechanism ``tests/test_frames.py``'s M6.3 attitude tests already validate against GMAT's
own nadir direction to 1e-9 (read-only per this task's file-ownership rules; this script
only calls its public API, never edits it). The Keplerian elements below match that
file's own ``_M63_KEPLERIAN`` fixture, so this rides on an already-validated orbit rather
than an invented one.

Fine sampling (every 5 s over 1200 s = 240 samples) is treated as ground truth; every
12th fine sample (60 s spacing, 21 samples) is the "coarse" track
``web/js/attitude_slerp_check.mjs`` slerps between with the viewer's own real
``QuaternionTrackInterp`` (``web/js/interp.js``) and compares back against every fine
sample in between -- the measured angular error this task must state honestly, never
loosened to make a bound pass.

Run manually to regenerate the fixture (requires a real GMAT process; this script is not
run by pytest -- the committed JSON output is what tests/test_viewer_jitter.py's node
check consumes):

    .venv/bin/python web/js/fixtures/gen_nadir_attitude_fixture.py
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT))

import altavista as gv  # noqa: E402

OUT_PATH = Path(__file__).resolve().parent / "nadir_attitude_fixture.json"

# Same Keplerian elements as tests/test_frames.py's _M63_KEPLERIAN -- an already-validated
# (nadir residual < 1e-9) NadirPointing orbit, not an invented one.
_KEPLERIAN = dict(SMA=6778.0, ECC=0.0005, INC=51.64, RAAN=120.0, AOP=30.0, TA=0.0)

FINE_STEP_S = 5.0
DURATION_S = 1200.0
COARSE_STRIDE = 12  # 12 * 5s = 60s coarse spacing

sc = gv.Scenario("m71_nadir_attitude_fixture", frame="EarthMJ2000Eq")
sat = sc.spacecraft(
    "NadirFixtureSat", epoch="01 Jan 2026 00:00:00.000",
    keplerian=_KEPLERIAN, Attitude="NadirPointing",
)
sc.propagate(sat, seconds=DURATION_S, step=FINE_STEP_S)
tr = sat.trajectory
n = len(tr.t)
assert len(tr.attitude) == n and n > COARSE_STRIDE * 2, f"expected a densely sampled attitude stream, got {n} points"

coarse_idx = list(range(0, n, COARSE_STRIDE))
if coarse_idx[-1] != n - 1:
    coarse_idx.append(n - 1)

fixture = {
    "meta": {
        "source": "tests/test_frames.py _M63_KEPLERIAN orbit (same Keplerian elements), Attitude=NadirPointing",
        "groundTruth": "altavista.scenario.Scenario with Attitude='NadirPointing' (read-only)",
        "fineStepS": FINE_STEP_S,
        "durationS": DURATION_S,
        "coarseStride": COARSE_STRIDE,
        "note": "attitude is scalar-last [x,y,z,w], expressed in the scenario frame (EarthMJ2000Eq); t is A1MJD",
    },
    "fine": {
        "t": tr.t,
        "quat": [c for q in tr.attitude for c in q],
    },
    "coarseIndices": coarse_idx,
}
OUT_PATH.write_text(json.dumps(fixture, indent=1))
print(f"wrote {OUT_PATH} ({n} fine samples, {len(coarse_idx)} coarse samples)")
