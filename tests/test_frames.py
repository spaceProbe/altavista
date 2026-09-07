"""Tests for altavista.frames.FrameRegistry (M1.2), run against a real GMAT process.

GMAT is a process-wide singleton (altavista/bodies.py's module docstring): every test in
this file shares one GMAT configuration, built up incrementally with uniquely-named
objects and existence-checked (``gmat.Exists``) before construction, exactly like
``altavista/bodies.py`` and ``altavista/frames.py`` do. Nothing here calls ``gmat.LoadScript``
(which wipes the configuration) or ``gmat.SaveScript`` (which segfaults). Do not run this
file under pytest-xdist -- these tests are not safe to run across multiple processes.

Fixtures are module-scoped so each GMAT object is built once and reused; pytest resolves
fixtures by the dependency graph, not test declaration order, so tests remain safe to run
in any order.
"""
from __future__ import annotations

import math

import numpy as np
import pytest

from altavista.frames import (
    AttitudeServiceUnavailableError,
    FrameCycleError,
    FrameError,
    FrameNotRealizableError,
    FrameParentMissingError,
    FrameRegistry,
    InconsistentParentFrameError,
    MissingFieldError,
    UnknownEntityError,
    UnknownGmatAttitudeModelError,
)
from altavista.gmat_env import gmat
from altavista.pb import core_pb2
from altavista.scenario import (
    Scenario,
    _cone_ray_directions,
    _quat_apply,
    _ray_ellipsoid_intersect,
    nadir_footprint_half_angle_spherical,
)

# IAU-1976/FK5 mean obliquity of the ecliptic at J2000.0: 84381.448 arcsec.
J2000_MEAN_OBLIQUITY_DEG = 84381.448 / 3600.0
ROUND_TRIP_REL_TOL = 1e-9

# A representative (eccentric-looking) LEO-ish Cartesian state, km / km-s, used
# throughout. Deliberately not axis-aligned so orthonormality/alignment checks and
# round trips are not accidentally trivial.
_SAT_EPOCH_A1MJD = 21545.0
_SAT_STATE_KM = [6878.137, 512.4, -188.9, -0.221, 7.4013, 2.0110]


def _rel_err(a, b):
    return max(abs(x - y) / max(abs(x), 1.0) for x, y in zip(a, b))


def _state_m(state_km):
    return [x * 1000.0 for x in state_km]


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def g():
    return gmat()


@pytest.fixture(scope="module")
def registry(g):
    return FrameRegistry()


@pytest.fixture(scope="module")
def earth_eq(registry):
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_EarthMJ2000Eq", body="Earth", axes=core_pb2.AXES_KIND_MJ2000_EQ))


@pytest.fixture(scope="module")
def earth_ec(registry):
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_EarthMJ2000Ec", body="Earth", axes=core_pb2.AXES_KIND_MJ2000_EC))


@pytest.fixture(scope="module")
def earth_fixed(registry):
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_EarthFixed", body="Earth", axes=core_pb2.AXES_KIND_BODY_FIXED))


@pytest.fixture(scope="module")
def test_spacecraft(g):
    """A GMAT Spacecraft built once and reused across tests (see module docstring)."""
    name = "gv_test_frames_sat"
    if not g.Exists(name):
        sat = g.Construct("Spacecraft", name)
        sat.SetField("DateFormat", "A1ModJulian")
        sat.SetField("Epoch", repr(_SAT_EPOCH_A1MJD))
        sat.SetField("CoordinateSystem", "EarthMJ2000Eq")
        sat.SetField("DisplayStateType", "Cartesian")
        for k, v in zip(("X", "Y", "Z", "VX", "VY", "VZ"), _SAT_STATE_KM):
            sat.SetField(k, v)
        g.Initialize()
    return name


@pytest.fixture(scope="module")
def ric_frame(registry, test_spacecraft):
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_ric", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
        reference_entity_id=test_spacecraft, reference_body="Earth"))


@pytest.fixture(scope="module")
def vnb_frame(registry, test_spacecraft):
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_vnb", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_VNB,
        reference_entity_id=test_spacecraft, reference_body="Earth"))


@pytest.fixture(scope="module")
def vvlh_frame(registry, test_spacecraft):
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_vvlh", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_VVLH,
        reference_entity_id=test_spacecraft, reference_body="Earth"))


@pytest.fixture(scope="module")
def enu_frame(registry):
    geo = core_pb2.Geodetic(body="Earth", latitude_rad=math.radians(28.5),
                             longitude_rad=math.radians(-80.6), height_m=10.0)
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_enu", axes=core_pb2.AXES_KIND_ENU, origin_geodetic=geo))


@pytest.fixture(scope="module")
def ned_frame(registry):
    geo = core_pb2.Geodetic(body="Earth", latitude_rad=math.radians(28.5),
                             longitude_rad=math.radians(-80.6), height_m=10.0)
    return registry.register(core_pb2.FrameDefinition(
        id="test_frames_ned", axes=core_pb2.AXES_KIND_NED, origin_geodetic=geo))


