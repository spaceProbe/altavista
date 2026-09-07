"""Generates ``ric_axes_fixture.json``: GMAT ground-truth RIC/VNB/VVLH rotation
matrices for examples/05_rpo_ric.py's Target orbit, at several epochs, for
``web/js/ric_axes_check.mjs`` (invoked by tests/test_viewer_jitter.py) to check the
viewer's client-side axes computation (web/js/frames.js's
axesRIC/axesVNB/axesVVLH) against.

Ground truth: altavista.frames.FrameRegistry.rotation_matrix() -- read-only per this
task's file-ownership rules (M5.2); this script only calls its public API, never
edits it. GMAT's CoordinateConverter/ObjectReferenced axis system is the reference
implementation (altavista/frames.py's own module docstring: "GMAT's CoordinateSystem is
the reference implementation and the validator").

Epoch choice -- exact originTrack knots, not arbitrary intermediate times
---------------------------------------------------------------------------
web/js/interp.js's cubic Hermite interpolation reduces *exactly* to the recorded
sample position/velocity at a segment boundary (s=0 or s=1: the position basis
functions h10/h01 vanish leaving p=p[a] or p[b], and -- since interp.js's new (M5.2)
velocity derivative uses the *same* cubic, differentiated -- the derivative basis
functions dh10=1/dh11=1 (others zero) at s=0/s=1 give v=v[a] or v[b] exactly too; this
is a checkable property of the Hermite formula, not an approximation). Testing at
knots therefore isolates what this fixture is actually meant to check -- do the JS
client's axesRIC/axesVNB/axesVVLH functions agree with GMAT's own ObjectReferenced
axes for an *identical* input state -- from a completely separate question (how well a
cubic Hermite fit tracks GMAT's true orbital dynamics *between* samples), which is
already covered by tests/test_viewer_jitter.py's jitter-bound tests (LEO/Moon/Mars/RPO
sub-metre and centimetre bounds). Mixing the two would make a 1e-9 agreement bound
meaningless (Hermite-vs-true-dynamics error between samples is many orders of
magnitude looser than 1e-9), so this script deliberately does not do that.

At each chosen epoch, the Target spacecraft's GMAT object is set to exactly the
recorded (t, pos, vel) sample (mirroring altavista.scenario.Spacecraft._write_back /
tests/test_frames.py's ``test_spacecraft`` fixture pattern: GMAT's ObjectReferenced
axis system reads the reference entity's *current* stored state, not a
time-parameterized ephemeris, so the queried epoch and the object's own Epoch field
must match).

Run manually to regenerate the fixture (requires a real GMAT process; this script is
not run by pytest -- the committed JSON output is what
tests/test_viewer_jitter.py's node check consumes):

    .venv/bin/python web/js/fixtures/gen_ric_fixture.py
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT))

import altavista as gv  # noqa: E402
from altavista import frames as frames_mod  # noqa: E402
from altavista.pb import core_pb2  # noqa: E402

OUT_PATH = Path(__file__).resolve().parent / "ric_axes_fixture.json"

# Same Keplerian elements as examples/05_rpo_ric.py's Target -- this fixture rides on
# that example's own orbit, not an invented one.
sc = gv.Scenario("m52_ric_fixture", frame="EarthMJ2000Eq")
target = sc.spacecraft(
    "Target", epoch="01 Jan 2026 00:00:00.000",
    keplerian=dict(SMA=6878.0, ECC=0.0005, INC=51.6, RAAN=45.0, AOP=0.0, TA=0.0),
    DryMass=500.0,
)
sc.propagate([target], hours=2.33, step=15)
tr = target.trajectory
n = len(tr.t)
assert n >= 6, f"expected a densely sampled trajectory, got {n} points"

registry = frames_mod.FrameRegistry()
earth_eq = registry.register(core_pb2.FrameDefinition(
    id="earth_mj2000eq", body="Earth", axes=core_pb2.AXES_KIND_MJ2000_EQ))
AXES_BY_KIND = {
    "ric": core_pb2.AXES_KIND_RIC,
    "vnb": core_pb2.AXES_KIND_VNB,
    "vvlh": core_pb2.AXES_KIND_VVLH,
}
registered_id = {}
for kind, axes in AXES_BY_KIND.items():
    fd = registry.register(core_pb2.FrameDefinition(
        id=f"target_{kind}", entity_id="Target", axes=axes,
        reference_entity_id="Target", reference_body="Earth"))
    registered_id[kind] = fd.id

# Several knot indices spread across the trajectory (first, ~20%, ~50%, ~75%, last) --
# not only the endpoints.
idx = sorted({0, n // 5, n // 2, (3 * n) // 4, n - 1})

g = sc.gmat
epochs = []
for i in idx:
    t = tr.t[i]
    pos, vel = tr.pos[i], tr.vel[i]
    target.obj.SetField("DateFormat", "A1ModJulian")
    target.obj.SetField("Epoch", repr(t))
    target.obj.SetField("CoordinateSystem", "EarthMJ2000Eq")
    target.obj.SetField("DisplayStateType", "Cartesian")
    for k, v in zip(("X", "Y", "Z", "VX", "VY", "VZ"), list(pos) + list(vel)):
        target.obj.SetField(k, float(v))
    g.Initialize()

    entry = {"t": t, "pos": pos, "vel": vel, "rotation": {}}
    for kind in AXES_BY_KIND:
        # R's ROWS are the target-frame's basis axes expressed in earth_eq
        # coordinates (registry.rotation_matrix(from_id, to_id, epoch); confirmed
        # against tests/test_frames.py::test_ric_axes_orthonormal_and_radial, which
        # reads R[0, :] as the RIC frame's R-axis expressed in the source (earth_eq)
        # frame). web/js/ric_axes_check.mjs compares this directly against the JS
        # client's own axesRIC/axesVNB/axesVVLH output vectors.
        R = registry.rotation_matrix(earth_eq.id, registered_id[kind], t)
        entry["rotation"][kind] = R.tolist()
    epochs.append(entry)

fixture = {
    "meta": {
        "source": "examples/05_rpo_ric.py Target orbit (same Keplerian elements)",
        "parentFrame": "EarthMJ2000Eq",
        "groundTruth": "altavista.frames.FrameRegistry.rotation_matrix() (GMAT ObjectReferenced, read-only)",
        "rotationConvention": "rotation[kind][i] = axes kind's i-th basis vector (X,Y,Z) expressed in EarthMJ2000Eq coordinates",
        "note": "epochs are exact originTrack knots -- see this script's module docstring",
    },
    "originTrack": {
        "t": tr.t,
        "pos": [c for p in tr.pos for c in p],
        "vel": [c for v in tr.vel for c in v],
    },
    "epochs": epochs,
}
OUT_PATH.write_text(json.dumps(fixture, indent=1))
print(f"wrote {OUT_PATH} ({len(epochs)} epochs, {n} track samples)")
