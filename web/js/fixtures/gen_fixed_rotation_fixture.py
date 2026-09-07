"""Generates ``fixed_rotation_fixture.json``: real-GMAT ground truth for
``web/js/fixed_rotation_check.mjs`` (invoked by tests/test_viewer_jitter.py) -- the M19.2
(``docs/open-questions.md`` question 129, ADR-002's fourth amendment) required test: "the
ingested demo run viewed in EarthICRF must differ from EarthMJ2000Eq by the frame bias
magnitude."

Ground truth for two independent things, both real GMAT, neither reimplemented in JS:

1. ``quaternion`` -- the real, committed ``fixed_rotation_q`` this task's producer
   (``av-kernel::drm::executor::fill_fixed_rotations``) computed for ``EarthICRF``, read
   straight off the regenerated ``tests/fixtures/demo_two_instance.runproducts.bin`` (a real
   ``av-run`` binary's output over ``drms/demo_two_instance.*``). Not recomputed here --
   this fixture proves the *actual* wire value the viewer will receive, not a fresh,
   possibly-differently-parameterized GMAT call.
2. ``icrfGroundTruthM`` -- ``demo_flt``'s own last trajectory sample (``EarthMJ2000Eq``,
   metres) converted to ``EarthICRF`` directly through
   ``altavista.frames.FrameRegistry.convert`` (the read-only reference GMAT adapter this
   repository already trusts for CDM-facing conversions -- module docstring: "GMAT's
   CoordinateSystem is the reference implementation and the validator"), at that sample's
   own real epoch.

``web/js/fixed_rotation_check.mjs`` builds a real ``frames.js`` ``FrameGraph`` (two sibling
nodes, ``EarthMJ2000Eq`` and ``EarthICRF``, both parented at the graph root -- exactly what
``web/js/scene.js``'s ``_buildFrameGraph`` would build from ``RunProducts.frames``, since
both frames' own ``parent_frame_id`` is `""`, question 76), places a probe at
``posMJ2000EqM`` under the ``EarthMJ2000Eq`` node, and reads its coordinates back out
*as seen from* the ``EarthICRF`` node (``Object3D.worldToLocal``) -- exactly what "viewing
the ingested run in EarthICRF" means once a camera is parented there (`Viewer.
setViewFrame`). That result must match ``icrfGroundTruthM`` to tight tolerance: this is the
one check that would catch a viewer-side sign/inversion bug (the wire's own "parent -> this"
quaternion and Three.js's own "child-to-parent" `Object3D.quaternion` convention are
opposite -- see ``web/js/frames.js``'s ``fixedRotationQuaternion`` for the derivation), which
a magnitude-only comparison could not.

Run manually to regenerate the fixture (requires a real GMAT process; this script is not run
by pytest -- the committed JSON output is what tests/test_viewer_jitter.py's node check
consumes). Regenerate ``tests/fixtures/demo_two_instance.runproducts.bin`` first if the
producer's own frame output changed:

    cargo build -p av-run --bin av-run
    ./target/debug/av-run --drm drms/demo_two_instance.drm.yaml --sos drms/demo_two_instance.sos.yaml \\
        --system drms/demo_two_instance.system.yaml --run-id <id> \\
        --out tests/fixtures/demo_two_instance.runproducts.bin
    .venv/bin/python web/js/fixtures/gen_fixed_rotation_fixture.py
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT))

from altavista import cdm as cdm_adapter  # noqa: E402
from altavista import frames as frames_mod  # noqa: E402
from altavista.pb import core_pb2  # noqa: E402
from altavista.pb.altavista.v1 import run_pb2  # noqa: E402

OUT_PATH = Path(__file__).resolve().parent / "fixed_rotation_fixture.json"
DEMO_BUNDLE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance.runproducts.bin"

run_products = run_pb2.RunProducts()
run_products.ParseFromString(DEMO_BUNDLE_PATH.read_bytes())

icrf_def = next(f for f in run_products.frames if f.id == "EarthICRF")
assert len(icrf_def.fixed_rotation_q) == 4, (
    f"expected EarthICRF.fixed_rotation_q to be filled (question 129); got "
    f"{list(icrf_def.fixed_rotation_q)} -- regenerate {DEMO_BUNDLE_PATH} with a build "
    f"that includes M19.2's fill_fixed_rotations")
quaternion = list(icrf_def.fixed_rotation_q)  # wire order: [w, x, y, z]

mj2000eq_def = next(f for f in run_products.frames if f.id == "EarthMJ2000Eq")
assert not mj2000eq_def.fixed_rotation_q, "EarthMJ2000Eq is the reference itself; must stay empty"

demo_flt = run_products.trajectories["demo_flt"]
assert demo_flt.frame_id == "EarthMJ2000Eq"
last = demo_flt.samples[-1]
pos_m = list(last.mean[0:3])
epoch_a1mjd = cdm_adapter.tai_ns_to_a1mjd(last.tai_ns)

registry = frames_mod.FrameRegistry()
mj2000eq_id = registry.register(core_pb2.FrameDefinition(
    id="EarthMJ2000Eq", body="Earth", axes=core_pb2.AXES_KIND_MJ2000_EQ)).id
icrf_id = registry.register(core_pb2.FrameDefinition(
    id="EarthICRF", body="Earth", axes=core_pb2.AXES_KIND_ICRF)).id

state_m = pos_m + [0.0, 0.0, 0.0]  # convert() wants a 6-vector; velocity is unused here
icrf_state_m = registry.convert(state_m, epoch_a1mjd, mj2000eq_id, icrf_id)
icrf_ground_truth_m = icrf_state_m[0:3]

import math
orbit_radius_m = math.sqrt(sum(c * c for c in pos_m))
displacement_m = math.sqrt(sum((a - b) ** 2 for a, b in zip(icrf_ground_truth_m, pos_m)))
w, x, y, z = quaternion
angle_rad = 2.0 * math.acos(min(1.0, max(-1.0, abs(w))))
# Naive upper bound (this task's brief: "rotation angle from the quaternion, times orbit
# radius") -- exact only when the position vector is perpendicular to the rotation axis;
# in general the true chord length is 2*radius*sin(angle/2)*sin(theta), theta = angle
# between the rotation axis and the position vector, so this is an upper bound (sin(theta)
# <= 1), not the precise value. Both are recorded so the report can state honestly why they
# differ, rather than silently picking whichever one happens to match.
expected_displacement_from_quaternion_m = angle_rad * orbit_radius_m
axis_norm = math.sqrt(x * x + y * y + z * z)
axis = [x / axis_norm, y / axis_norm, z / axis_norm] if axis_norm > 0 else [0.0, 0.0, 0.0]
cos_theta = sum(a * p for a, p in zip(axis, pos_m)) / orbit_radius_m if axis_norm > 0 else 0.0
theta_rad = math.acos(min(1.0, max(-1.0, cos_theta)))
precise_expected_displacement_m = 2.0 * orbit_radius_m * math.sin(angle_rad / 2.0) * math.sin(theta_rad)

fixture = {
    "meta": {
        "source": "tests/fixtures/demo_two_instance.runproducts.bin (drms/demo_two_instance.*, "
                   "a real av-run binary)",
        "groundTruth": "altavista.frames.FrameRegistry.convert() (real GMAT CoordinateConverter, "
                        "read-only)",
        "quaternionSource": "RunProducts.frames[EarthICRF].fixed_rotation_q, as actually "
                             "computed by av_kernel::drm::executor::fill_fixed_rotations",
        "quaternionConvention": "wire order [w, x, y, z], parent (EarthMJ2000Eq) -> this "
                                 "(EarthICRF)",
        "note": "posMJ2000EqM is demo_flt's own last trajectory sample position (EarthMJ2000Eq, "
                "metres); icrfGroundTruthM is that same physical point's position converted to "
                "EarthICRF directly through GMAT, at the sample's own real epoch",
    },
    "quaternion": quaternion,
    "epochA1mjd": epoch_a1mjd,
    "epochTaiNs": last.tai_ns,
    "posMJ2000EqM": pos_m,
    "icrfGroundTruthM": icrf_ground_truth_m,
    "orbitRadiusM": orbit_radius_m,
    "measuredDisplacementM": displacement_m,
    "expectedDisplacementFromQuaternionM": expected_displacement_from_quaternion_m,
    "rotationAngleRad": angle_rad,
    "angleToPositionVectorRad": theta_rad,
    "preciseExpectedDisplacementM": precise_expected_displacement_m,
}
OUT_PATH.write_text(json.dumps(fixture, indent=1))
print(f"wrote {OUT_PATH}")
print(f"  quaternion (w,x,y,z) = {quaternion}")
print(f"  orbit radius = {orbit_radius_m:.3f} m")
print(f"  rotation angle from quaternion = {angle_rad:.6e} rad ({math.degrees(angle_rad) * 3600:.6f} arcsec)")
print(f"  angle between rotation axis and position vector (theta) = {math.degrees(theta_rad):.3f} deg, sin(theta) = {math.sin(theta_rad):.6f}")
print(f"  naive upper-bound expected displacement (angle * radius) = {expected_displacement_from_quaternion_m:.6f} m")
print(f"  precise expected displacement (2*radius*sin(angle/2)*sin(theta)) = {precise_expected_displacement_m:.6f} m")
print(f"  measured displacement (GMAT convert ground truth) = {displacement_m:.6f} m")