# --------------------------------------------------------------------------- obliquity
def test_obliquity(registry, earth_eq, earth_ec):
    """MJ2000Eq -> MJ2000Ec is a pure rotation about +X by the J2000 mean obliquity."""
    R = registry.rotation_matrix(earth_eq.id, earth_ec.id, _SAT_EPOCH_A1MJD)

    # Rotation about +X: R = [[1,0,0],[0,cos e,sin e],[0,-sin e,cos e]] (row-vector
    # convention consistent with GMAT's GetLastRotationMatrix / bodies.py's usage).
    assert R[0][0] == pytest.approx(1.0, abs=1e-12)
    assert R[0][1] == pytest.approx(0.0, abs=1e-10)
    assert R[0][2] == pytest.approx(0.0, abs=1e-10)
    assert R[1][0] == pytest.approx(0.0, abs=1e-10)
    assert R[2][0] == pytest.approx(0.0, abs=1e-10)

    # GMAT's GetLastRotationMatrix() here returns the matrix that rotates a vector by
    # -obliquity about +X (equivalently, it is the *equatorial-to-ecliptic* passive
    # rotation transposed relative to the naive +obliquity-about-+X convention); the
    # sign is a matrix-convention fact about GetLastRotationMatrix, not a free
    # parameter, so the test reports the signed value and asserts on the magnitude.
    measured_deg = math.degrees(math.atan2(R[2][1], R[1][1]))
    print(f"\n[test_obliquity] measured obliquity = {measured_deg!r} deg "
          f"(signed; expected magnitude {J2000_MEAN_OBLIQUITY_DEG!r} deg, "
          f"diff = {abs(abs(measured_deg) - J2000_MEAN_OBLIQUITY_DEG):.3e} deg)")
    # Tight tolerance: GMAT's own MJ2000Ec uses IAU-1976/FK5 with the 1980 nutation
    # update, so this checks GMAT's constant against the IAU-76 mean-obliquity value,
    # not a numerical-precision bound.
    assert abs(measured_deg) == pytest.approx(J2000_MEAN_OBLIQUITY_DEG, abs=1e-4)


# --------------------------------------------------------------------------- RIC axes
def test_ric_axes_orthonormal_and_radial(registry, earth_eq, ric_frame, test_spacecraft):
    R = registry.rotation_matrix(earth_eq.id, ric_frame.id, _SAT_EPOCH_A1MJD)

    residual = float(np.max(np.abs(R @ R.T - np.eye(3))))
    print(f"\n[test_ric_axes] orthonormality residual (max|R R^T - I|) = {residual!r}")
    assert residual < 1e-12

    pos_hat = np.array(_SAT_STATE_KM[0:3]) / np.linalg.norm(_SAT_STATE_KM[0:3])
    r_axis = R[0, :] / np.linalg.norm(R[0, :])
    dot = float(np.dot(r_axis, pos_hat))
    print(f"[test_ric_axes] R-axis . spacecraft position unit vector = {dot!r}")
    assert dot == pytest.approx(1.0, abs=1e-9)


# --------------------------------------------------------------------------- round trips
@pytest.mark.parametrize("dst_fixture_name", ["earth_ec", "earth_fixed", "ric_frame", "vnb_frame", "vvlh_frame"])
def test_round_trip(request, registry, earth_eq, dst_fixture_name):
    dst = request.getfixturevalue(dst_fixture_name)
    state_m = _state_m(_SAT_STATE_KM)
    out = registry.convert(state_m, _SAT_EPOCH_A1MJD, earth_eq.id, dst.id)
    back = registry.convert(out, _SAT_EPOCH_A1MJD, dst.id, earth_eq.id)
    err = _rel_err(state_m, back)
    print(f"\n[test_round_trip] EarthMJ2000Eq <-> {dst_fixture_name} relative error = {err!r}")
    assert err < ROUND_TRIP_REL_TOL


@pytest.mark.parametrize("dst_fixture_name", ["enu_frame", "ned_frame"])
def test_round_trip_topocentric(request, registry, earth_eq, dst_fixture_name):
    dst = request.getfixturevalue(dst_fixture_name)
    state_m = _state_m(_SAT_STATE_KM)
    out = registry.convert(state_m, _SAT_EPOCH_A1MJD, earth_eq.id, dst.id)
    back = registry.convert(out, _SAT_EPOCH_A1MJD, dst.id, earth_eq.id)
    err = _rel_err(state_m, back)
    print(f"\n[test_round_trip] EarthMJ2000Eq <-> {dst_fixture_name} relative error = {err!r}")
    assert err < ROUND_TRIP_REL_TOL


def test_enu_ned_are_a_fixed_permutation_of_each_other(registry, earth_eq, enu_frame, ned_frame):
    """Sanity check on the documented ENU/NED matrices: E=E, N=N, U=-D for the same site."""
    state_m = _state_m(_SAT_STATE_KM)
    enu = registry.convert(state_m, _SAT_EPOCH_A1MJD, earth_eq.id, enu_frame.id)
    ned = registry.convert(state_m, _SAT_EPOCH_A1MJD, earth_eq.id, ned_frame.id)
    assert enu[0] == pytest.approx(ned[1], abs=1e-6)   # E
    assert enu[1] == pytest.approx(ned[0], abs=1e-6)   # N
    assert enu[2] == pytest.approx(-ned[2], abs=1e-6)  # U = -D


# --------------------------------------------------------------------------- validation
def test_missing_field_enu_origin_geodetic(registry):
    with pytest.raises(MissingFieldError) as exc_info:
        registry.register(core_pb2.FrameDefinition(id="test_frames_bad_enu", axes=core_pb2.AXES_KIND_ENU))
    assert exc_info.value.field == "origin_geodetic"


def test_missing_field_ric_reference_entity(registry):
    with pytest.raises(MissingFieldError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_bad_ric", entity_id="x", axes=core_pb2.AXES_KIND_RIC, reference_body="Earth"))
    assert exc_info.value.field == "reference_entity_id"


def test_missing_field_ric_reference_body(registry):
    with pytest.raises(MissingFieldError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_bad_ric2", entity_id="x", axes=core_pb2.AXES_KIND_RIC, reference_entity_id="x"))
    assert exc_info.value.field == "reference_body"


def test_unknown_reference_entity(registry, test_spacecraft):
    with pytest.raises(UnknownEntityError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_bad_ric3", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
            reference_entity_id="does_not_exist", reference_body="Earth"))
    assert exc_info.value.entity_id == "does_not_exist"


def test_platform_body_not_realizable(registry):
    with pytest.raises(FrameNotRealizableError):
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_bad_platform", entity_id="x", axes=core_pb2.AXES_KIND_PLATFORM_BODY))
    # Typed as a NotImplementedError specifically (task requirement), not just FrameError.
    with pytest.raises(NotImplementedError):
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_bad_platform2", entity_id="x", axes=core_pb2.AXES_KIND_PLATFORM_BODY))


def test_local_cartesian_registers_but_does_not_convert(registry, earth_eq):
    lc = registry.register(core_pb2.FrameDefinition(
        id="test_frames_lc", axes=core_pb2.AXES_KIND_LOCAL_CARTESIAN, entity_id="unused"))
    assert lc.gmat_name  # non-empty per core.proto's "empty until validated"
    with pytest.raises(FrameError):
        registry.convert([0.0] * 6, _SAT_EPOCH_A1MJD, lc.id, earth_eq.id)
    with pytest.raises(FrameError):
        registry.convert([0.0] * 6, _SAT_EPOCH_A1MJD, earth_eq.id, lc.id)


# --------------------------------------------------------------------------- gmat_name
def test_gmat_name_populated_after_validation(earth_eq, earth_ec, earth_fixed, ric_frame, enu_frame):
    assert earth_eq.gmat_name == "EarthMJ2000Eq"
    assert earth_ec.gmat_name == "EarthMJ2000Ec"
    assert earth_fixed.gmat_name == "EarthBodyFixed"  # f"{origin}{axes}" naming, see _register_body_axes
    assert ric_frame.gmat_name  # a real GMAT ObjectReferenced CS name, non-empty
    assert enu_frame.gmat_name  # names the underlying GMAT Topocentric (SEZ) CS


# ============================================================================= M3.3
# --------------------------------------------------------------- parent_frame_id fill (question 76)
def test_parent_fill_body_axes_and_local_cartesian_get_root(registry, earth_eq, earth_ec, earth_fixed):
    """Body-centred inertial/fixed frames, and the frameless LOCAL_CARTESIAN, are the
    top of the tree by construction: their omitted parent gets filled with "" (root)."""
    assert earth_eq.parent_frame_id == ""
    assert earth_ec.parent_frame_id == ""
    assert earth_fixed.parent_frame_id == ""

    icrf = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_icrf", body="Earth", axes=core_pb2.AXES_KIND_ICRF))
    assert icrf.parent_frame_id == ""

    lc = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_lc", axes=core_pb2.AXES_KIND_LOCAL_CARTESIAN, entity_id="unused"))
    assert lc.parent_frame_id == ""


def test_parent_fill_ric_vnb_vvlh_get_reference_body_mj2000eq(registry, ric_frame, vnb_frame, vvlh_frame):
    """RIC/VNB/VVLH's omitted parent is filled with the reference body's MJ2000_EQ
    frame, and that frame really exists in the registry (not just a dangling string)."""
    for fd in (ric_frame, vnb_frame, vvlh_frame):
        assert fd.parent_frame_id
        parent = registry.get(fd.parent_frame_id)  # raises if not actually registered
        assert parent.axes == core_pb2.AXES_KIND_MJ2000_EQ
        assert parent.WhichOneof("origin") == "body"
        assert parent.body == "Earth"
        assert parent.gmat_name  # a real GMAT CoordinateSystem was built for it


def test_parent_fill_enu_ned_get_body_fixed(registry, enu_frame, ned_frame):
    """ENU/NED's omitted parent is filled with the origin body's BODY_FIXED frame,
    which really exists in the registry."""
    for fd in (enu_frame, ned_frame):
        assert fd.parent_frame_id
        parent = registry.get(fd.parent_frame_id)
        assert parent.axes == core_pb2.AXES_KIND_BODY_FIXED
        assert parent.WhichOneof("origin") == "body"
        assert parent.body == "Earth"
        assert parent.gmat_name


def test_parent_fill_reuses_existing_matching_parent(registry, earth_eq, test_spacecraft):
    """Registering a second RIC frame about the same reference_body must not create a
    second Earth/MJ2000_EQ registry entry -- it reuses whichever one already exists."""
    def _earth_mj2000eq_ids():
        return sorted(fid for fid, d in registry._defs.items()
                       if d.axes == core_pb2.AXES_KIND_MJ2000_EQ
                       and d.WhichOneof("origin") == "body" and d.body == "Earth")

    before = _earth_mj2000eq_ids()
    assert before  # earth_eq fixture guarantees at least one is already registered

    new_ric = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_reuse_ric", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
        reference_entity_id=test_spacecraft, reference_body="Earth"))

    after = _earth_mj2000eq_ids()
    assert after == before  # no new Earth/MJ2000_EQ frame was auto-registered
    assert new_ric.parent_frame_id in before


def test_parent_fill_auto_registers_missing_parent(registry, test_spacecraft):
    """A reference_body with no existing MJ2000_EQ (or BODY_FIXED) frame registered yet
    gets one auto-registered -- proven here with Luna, untouched by the Earth fixtures."""
    ric = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_ric_luna", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
        reference_entity_id=test_spacecraft, reference_body="Luna"))
    ric_parent = registry.get(ric.parent_frame_id)
    assert ric_parent.axes == core_pb2.AXES_KIND_MJ2000_EQ
    assert ric_parent.body == "Luna"
    assert ric_parent.gmat_name == "LunaMJ2000Eq"

    geo = core_pb2.Geodetic(body="Luna", latitude_rad=0.1, longitude_rad=0.2, height_m=0.0)
    enu = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_enu_luna", axes=core_pb2.AXES_KIND_ENU, origin_geodetic=geo))
    enu_parent = registry.get(enu.parent_frame_id)
    assert enu_parent.axes == core_pb2.AXES_KIND_BODY_FIXED
    assert enu_parent.body == "Luna"
    assert enu_parent.gmat_name == "LunaBodyFixed"


def test_parent_fill_supplied_consistent_parent_is_kept(registry, test_spacecraft):
    """A caller-supplied parent_frame_id that matches the fill rule is kept as-is (not
    replaced with some other id the rule would also have accepted)."""
    probe = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_probe_ric", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
        reference_entity_id=test_spacecraft, reference_body="Earth"))
    computed = probe.parent_frame_id

    fd = registry.register(core_pb2.FrameDefinition(
        id="test_frames_p76_consistent_ric", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
        reference_entity_id=test_spacecraft, reference_body="Earth", parent_frame_id=computed))
    assert fd.parent_frame_id == computed


def test_parent_fill_supplied_inconsistent_parent_rejected(registry, test_spacecraft):
    with pytest.raises(InconsistentParentFrameError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_p76_bad_parent", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_RIC,
            reference_entity_id=test_spacecraft, reference_body="Earth",
            parent_frame_id="not_the_right_parent"))
    assert exc_info.value.frame_id == "test_frames_p76_bad_parent"
    assert exc_info.value.supplied == "not_the_right_parent"


# --------------------------------------------------------------------- tree operations (question 76)
def test_tree_children_sorted_and_root_listing(registry, ric_frame, vnb_frame, vvlh_frame):
    parent_id = ric_frame.parent_frame_id  # shared by ric/vnb/vvlh (same reference_body)
    assert vnb_frame.parent_frame_id == parent_id
    assert vvlh_frame.parent_frame_id == parent_id

    kids = registry.children(parent_id)
    assert kids == sorted(kids)  # deterministic order, not registration order
    assert {ric_frame.id, vnb_frame.id, vvlh_frame.id} <= set(kids)

    roots = registry.children("")  # frame_id="" default lists the top-level frames
    assert roots == sorted(roots)
    assert parent_id in roots  # the auto-registered/reused MJ2000_EQ frame is top-level


def test_tree_children_unregistered_frame_raises(registry):
    with pytest.raises(FrameError):
        registry.children("no_such_frame_id")


def test_tree_path_to_root(registry, ric_frame):
    path = registry.path_to_root(ric_frame.id)
    assert path[0] == ric_frame.id
    assert path[-1] == ric_frame.parent_frame_id
    # every step's stored parent chains to the next entry, ending just below the root
    for i in range(len(path) - 1):
        assert registry.get(path[i]).parent_frame_id == path[i + 1]
    assert registry.get(path[-1]).parent_frame_id == ""


def test_tree_cycle_detected(test_spacecraft):
    """A cycle is only reachable through PLATFORM_BODY's verbatim reference_frame_id (the
    only rule that is not auto-registered/derived) -- construct one via the public API."""
    reg = FrameRegistry()
    att_x_to_y = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing", reference_frame_id="p76_cyc_y")
    reg.register(core_pb2.FrameDefinition(
        id="p76_cyc_x", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
        reference_entity_id=test_spacecraft, attitude_source=att_x_to_y))
    att_y_to_x = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing", reference_frame_id="p76_cyc_x")
    reg.register(core_pb2.FrameDefinition(
        id="p76_cyc_y", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
        reference_entity_id=test_spacecraft, attitude_source=att_y_to_x))

    with pytest.raises(FrameCycleError) as exc_info:
        reg.path_to_root("p76_cyc_x")
    assert exc_info.value.frame_id == "p76_cyc_x"


def test_tree_missing_parent_detected(test_spacecraft):
    reg = FrameRegistry()
    att = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing", reference_frame_id="p76_does_not_exist")
    reg.register(core_pb2.FrameDefinition(
        id="p76_dangling", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
        reference_entity_id=test_spacecraft, attitude_source=att))

    with pytest.raises(FrameParentMissingError) as exc_info:
        reg.path_to_root("p76_dangling")
    assert exc_info.value.missing_parent_id == "p76_does_not_exist"


# --------------------------------------------------------------- PLATFORM_BODY / attitude_source (question 72)
def test_platform_body_gmat_attitude_model_registers_and_declines_convert(registry, earth_eq, test_spacecraft):
    att = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing", reference_frame_id=earth_eq.id)
    fd = registry.register(core_pb2.FrameDefinition(
        id="test_frames_platform_gmat", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
        reference_entity_id=test_spacecraft, attitude_source=att))
    assert fd.gmat_name  # declared: non-empty per "empty until validated"
    # parent_frame_id mirrors attitude_source.reference_frame_id verbatim (question 76)
    assert fd.parent_frame_id == earth_eq.id

    with pytest.raises(AttitudeServiceUnavailableError):
        registry.convert([0.0] * 6, _SAT_EPOCH_A1MJD, earth_eq.id, fd.id)
    with pytest.raises(AttitudeServiceUnavailableError):
        registry.convert([0.0] * 6, _SAT_EPOCH_A1MJD, fd.id, earth_eq.id)
    # AttitudeServiceUnavailableError is deliberately not a FrameNotRealizableError --
    # the frame IS declared/validated, unlike the no-attitude_source case below.
    with pytest.raises(AttitudeServiceUnavailableError):
        try:
            registry.convert([0.0] * 6, _SAT_EPOCH_A1MJD, earth_eq.id, fd.id)
        except FrameNotRealizableError:
            pytest.fail("convert() through a declared PLATFORM_BODY must not raise FrameNotRealizableError")


def test_platform_body_entity_attitude_stream_registers(registry, earth_eq, test_spacecraft):
    """The other attitude_source oneof arm also registers -- entity_attitude_stream is a
    CDM concept this module does not validate against GMAT (see FRAMES.md)."""
    att = core_pb2.AttitudeSource(entity_attitude_stream="some_attitude_publisher",
                                   reference_frame_id=earth_eq.id)
    fd = registry.register(core_pb2.FrameDefinition(
        id="test_frames_platform_stream", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
        reference_entity_id=test_spacecraft, attitude_source=att))
    assert fd.gmat_name
    assert fd.parent_frame_id == earth_eq.id


def test_platform_body_unknown_gmat_attitude_model_rejected(registry, earth_eq, test_spacecraft):
    """Validated against real GMAT, not a hardcoded Python list -- GMAT rejects the name
    with its own exception, which this module surfaces as a typed error."""
    att = core_pb2.AttitudeSource(gmat_attitude_model="TotallyBogusAttitudeModel",
                                   reference_frame_id=earth_eq.id)
    with pytest.raises(UnknownGmatAttitudeModelError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_bad_model", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            reference_entity_id=test_spacecraft, attitude_source=att))
    assert exc_info.value.model_name == "TotallyBogusAttitudeModel"


def test_platform_body_unknown_reference_entity_rejected(registry, earth_eq):
    att = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing", reference_frame_id=earth_eq.id)
    with pytest.raises(UnknownEntityError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_bad_entity", entity_id="x", axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            reference_entity_id="does_not_exist", attitude_source=att))
    assert exc_info.value.entity_id == "does_not_exist"


def test_platform_body_missing_reference_frame_id_rejected(registry, test_spacecraft):
    att = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing")  # reference_frame_id unset
    with pytest.raises(MissingFieldError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_no_ref_frame", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            reference_entity_id=test_spacecraft, attitude_source=att))
    assert exc_info.value.field == "attitude_source.reference_frame_id"


def test_platform_body_missing_reference_entity_rejected(registry, earth_eq):
    att = core_pb2.AttitudeSource(gmat_attitude_model="NadirPointing", reference_frame_id=earth_eq.id)
    with pytest.raises(MissingFieldError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_no_ref_entity", entity_id="x", axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            attitude_source=att))
    assert exc_info.value.field == "reference_entity_id"


def test_platform_body_attitude_source_without_oneof_rejected(registry, earth_eq, test_spacecraft):
    att = core_pb2.AttitudeSource(reference_frame_id=earth_eq.id)  # neither oneof arm set
    with pytest.raises(MissingFieldError) as exc_info:
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_no_source", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            reference_entity_id=test_spacecraft, attitude_source=att))
    assert exc_info.value.field == "attitude_source.source"


def test_platform_body_without_attitude_source_still_rejected(registry, test_spacecraft):
    """Unchanged from before this frame carried attitude_source: still unconditional."""
    with pytest.raises(FrameNotRealizableError):
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_still_bad", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            reference_entity_id=test_spacecraft))
    with pytest.raises(NotImplementedError):
        registry.register(core_pb2.FrameDefinition(
            id="test_frames_platform_still_bad2", entity_id=test_spacecraft, axes=core_pb2.AXES_KIND_PLATFORM_BODY,
            reference_entity_id=test_spacecraft))


# --------------------------------------------------------------------------- attitude sampling & sensor footprint (M6.3)
# altavista.scenario.Scenario builds its own named GMAT objects (Spacecraft, ForceModel,
# Propagator...) through the same Construct/Exists-guarded pattern as this file's other
# fixtures (module docstring); it shares the one process-wide GMAT configuration these
# tests already use, so no LoadScript/SaveScript risk here either.
_M63_KEPLERIAN = dict(SMA=6778.0, ECC=0.0005, INC=51.64, RAAN=120.0, AOP=30.0, TA=0.0)


def test_nadir_pointing_default_body_alignment_axis_is_plus_x():
    """GMAT's NadirPointing default `BodyAlignmentVector` is +X (not +Z) -- verified
    against third_party/gmat-src/src/base/attitude/Attitude.cpp's
    `bodyAlignmentVector.Set(1.0, 0.0, 0.0)` default, and confirmed live here by reading
    the field back off a freshly constructed spacecraft rather than trusting the source
    comment alone."""
    sc = Scenario("test_frames_nadir_axis_check")
    sat = sc.spacecraft("gv_tf_nadir_axis_sat", epoch="01 Jan 2026 00:00:00.000",
                       keplerian=_M63_KEPLERIAN, Attitude="NadirPointing")
    assert sat.obj.GetRealParameter("BodyAlignmentVectorX") == 1.0
    assert sat.obj.GetRealParameter("BodyAlignmentVectorY") == 0.0
    assert sat.obj.GetRealParameter("BodyAlignmentVectorZ") == 0.0


def test_nadir_pointing_plus_x_body_axis_tracks_nadir_to_1e_minus_9():
    """The measured part of M6.3: sample a NadirPointing spacecraft's attitude stream
    across a real propagation and confirm the body axis GMAT actually points at nadir
    (determined above to be +X, GMAT's own default -- not assumed) tracks the
    instantaneous nadir direction (``-pos/|pos|``, in the same scenario frame the
    attitude stream is expressed in) to 1e-9, every sample, not loosened."""
    sc = Scenario("test_frames_nadir_residual", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_nadir_residual_sat", epoch="01 Jan 2026 00:00:00.000",
                       keplerian=_M63_KEPLERIAN, Attitude="NadirPointing")
    sc.propagate(sat, hours=3, step=60)
    tr = sat.trajectory
    assert len(tr.attitude) == len(tr.t) and len(tr.t) > 100
    worst = 0.0
    for pos, q in zip(tr.pos, tr.attitude):
        pos = np.array(pos)
        nadir = -pos / np.linalg.norm(pos)
        body_x_in_frame = _quat_apply(q, [1.0, 0.0, 0.0])
        worst = max(worst, float(np.linalg.norm(body_x_in_frame - nadir)))
    assert worst < 1e-9, f"worst NadirPointing +X-vs-nadir residual {worst!r} >= 1e-9"


def test_nadir_pointing_across_a_maneuver_still_tracks_nadir():
    """Same check, but spanning an impulsive maneuver (Scenario.maneuver appends its own
    trajectory/attitude sample outside propagate()'s record() loop -- this is the
    regression this test guards)."""
    sc = Scenario("test_frames_nadir_maneuver", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_nadir_maneuver_sat", epoch="01 Jan 2026 00:00:00.000",
                       keplerian=_M63_KEPLERIAN, Attitude="NadirPointing")
    sc.propagate(sat, hours=1, step=60)
    sc.maneuver(sat, dv=[0.02, 0, 0], frame="VNB")
    sc.propagate(sat, hours=1, step=60)
    tr = sat.trajectory
    assert len(tr.attitude) == len(tr.t)
    worst = max(
        float(np.linalg.norm(_quat_apply(q, [1.0, 0.0, 0.0]) - (-np.array(pos) / np.linalg.norm(pos))))
        for pos, q in zip(tr.pos, tr.attitude)
    )
    assert worst < 1e-9


def _rv_after_maneuver(sc, sat, dv, frame):
    """[r, v] (km, km/s) immediately after ``sc.maneuver(sat, dv, frame=frame)``, plus the
    pre-burn [r, v] -- used by the M11.3 tests below to check the applied delta-v directly,
    the same way ``goldens/gen_leo_1day_maneuver_*.py`` check ``state_pre_burn``/
    ``state_post_burn``."""
    r0, v0 = list(sat.state[0:3]), list(sat.state[3:6])
    sc.maneuver(sat, dv=dv, frame=frame)
    r1, v1 = list(sat.state[0:3]), list(sat.state[3:6])
    return r0, v0, r1, v1


def test_maneuver_ric_burn_fires_a_real_gmat_impulsive_burn_and_is_radial():
    """M11.3 (question 102): frame="RIC" is realized as a real GMAT ImpulsiveBurn whose
    CoordinateSystem is an ObjectReferenced RIC system (XAxis=R, ZAxis=N, question 73). A
    pure dv=(x,0,0) burn must therefore land exactly along the spacecraft's own radial unit
    vector r/|r| -- checked against the actual r/v this run's Scenario.propagate produced
    (never a second, independently-derived R/N/in-track basis), the same "compare the real
    output, don't re-derive the formula" discipline goldens/gen_leo_1day_maneuver_*.py use.
    """
    sc = Scenario("test_frames_maneuver_ric_radial", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_mvr_ric_radial_sat", epoch="01 Jan 2026 00:00:00.000", keplerian=_M63_KEPLERIAN)
    sc.propagate(sat, hours=1, step=60)
    r0, v0, r1, v1 = _rv_after_maneuver(sc, sat, [0.02, 0.0, 0.0], "RIC")
    assert r0 == r1, "an impulsive maneuver must not change position"
    r0_hat = np.array(r0) / np.linalg.norm(r0)
    dv_applied = np.array(v1) - np.array(v0)
    assert np.linalg.norm(dv_applied - 0.02 * r0_hat) < 1e-12, f"{dv_applied!r} vs 0.02 * r_hat = {0.02 * r0_hat!r}"


def test_maneuver_ric_and_gmat_native_lvlh_apply_the_identical_dv():
    """M11.3 (question 102)'s central empirical finding, pinned as a regression on the
    Python side (crates/av-kernel/tests/drm_maneuver.rs::
    drm_maneuver_axes_kind_ric_reproduces_gmats_native_lvlh_burn pins the same finding on
    the Rust side): GMAT's own ImpulsiveBurn ``Axes = LVLH`` (frame="LVLH", CoordinateSystem
    = Local / Origin=Earth / Axes=LVLH) is numerically identical to the ObjectReferenced RIC
    system frame="RIC" builds through FrameRegistry (XAxis=R, ZAxis=N) -- for an arbitrary,
    non-axis-aligned dv, not just a symmetric one. Both are real GMAT ImpulsiveBurn.Fire()
    calls; nothing here re-derives or reorients either result.
    """
    dv = [0.011, -0.006, 0.017]  # deliberately asymmetric/non-trivial, km/s

    sc_ric = Scenario("test_frames_maneuver_ric_vs_lvlh_ric", frame="EarthMJ2000Eq")
    sat_ric = sc_ric.spacecraft("gv_tf_mvr_ric_vs_lvlh_ric_sat", epoch="01 Jan 2026 00:00:00.000", keplerian=_M63_KEPLERIAN)
    sc_ric.propagate(sat_ric, hours=1, step=60)
    r0_ric, v0_ric, r1_ric, v1_ric = _rv_after_maneuver(sc_ric, sat_ric, dv, "RIC")

    sc_lvlh = Scenario("test_frames_maneuver_ric_vs_lvlh_lvlh", frame="EarthMJ2000Eq")
    sat_lvlh = sc_lvlh.spacecraft("gv_tf_mvr_ric_vs_lvlh_lvlh_sat", epoch="01 Jan 2026 00:00:00.000", keplerian=_M63_KEPLERIAN)
    sc_lvlh.propagate(sat_lvlh, hours=1, step=60)
    r0_lvlh, v0_lvlh, r1_lvlh, v1_lvlh = _rv_after_maneuver(sc_lvlh, sat_lvlh, dv, "LVLH")

    # Both scenarios propagate the identical orbit/epoch, so the pre-burn state must agree
    # (sanity: this comparison is meaningful) and the applied dv must be bit-identical.
    assert np.allclose(r0_ric, r0_lvlh) and np.allclose(v0_ric, v0_lvlh)
    dv_ric = np.array(v1_ric) - np.array(v0_ric)
    dv_lvlh = np.array(v1_lvlh) - np.array(v0_lvlh)
    assert np.linalg.norm(dv_ric - dv_lvlh) < 1e-12, f"RIC dv={dv_ric!r} vs GMAT-native-LVLH dv={dv_lvlh!r}"


def test_maneuver_gmat_native_lvlh_does_not_match_the_ratified_axes_kind_vvlh_convention():
    """The other half of question 102's finding (M12.4, question 106: the platform's
    convention this test pins is now named ``AXES_KIND_VVLH``, not ``AXES_KIND_LVLH`` --
    the *docstring and variable names* below were updated by the rename, not the physics):
    GMAT's ImpulsiveBurn ``Axes = LVLH`` (frame="LVLH") does NOT match this platform's
    ratified AXES_KIND_VVLH convention (Z=-R, Y=-N, X=N x R, question 73, altavista/frames.py's
    _OBJECT_REFERENCED_FIELDS) -- measured directly here by building that ratified rotation
    from the same r/v this run's Scenario.propagate produced and comparing it to
    Scenario.maneuver's own frame="LVLH" result, never by asserting agreement and hoping.
    See altavista/FRAMES.md's "GMAT ImpulsiveBurn Axes = LVLH vs AXES_KIND_VVLH" section.
    """
    dv = [0.011, -0.006, 0.017]  # same as the RIC-vs-LVLH test above

    sc = Scenario("test_frames_maneuver_lvlh_vs_ratified", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_mvr_lvlh_vs_ratified_sat", epoch="01 Jan 2026 00:00:00.000", keplerian=_M63_KEPLERIAN)
    sc.propagate(sat, hours=1, step=60)
    r0, v0, r1, v1 = _rv_after_maneuver(sc, sat, dv, "LVLH")
    dv_gmat_native_lvlh = np.array(v1) - np.array(v0)

    r0, v0 = np.array(r0), np.array(v0)
    r_hat = r0 / np.linalg.norm(r0)
    n_hat = np.cross(r0, v0)
    n_hat = n_hat / np.linalg.norm(n_hat)
    in_track = np.cross(n_hat, r_hat)  # N x R, ratified VVLH's X axis
    # Ratified AXES_KIND_VVLH: X = N x R (in-track), Y = -N, Z = -R (question 73).
    dv_ratified_vvlh = dv[0] * in_track + dv[1] * (-n_hat) + dv[2] * (-r_hat)

    residual = np.linalg.norm(dv_gmat_native_lvlh - dv_ratified_vvlh)
    assert residual > 1e-6, (
        f"expected GMAT's native Axes=LVLH burn to disagree with the ratified AXES_KIND_VVLH "
        f"convention, but they matched: gmat_native={dv_gmat_native_lvlh!r} "
        f"ratified={dv_ratified_vvlh!r} residual={residual!r}")

    # And, per FRAMES.md's finding, GMAT's own Axes=LVLH burn should instead equal the X=R,
    # Z=N (RIC) triad exactly.
    dv_ric_convention = dv[0] * r_hat + dv[1] * in_track + dv[2] * n_hat
    ric_residual = np.linalg.norm(dv_gmat_native_lvlh - dv_ric_convention)
    assert ric_residual < 1e-12, f"gmat_native={dv_gmat_native_lvlh!r} vs RIC-convention={dv_ric_convention!r}"


def test_maneuver_vvlh_burn_fires_a_real_gmat_impulsive_burn_matching_the_ratified_convention():
    """M12.4 (question 106): ``frame="VVLH"`` is a new ``Scenario.maneuver`` option (this
    platform's ratified AXES_KIND_VVLH did not have a real-GMAT-burn realization before
    M12.4 -- only GMAT's *different*, literal ``Axes=LVLH`` did). Realized through
    :meth:`Scenario._fire_impulsive_burn`'s new ``"vvlh"`` branch, the same ObjectReferenced
    mechanism ``frame="RIC"`` already uses, with ``YAxis=-N, ZAxis=-R`` (question 73) instead
    of RIC's ``XAxis=R, ZAxis=N``. Checked against a hand-built rotation from the same r/v
    this run's own Scenario.propagate produced (never a second, independently-derived basis),
    for the same non-trivial dv the mismatch test above uses.
    """
    dv = [0.011, -0.006, 0.017]  # same as the RIC-vs-LVLH / GMAT-native-LVLH tests above

    sc = Scenario("test_frames_maneuver_vvlh_matches_ratified", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_mvr_vvlh_matches_ratified_sat", epoch="01 Jan 2026 00:00:00.000", keplerian=_M63_KEPLERIAN)
    sc.propagate(sat, hours=1, step=60)
    r0, v0, r1, v1 = _rv_after_maneuver(sc, sat, dv, "VVLH")
    assert r0 == r1, "an impulsive maneuver must not change position"
    dv_applied = np.array(v1) - np.array(v0)

    r0, v0 = np.array(r0), np.array(v0)
    r_hat = r0 / np.linalg.norm(r0)
    n_hat = np.cross(r0, v0)
    n_hat = n_hat / np.linalg.norm(n_hat)
    in_track = np.cross(n_hat, r_hat)  # N x R, ratified VVLH's X axis
    dv_ratified_vvlh = dv[0] * in_track + dv[1] * (-n_hat) + dv[2] * (-r_hat)

    residual = np.linalg.norm(dv_applied - dv_ratified_vvlh)
    assert residual < 1e-9, f"frame=\"VVLH\" applied={dv_applied!r} vs ratified AXES_KIND_VVLH={dv_ratified_vvlh!r} residual={residual!r}"


def test_spinner_measured_rate_matches_configured():
    """The Spinner half of M6.3's "measured, not asserted" requirement: configure a
    known constant angular velocity, sample the attitude stream at a fine enough time
    step to avoid angle-wrapping (each step's rotation must stay well under 180 deg),
    reconstruct the relative rotation between consecutive samples, and confirm the
    measured rate matches what was configured to within a small fraction of a
    deg/s."""
    configured_deg_s = 10.0
    sc = Scenario("test_frames_spinner_rate", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_spinner_sat", epoch="01 Jan 2026 00:00:00.000",
                       keplerian=_M63_KEPLERIAN, Attitude="Spinner",
                       AngularVelocityX=0.0, AngularVelocityY=0.0, AngularVelocityZ=configured_deg_s)
    sc.propagate(sat, seconds=20, step=1.0)  # 1 deg-of-rotation-per-configured-deg/s per step
    tr = sat.trajectory
    assert len(tr.attitude) == len(tr.t) and len(tr.t) > 10

    def quat_to_mat(q):
        x, y, z, w = q
        return np.array([
            [1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
            [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
            [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)],
        ])

    rates = []
    for i in range(len(tr.t) - 1):
        dt_s = (tr.t[i + 1] - tr.t[i]) * 86400.0
        m0, m1 = quat_to_mat(tr.attitude[i]), quat_to_mat(tr.attitude[i + 1])
        rel = m1.T @ m0  # body(t_i) -> body(t_i+1) rotation
        cos_theta = np.clip((np.trace(rel) - 1) / 2, -1.0, 1.0)
        rates.append(math.degrees(math.acos(cos_theta)) / dt_s)
    rates = np.array(rates)
    assert np.max(np.abs(rates - configured_deg_s)) < 1e-6, \
        f"measured Spinner rates {rates!r} deg/s vs configured {configured_deg_s} deg/s"


def test_footprint_ray_ellipsoid_matches_closed_form_nadir_radius_on_a_sphere():
    """Validates altavista.scenario's ray/cone/ellipsoid-intersection geometry (the
    sensor-footprint code) against the closed-form spherical nadir footprint formula
    (nadir_footprint_half_angle_spherical): a nadir-pointing cone of half-angle `a` at
    altitude `h` over a *sphere* of radius `R`. Uses a synthetic spacecraft placed on
    the sphere's own +Z axis (pos=(0,0,R+h), boresight=-Z) so the numeric ring-point
    central angle is exactly comparable to the closed form's Earth-central angle -- a
    perfect sphere (a_km == b_km, flattening forced to 0) so this is an apples-to-apples
    check of the *geometry code*, not a claim that the closed form is exact for the real
    (oblate) WGS84 ellipsoid -- see altavista/FRAMES.md's footprint section for the
    measured sphere-vs-ellipsoid discrepancy on a real scenario."""
    R = 6378.137
    h = 400.0
    for half_angle_deg in (2.0, 10.0, 30.0):
        a_rad = math.radians(half_angle_deg)
        pos = np.array([0.0, 0.0, R + h])
        boresight = np.array([0.0, 0.0, -1.0])
        rays = _cone_ray_directions(boresight, a_rad, 64)
        angles = []
        for ray in rays:
            hit = _ray_ellipsoid_intersect(pos, ray, R, R)
            assert hit is not None
            nadir_pt = np.array([0.0, 0.0, R])
            angles.append(math.acos(np.clip(np.dot(nadir_pt, hit) / (R * R), -1.0, 1.0)))
        closed = nadir_footprint_half_angle_spherical(a_rad, h, R)
        worst = max(abs(x - closed) for x in angles)
        assert worst < 1e-9, f"half_angle_deg={half_angle_deg}: worst angle residual {worst!r} rad >= 1e-9"


def test_footprint_beyond_horizon_returns_zero_not_nan():
    """nadir_footprint_half_angle_spherical returns 0.0 (never NaN/an exception) once
    the cone edge is beyond the horizon -- matching _ray_ellipsoid_intersect's own
    "misses entirely" (`None`) case rather than silently producing a bogus angle."""
    R, h = 6378.137, 400.0
    result = nadir_footprint_half_angle_spherical(math.radians(89.9), h, R)
    assert result == 0.0
    # and the ray-based code agrees: at that grazing angle the ray misses the sphere
    pos = np.array([0.0, 0.0, R + h])
    ray = _cone_ray_directions(np.array([0.0, 0.0, -1.0]), math.radians(89.9), 8)[0]
    assert _ray_ellipsoid_intersect(pos, ray, R, R) is None


def test_footprint_end_to_end_via_scenario_footprint():
    """altavista.scenario.Scenario.footprint() end to end: declares a footprint on a
    NadirPointing spacecraft, builds the scenario, and checks the resulting
    Footprint's shape (one center/ring per t sample, boresight center close to nadir,
    ring points at the expected angular radius from center)."""
    sc = Scenario("test_frames_footprint_e2e", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_footprint_sat", epoch="01 Jan 2026 00:00:00.000",
                       keplerian=_M63_KEPLERIAN, Attitude="NadirPointing")
    sc.propagate(sat, hours=0.33, step=60)
    sc.footprint(sat, half_angle_deg=10.0, axis=(1.0, 0.0, 0.0), n_points=32)
    data = sc.build()
    assert len(data.footprints) == 1
    fp = data.footprints[0]
    assert fp.spacecraft == sat.name
    assert len(fp.t) == len(sat.trajectory.t)
    assert len(fp.center) == len(fp.t) and len(fp.ring) == len(fp.t)
    hit_any = False
    for pos, center, ring in zip(sat.trajectory.pos, fp.center, fp.ring):
        if center is None:
            continue
        hit_any = True
        pos = np.array(pos)
        # The sub-boresight ("nadir") ground point, as a position vector *from Earth's
        # centre*, points in very nearly the same direction as the spacecraft's own
        # position vector (both lie close to the same line through Earth's centre) --
        # not the *nadir direction* (spacecraft -> Earth centre, i.e. -pos), which is
        # the opposite sign. Only a small angular offset is expected, from the body
        # being an oblate ellipsoid rather than a sphere centred exactly under the
        # spacecraft.
        pos_dir = pos / np.linalg.norm(pos)
        center_dir = np.array(center) / np.linalg.norm(center)
        assert np.dot(pos_dir, center_dir) > 0.999
        assert len(ring) % 3 == 0
    assert hit_any


def test_footprint_requires_attitude_stream():
    """footprint() raises (at build() time) rather than inventing a boresight
    direction for a spacecraft with no attitude model configured."""
    sc = Scenario("test_frames_footprint_no_attitude", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("gv_tf_footprint_no_att_sat", epoch="01 Jan 2026 00:00:00.000",
                       keplerian=_M63_KEPLERIAN)  # no Attitude field set
    sc.propagate(sat, hours=0.1, step=60)
    sc.footprint(sat, half_angle_deg=10.0)
    with pytest.raises(ValueError):
        sc.build()
