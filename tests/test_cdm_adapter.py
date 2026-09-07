"""Tests for altavista/cdm.py (M1.3): the altavista <-> altavista.v1 CDM v1 adapter and the
POST /api/cdm/trajectory endpoint.

Pure-conversion tests (units, epoch arithmetic, proto round trips, config-hash stability,
the endpoint) need no GMAT and run fast. Only
``test_a1_tai_matches_gmat_time_system_converter_post_1972`` loads GMAT, to cross-check
this module's epoch constants against GMAT's own ``TimeSystemConverter`` -- see that
test's docstring for exactly what that does and does not prove.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import pytest
from fastapi.testclient import TestClient
from google.protobuf import json_format

from altavista import cdm
from altavista.model import Event, Frame, ScenarioData, Trajectory
from altavista.pb import trajectory_pb2
from altavista.server import create_app

# --------------------------------------------------------------------------- helpers
def _traj(name="Sat", color="#fff"):
    tr = Trajectory(name=name, color=color)
    # Values chosen with short exact binary fractions (.5, .25, .125, .0625) so
    # `x * 1000.0` and `y / 1000.0` are the same floating-point operations the test and
    # the adapter both perform -- an "exact SI value" check, not just a self-consistency
    # check that would pass even if the conversion factor were wrong in a way that still
    # round-trips.
    tr.append(21545.0, [6800.5, 100.25, -50.125, -1.5, 7.625, 0.0625])
    tr.append(21545.25, [6801.5, 101.25, -49.125, -1.4, 7.525, 0.0725])
    tr.append(21544.75, [6799.5, 99.25, -51.125, -1.6, 7.725, 0.0525])  # out of order on purpose
    return tr


# --------------------------------------------------------------------------- units: km <-> m
def test_km_to_m_round_trip_is_exact_not_just_self_consistent():
    tr = _traj()
    cdm_traj = cdm.trajectory_to_cdm(tr, entity_id="Sat", frame_id="EarthMJ2000Eq")

    # Samples come out sorted by epoch even though the input trajectory was not.
    assert [s.tai_ns for s in cdm_traj.samples] == sorted(s.tai_ns for s in cdm_traj.samples)

    by_tai_ns = {s.tai_ns: s for s in cdm_traj.samples}
    for t, pos, vel in zip(tr.t, tr.pos, tr.vel):
        sample = by_tai_ns[cdm.a1mjd_to_tai_ns(t)]
        expected = [pos[0] * 1000.0, pos[1] * 1000.0, pos[2] * 1000.0,
                   vel[0] * 1000.0, vel[1] * 1000.0, vel[2] * 1000.0]
        assert list(sample.mean) == expected
        assert list(sample.cov) == []  # covariance placeholder: always empty, never fabricated

    viewer_tr = cdm.cdm_trajectory_to_viewer_json(cdm_traj)
    # viewer_tr is now epoch-sorted; find each original sample by its (exactly recovered)
    # epoch and check its km / km/s values are exactly recovered too -- not merely
    # "close", the chosen values (short binary fractions) round-trip through *1000.0/1000.0
    # bit-exactly (verified independently: see the M1.3 report).
    for i in range(len(tr.t)):
        j = viewer_tr.t.index(cdm.tai_ns_to_a1mjd(cdm.a1mjd_to_tai_ns(tr.t[i])))
        assert viewer_tr.pos[j] == tr.pos[i]
        assert viewer_tr.vel[j] == tr.vel[i]


def test_interpolation_is_always_hermite_velocity():
    cdm_traj = cdm.trajectory_to_cdm(_traj(), entity_id="Sat", frame_id="EarthMJ2000Eq")
    assert cdm_traj.interpolation == trajectory_pb2.INTERPOLATION_HERMITE_VELOCITY


def test_cdm_trajectory_to_viewer_json_rejects_non_hermite_interpolation():
    cdm_traj = cdm.trajectory_to_cdm(_traj(), entity_id="Sat", frame_id="EarthMJ2000Eq")
    cdm_traj.interpolation = trajectory_pb2.INTERPOLATION_LINEAR
    with pytest.raises(cdm.CdmAdapterError):
        cdm.cdm_trajectory_to_viewer_json(cdm_traj)


# --------------------------------------------------------------------------- epoch: A1MJD <-> TAI ns
def test_a1mjd_tai_ns_round_trip_within_a_microsecond():
    # Same bound crates/av-cdm/src/time.rs's own `a1_mjd_round_trips_within_a_microsecond`
    # test uses (f64 days at these magnitudes carry roughly microsecond resolution).
    for ns in [0, 1_700_000_000_000_000_000, -1_700_000_000_000_000_000, 63_072_010_000_000_000]:
        a1 = cdm.tai_ns_to_a1mjd(ns)
        back = cdm.a1mjd_to_tai_ns(a1)
        assert abs(back - ns) < 1000, f"residual {back - ns} ns for {ns}"


def test_tai_origin_matches_the_literal_constant_time_rs_documents():
    """Reproduces crates/av-cdm/src/time.rs::tests::a1_mjd_of_the_tai_origin_matches_the_documented_constant
    verbatim: ``Tai(0).to_a1_mjd() == 10_587.5 + 34_381_700.0 / 86_400_000_000_000.0``. This
    is a literal-value check copied from that Rust test's own assertion (not a run of the
    Rust binary -- see the module docstring below for why), so it catches a wrong constant
    or a wrong formula shape (e.g. minus instead of plus) even without a Rust toolchain.
    """
    expected = 10_587.5 + 34_381_700.0 / 86_400_000_000_000.0
    assert abs(cdm.tai_ns_to_a1mjd(0) - expected) < 1e-15


def test_leap_second_table_boundaries_match_the_shared_json_file():
    """Table-driven UTC<->TAI parity functions (utc_ns_to_tai_ns / tai_ns_to_utc_ns) are
    exact at every entry of data/time/leap_seconds.json, mirroring
    crates/av-cdm/src/time.rs::tests::to_utc_nanos_is_exact_at_every_boundary /
    from_utc_nanos_just_before_and_at_every_boundary. Pure Python + the JSON file; no GMAT.
    """
    entries = cdm._leap_table()
    assert len(entries) == 28
    assert entries[0].offset_s == 10
    assert entries[-1].offset_s == 37
    for e in entries:
        assert cdm.tai_ns_to_utc_ns(e.tai_effective_ns) == e.utc_effective_unix_ns
    for e in entries[1:]:
        # one ns before the boundary: still the old (pre-insertion) offset -- one second
        # less than the new offset in force at (and after) the boundary itself.
        before = cdm.utc_ns_to_tai_ns(e.utc_effective_unix_ns - 1)
        # exactly at the boundary: the new offset (the documented "post-insertion" rule)
        at = cdm.utc_ns_to_tai_ns(e.utc_effective_unix_ns)
        assert at == e.tai_effective_ns
        assert before == (e.utc_effective_unix_ns - 1) + (e.offset_s - 1) * 1_000_000_000


def test_pre_1972_clamps_to_the_first_offset():
    first = cdm._leap_table()[0]
    utc_ns = first.utc_effective_unix_ns - 10 * 365 * 86_400_000_000_000
    assert cdm.utc_ns_to_tai_ns(utc_ns) == utc_ns + first.offset_s * 1_000_000_000


def test_post_table_extrapolates_the_last_offset():
    last = cdm._leap_table()[-1]
    utc_ns = last.utc_effective_unix_ns + 5 * 365 * 86_400_000_000_000
    assert cdm.utc_ns_to_tai_ns(utc_ns) == utc_ns + last.offset_s * 1_000_000_000


@pytest.mark.parametrize("a1mjd", [21545.0, 25000.0, 30000.0, 40000.0, 43000.5, 30317.0])
def test_a1_tai_matches_gmat_time_system_converter_post_1972(a1mjd):
    """Cross-checks this module's A1<->TAI arithmetic against GMAT's own
    ``TimeSystemConverter`` -- the platform's other, independent, authoritative time
    implementation (already used throughout altavista/timeutil.py and altavista/frames.py).

    **Method, and what it does and does not prove.** There is no Rust toolchain (cargo /
    rustc) in this environment (`which cargo rustc` finds neither), so
    ``crates/av-cdm/src/time.rs`` cannot be compiled or run here to compare against
    directly. Instead:

    1. ``test_tai_origin_matches_the_literal_constant_time_rs_documents`` above already
       checks this module's formula against a literal value time.rs's own test suite
       asserts, copied verbatim from that file's source text.
    2. *This* test exercises the full chain -- GMAT's UTCGregorian/UTCMJD conversion (via
       ``altavista.timeutil.a1_to_utc_mjd``, GMAT's own leap-second-aware
       ``TimeSystemConverter``) -> a naive proleptic UTC nanosecond count
       (``altavista.timeutil.utc_mjd_to_datetime``, which -- like this module's own
       ``utc_ns`` -- assumes every day is exactly 86 400 SI seconds) ->
       ``cdm.utc_ns_to_tai_ns`` (this module, table-driven) -> ``cdm.tai_ns_to_a1mjd``
       (this module, constant arithmetic) -- and checks the round trip lands back within
       200 microseconds of the starting A1MJD, for epochs spanning several leap-second
       insertions between 1972 and 2017.

    This proves this module's leap-second table lookups and its A1/TAI constant agree with
    GMAT's own notion of the same UTC instant, to a stated tolerance, for dates in the
    table's supported span. It does **not** independently prove bit-for-bit equality with
    the compiled Rust crate (no toolchain here to run it), and it does not cover pre-1972
    dates: manually running this same chain at, e.g., A1MJD 10591.0 (1970-01-04) shows a
    ~2-second disagreement with GMAT, which is the *expected* result of this module's
    documented pre-1972 clamp-to-first-offset approximation (real 1970-era TAI-UTC used
    the fractional "rubber second" formula, not a flat 10 s) -- exactly the caveat
    crates/av-cdm/src/time.rs's module docs state, so this test deliberately only covers
    post-1972 epochs.
    """
    from datetime import datetime, timezone

    from altavista.timeutil import a1_to_utc_mjd, utc_mjd_to_datetime

    unix_epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
    utc_mjd = a1_to_utc_mjd(a1mjd)
    dt = utc_mjd_to_datetime(utc_mjd)
    utc_ns = round((dt - unix_epoch).total_seconds() * 1e9)
    tai_ns = cdm.utc_ns_to_tai_ns(utc_ns)
    predicted_a1mjd = cdm.tai_ns_to_a1mjd(tai_ns)

    diff_seconds = abs(predicted_a1mjd - a1mjd) * 86400.0
    assert diff_seconds < 2e-4, f"{a1mjd}: GMAT/py disagree by {diff_seconds * 1e6:.1f} microseconds"


# --------------------------------------------------------------------------- frame mapping
@pytest.mark.parametrize("axes, expected", [
    ("MJ2000Eq", cdm.core_pb2.AXES_KIND_MJ2000_EQ),
    ("MJ2000Ec", cdm.core_pb2.AXES_KIND_MJ2000_EC),
    ("BodyFixed", cdm.core_pb2.AXES_KIND_BODY_FIXED),
    ("ICRF", cdm.core_pb2.AXES_KIND_ICRF),
])
def test_frame_definition_for_maps_known_axes(axes, expected):
    fd = cdm.frame_definition_for(Frame(f"Earth{axes}", "Earth", axes))
    assert fd.axes == expected
    assert fd.body == "Earth"


def test_frame_definition_for_rejects_unmapped_axes_no_silent_default():
    with pytest.raises(cdm.UnmappedAxesError):
        cdm.frame_definition_for(Frame("MarsInertial", "Mars", "BodyInertial"))


# --------------------------------------------------------------------------- events
def test_event_to_cdm_parses_altavista_maneuver_detail():
    ev = Event(name="Reboost", t=21545.0, type="maneuver", spacecraft="Sat",
              detail="dv = 20.00 m/s (VNB)")
    cdm_ev = cdm.event_to_cdm(ev)
    assert dict(cdm_ev.values) == {"dv_mps": 20.0}
    assert cdm_ev.detail == "dv = 20.00 m/s (VNB)"
    assert cdm_ev.kind == trajectory_pb2.EVENT_KIND_MANEUVER


def test_event_to_cdm_leaves_values_empty_for_unrecognized_detail():
    """detail shapes this module cannot reliably parse (e.g. the free-form multi-line text
    Scenario._event_from_summary copies from a GMAT command summary) must never be guessed
    at -- values stays empty, detail is preserved verbatim."""
    ev = Event(name="Burn1", t=1.0, type="maneuver", spacecraft="Sat",
              detail="Delta V Vector\n  Component 1 (VNB, km/s): 0.02")
    cdm_ev = cdm.event_to_cdm(ev)
    assert dict(cdm_ev.values) == {}
    assert cdm_ev.detail == ev.detail


def test_event_to_cdm_maps_marker_type():
    ev = Event(name="Apogee", t=1.0)  # default type="marker"
    assert cdm.event_to_cdm(ev).kind == trajectory_pb2.EVENT_KIND_MARKER


# --------------------------------------------------------------------------- proto round trip
def test_trajectory_proto_round_trips():
    cdm_traj = cdm.trajectory_to_cdm(_traj(), entity_id="Sat", frame_id="EarthMJ2000Eq",
                                     config_hash="ab" * 32)
    assert trajectory_pb2.Trajectory.FromString(cdm_traj.SerializeToString()) == cdm_traj


# --------------------------------------------------------------------------- SampleKind (M15.2, question 116)
#
# `TrajectorySample.kind` (`proto/altavista/v1/trajectory.proto`) is the question-116 field
# that replaced the old `Trajectory.provenance.attributes["held_sample_tai_ns"]` side channel
# (crates/av-kernel/src/drm/executor.rs, deleted). These two tests are the Python-side half of
# M15.2's required coverage: (1) every `SampleKind` enum value the regenerated bindings declare
# survives a real serialize/parse round trip unchanged, and (2) `altavista.cdm.trajectory_to_cdm`
# -- the one place this codebase's Python side produces a `TrajectorySample` -- actually stamps
# `SAMPLE_KIND_NATIVE` on the wire rather than leaving `kind` at its zero-value default.
def test_sample_kind_round_trips_through_the_regenerated_bindings_for_every_declared_value():
    """Builds a bare `TrajectorySample` for each of the four `SampleKind` enum values
    (`SAMPLE_KIND_UNSPECIFIED`/`NATIVE`/`INTERPOLATED`/`HELD`) and serializes + reparses each
    one through the real wire encoding (`SerializeToString`/`FromString`), not just an
    in-memory attribute set/get.

    **What this would fail against:** stale, unregenerated Python bindings that predate
    question 116 -- `trajectory_pb2` would have no `kind` field on `TrajectorySample` at all
    (`AttributeError` constructing the message) and no `SAMPLE_KIND_HELD`/`SAMPLE_KIND_
    INTERPOLATED` names to import (`AttributeError` before the test body even runs); or a
    `generate.sh` regeneration that assigned the wire numbers out of step with
    `trajectory.proto` (e.g. a proto edit that renumbered the enum without rerunning codegen)
    -- the round trip would silently read back a *different* named value than was written,
    failing the per-value equality assertion below.
    """
    for kind in (trajectory_pb2.SAMPLE_KIND_UNSPECIFIED, trajectory_pb2.SAMPLE_KIND_NATIVE,
                trajectory_pb2.SAMPLE_KIND_INTERPOLATED, trajectory_pb2.SAMPLE_KIND_HELD):
        sample = trajectory_pb2.TrajectorySample(tai_ns=123, mean=[1.0, 2.0, 3.0], kind=kind)
        wire = sample.SerializeToString()
        parsed = trajectory_pb2.TrajectorySample.FromString(wire)
        assert parsed.kind == kind, f"SampleKind {kind} did not survive a serialize/parse round trip (got {parsed.kind})"
        assert parsed.tai_ns == 123 and list(parsed.mean) == [1.0, 2.0, 3.0], "kind must round-trip alongside the rest of the sample, not in place of it"


def test_trajectory_to_cdm_emits_sample_kind_native_not_the_unspecified_default():
    """`altavista.cdm.trajectory_to_cdm` (`altavista/cdm.py`) is the sole Python producer of
    `TrajectorySample` in this codebase; every sample it emits comes straight off GMAT's own
    report grid, so question 116 says it must always be `SAMPLE_KIND_NATIVE`.

    **What this would fail against:** an adapter that never sets `kind` at all -- protobuf3
    leaves an unset enum field at its zero value, so every sample would read back
    `SAMPLE_KIND_UNSPECIFIED` (0), failing the equality below; this is exactly the "a kind
    test that would still pass if every sample were stamped the wrong constant" trap the
    assertion is written to catch -- `SAMPLE_KIND_UNSPECIFIED` is also `0`/falsy, so a
    truthiness check (`assert sample.kind`) would miss this bug where an explicit equality
    check against the named constant does not.
    """
    cdm_traj = cdm.trajectory_to_cdm(_traj(), entity_id="Sat", frame_id="EarthMJ2000Eq")
    assert len(cdm_traj.samples) == 3
    for sample in cdm_traj.samples:
        assert sample.kind == trajectory_pb2.SAMPLE_KIND_NATIVE, f"tai_ns {sample.tai_ns}: altavista must emit NATIVE, got {sample.kind}"


# --------------------------------------------------------------------------- attitude (M6.3)
def _traj_with_attitude(name="Sat", color="#fff"):
    """Like `_traj()` but with a populated, parallel `attitude` stream -- deliberately
    *not* a unit quaternion (0.1, 0.2, 0.3, 0.9...) so a bug that silently dropped or
    renormalized components would be caught, not just one that got the identity
    quaternion right by accident."""
    tr = _traj(name, color)
    tr.attitude = [
        [0.1, 0.2, 0.3, 0.9273618495495704],
        [-0.1, 0.0, 0.2, 0.9746793608501049],
        [0.0, 0.0, 0.0, 1.0],
    ]
    return tr


def test_trajectory_to_cdm_carries_attitude_as_a_10_component_mean_no_proto_change():
    """M6.3: no `trajectory.proto` change was needed for attitude -- `TrajectorySample.mean`
    is already a generic "length n, in the trajectory's state space and frame" vector,
    so a populated `Trajectory.attitude` simply grows `mean` from 6 to 10 components
    (position xyz, velocity xyz, quaternion xyzw scalar-last, unchanged/uncoverted) and
    upgrades `state_space_id` to the "_attitude_quat_4" id when the caller left it at
    the plain default."""
    tr = _traj_with_attitude()
    cdm_traj = cdm.trajectory_to_cdm(tr, entity_id="Sat", frame_id="EarthMJ2000Eq")
    assert cdm_traj.state_space_id == cdm.DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE
    rows = sorted(zip(tr.t, tr.pos, tr.vel, tr.attitude), key=lambda row: row[0])
    for sample, (t, pos, vel, quat) in zip(cdm_traj.samples, rows):
        assert len(sample.mean) == 10
        assert list(sample.mean[6:10]) == pytest.approx(quat)  # dimensionless: no unit conversion


def test_trajectory_to_cdm_without_attitude_keeps_6_component_mean_and_default_id():
    """Old behaviour, byte-for-byte: a Trajectory with no attitude still gets exactly
    the pre-M6.3 6-component mean and the plain default state_space_id."""
    cdm_traj = cdm.trajectory_to_cdm(_traj(), entity_id="Sat", frame_id="EarthMJ2000Eq")
    assert cdm_traj.state_space_id == cdm.DEFAULT_STATE_SPACE_ID
    assert all(len(s.mean) == 6 for s in cdm_traj.samples)


def test_trajectory_to_cdm_respects_an_explicit_state_space_id_even_with_attitude():
    tr = _traj_with_attitude()
    cdm_traj = cdm.trajectory_to_cdm(tr, entity_id="Sat", frame_id="EarthMJ2000Eq",
                                     state_space_id="custom.10")
    assert cdm_traj.state_space_id == "custom.10"
    assert all(len(s.mean) == 10 for s in cdm_traj.samples)


def test_trajectory_to_cdm_rejects_partial_attitude():
    tr = _traj()
    tr.attitude = [[0.0, 0.0, 0.0, 1.0]]  # only 1 of 3 samples
    with pytest.raises(cdm.CdmAdapterError):
        cdm.trajectory_to_cdm(tr, entity_id="Sat", frame_id="EarthMJ2000Eq")


def test_cdm_trajectory_to_viewer_json_round_trips_attitude():
    """Full round trip: altavista.Trajectory (with attitude) -> CDM -> altavista.Trajectory,
    scalar-last order preserved exactly, still parallel to t (web/js/scene.js's
    `s.attitude.length === s.t.length * 4` check)."""
    tr = _traj_with_attitude()
    cdm_traj = cdm.trajectory_to_cdm(tr, entity_id="Sat", frame_id="EarthMJ2000Eq")
    back = cdm.cdm_trajectory_to_viewer_json(cdm_traj)
    assert len(back.attitude) == len(back.t) == 3
    rows = sorted(zip(tr.t, tr.attitude), key=lambda row: row[0])
    for got, (_, want_q) in zip(back.attitude, rows):
        assert got == pytest.approx(want_q)


def test_cdm_trajectory_to_viewer_json_rejects_partial_attitude():
    tr = _traj_with_attitude()
    cdm_traj = cdm.trajectory_to_cdm(tr, entity_id="Sat", frame_id="EarthMJ2000Eq")
    del cdm_traj.samples[0].mean[6:]  # truncate one sample back to 6 components
    with pytest.raises(cdm.CdmAdapterError):
        cdm.cdm_trajectory_to_viewer_json(cdm_traj)


# --------------------------------------------------------------------------- declared state spaces (M7.1)
def test_state_space_for_declares_the_6_component_cartesian_shape():
    """docs/open-questions.md question 88's condition (a): DEFAULT_STATE_SPACE_ID must
    name a *declared* StateSpace (labels and units), not merely exist as a string."""
    space = cdm.state_space_for(cdm.DEFAULT_STATE_SPACE_ID)
    assert space.id == cdm.DEFAULT_STATE_SPACE_ID
    assert [c.label for c in space.components] == ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"]
    assert [c.unit for c in space.components] == [
        cdm.core_pb2.UNIT_METER, cdm.core_pb2.UNIT_METER, cdm.core_pb2.UNIT_METER,
        cdm.core_pb2.UNIT_METER_PER_SECOND, cdm.core_pb2.UNIT_METER_PER_SECOND, cdm.core_pb2.UNIT_METER_PER_SECOND,
    ]


def test_state_space_for_declares_the_10_component_attitude_shape():
    space = cdm.state_space_for(cdm.DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE)
    labels = [c.label for c in space.components]
    assert labels == ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z", "q_x", "q_y", "q_z", "q_w"]
    assert all(c.unit == cdm.core_pb2.UNIT_DIMENSIONLESS for c in space.components[6:])


def test_state_space_for_rejects_an_unknown_id_rather_than_guess():
    with pytest.raises(cdm.UnknownStateSpaceError):
        cdm.state_space_for("no.such.space")


# --------------------------------------------------------------------------- M20.1 (question 133):
# non-physical instances get their own, non-Cartesian state space, and the ingest path skips
# rendering position for one -- while still delivering its events (checked separately, at the
# Rust level, by crates/av-kernel/tests/demo_two_instance.rs's own port-command-count test).
def test_state_space_for_recognizes_gmat_orbital_cartesian6_as_the_same_cartesian_shape():
    """``gmat.orbital.cartesian6`` (every real GMAT-bound ``RunProducts.trajectories`` entry
    ``av-run`` produces names this id, not ``DEFAULT_STATE_SPACE_ID``) must resolve to the
    identical 6-component Cartesian position/velocity shape, matching
    ``crates/av-kernel/src/trajectory.rs``'s own registry. Fails against the pre-M20.1 code,
    which had never heard of this id at all (``cdm_trajectory_to_viewer_json`` never looked
    at ``state_space_id``, so this gap was silent) -- a regression here would make every real
    ingested GMAT trajectory an ``UnknownStateSpaceError`` the moment ``cdm_trajectory_to_
    viewer_json`` starts consulting the registry.
    """
    space = cdm.state_space_for(cdm.GMAT_ORBITAL_CARTESIAN6_ID)
    assert space.id == cdm.GMAT_ORBITAL_CARTESIAN6_ID
    assert [c.label for c in space.components] == ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"]
    assert cdm.has_position_class(space)


def test_state_space_for_declares_native_controller_scalar6_as_six_scalars():
    """``native.controller.scalar6`` (a non-physical native instance's own state space,
    e.g. ``demo_ctrl``) resolves to six independent, unitless scalars -- component-for-
    component the same declaration ``crates/av-kernel/src/trajectory.rs``'s own
    ``native_controller_scalar6`` builds. Fails against an implementation that reuses the
    Cartesian shape for this id, or declares a different label/unit convention than the
    Rust side.
    """
    space = cdm.state_space_for(cdm.NATIVE_CONTROLLER_SCALAR6_ID)
    assert space.id == cdm.NATIVE_CONTROLLER_SCALAR6_ID
    assert [c.label for c in space.components] == ["state_1", "state_2", "state_3", "state_4", "state_5", "state_6"]
    assert all(c.unit == cdm.core_pb2.UNIT_DIMENSIONLESS for c in space.components)


def test_has_position_class_reads_the_declared_state_space_not_a_hardcoded_id():
    """Question 133's own instruction: "the 'position class' test must read from the
    declared state space, not from a hard-coded id string." Proven directly: a hand-built
    ``StateSpace`` with an arbitrary id but the real Cartesian position/velocity prefix is
    recognized (no id-string special case could ever match ``"arbitrary.id"``), and one
    with the *canonical* id but non-Cartesian labels is correctly rejected. Fails against
    an implementation that special-cases known ids (`if state_space_id == "gmat.orbital
    .cartesian6"`) instead of inspecting `space.components` itself.
    """
    cartesian_but_odd_id = cdm.core_pb2.StateSpace(id="arbitrary.id", components=[
        cdm.core_pb2.StateComponent(label=label, unit=unit) for label, unit in zip(
            ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"],
            [cdm.core_pb2.UNIT_METER] * 3 + [cdm.core_pb2.UNIT_METER_PER_SECOND] * 3)
    ])
    assert cdm.has_position_class(cartesian_but_odd_id)

    canonical_id_but_scalar_labels = cdm.core_pb2.StateSpace(id=cdm.GMAT_ORBITAL_CARTESIAN6_ID, components=[
        cdm.core_pb2.StateComponent(label=f"state_{i}", unit=cdm.core_pb2.UNIT_DIMENSIONLESS) for i in range(1, 7)
    ])
    assert not cdm.has_position_class(canonical_id_but_scalar_labels)


def test_has_position_class_rejects_native_controller_scalar6():
    space = cdm.state_space_for(cdm.NATIVE_CONTROLLER_SCALAR6_ID)
    assert not cdm.has_position_class(space)


def _ctrl_traj(state_space_id: str) -> trajectory_pb2.Trajectory:
    traj = trajectory_pb2.Trajectory(id="demo_ctrl-trajectory", entity_id="demo_ctrl", state_space_id=state_space_id,
                                     interpolation=trajectory_pb2.INTERPOLATION_HERMITE_VELOCITY)
    for tai_ns, values in ((0, [0.0] * 6), (100, [0.0] * 6)):
        traj.samples.add(tai_ns=tai_ns, mean=values, kind=trajectory_pb2.SAMPLE_KIND_NATIVE)
    return traj


def test_cdm_trajectory_to_viewer_json_returns_none_for_a_non_physical_native_controller():
    """M20.1 (question 133, decided by the lead): "the producer emits no position
    trajectory for a state space without a position class. Not a zero-filled one -- none."
    Fails against the pre-M20.1 code, which ignored ``state_space_id`` entirely and would
    have misread these six non-positional scalars as km position/velocity (a zero-filled
    position track at the origin -- this task's own found defect, one level up, at the
    Rust producer); also fails against an implementation that raises instead of returning
    ``None``, or one that only checks a hardcoded id string.
    """
    traj = _ctrl_traj(cdm.NATIVE_CONTROLLER_SCALAR6_ID)
    assert cdm.cdm_trajectory_to_viewer_json(traj) is None


def test_cdm_trajectory_to_viewer_json_still_renders_a_real_gmat_bound_trajectory():
    """The backward-compatible half: a real GMAT-bound trajectory (``gmat.orbital
    .cartesian6``, a real position class) is unaffected by the M20.1 check -- proves this
    is a genuine, narrow addition, not a blanket new refusal."""
    traj = _ctrl_traj(cdm.GMAT_ORBITAL_CARTESIAN6_ID)
    viewer_traj = cdm.cdm_trajectory_to_viewer_json(traj)
    assert viewer_traj is not None
    assert len(viewer_traj.t) == 2


def test_cdm_trajectory_to_viewer_json_still_renders_an_unresolvable_state_space_id():
    """Deliberately conservative in the other direction (disclosed in the M20.1 report):
    an id neither this module nor the Rust side declares (a synthetic fixture's own
    placeholder, never a real ``av-run`` output) falls through to the unchanged, pre-M20.1
    behaviour -- treated as carrying position/velocity in its first six components --
    rather than a new hard refusal this task's own scope never asked for."""
    traj = _ctrl_traj("some.unregistered.id")
    viewer_traj = cdm.cdm_trajectory_to_viewer_json(traj)
    assert viewer_traj is not None
    assert len(viewer_traj.t) == 2


def test_scenario_to_cdm_emits_the_declared_state_spaces_the_trajectories_use():
    """The altavista CDM adapter emits a StateSpace message wherever a Trajectory is
    produced (question 88's condition (a), 'What to build' item 1): a scenario with one
    plain (6-component) spacecraft trajectory gets exactly one declared StateSpace back,
    matching every trajectory's own state_space_id."""
    bundle = cdm.scenario_to_cdm(_build_scenario_data())
    ids = {tr.state_space_id for tr in bundle.trajectories}
    assert ids == {cdm.DEFAULT_STATE_SPACE_ID}
    assert [s.id for s in bundle.state_spaces] == [cdm.DEFAULT_STATE_SPACE_ID]
    assert [c.label for c in bundle.state_spaces[0].components] == ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"]


def test_scenario_build_populates_state_spaces_from_a_real_gmat_run():
    """End-to-end (real GMAT): altavista.scenario.Scenario.build()'s additive
    ``state_spaces`` list (altavista/scenario.py's ``_build_state_spaces``) actually gets
    populated for a live-propagated spacecraft, and upgrades to the 10-component id once
    a NadirPointing attitude model is configured -- not just the pure-Python
    CdmBundle-level tests above."""
    from altavista.scenario import Scenario

    keplerian = dict(SMA=6878.0, ECC=0.0005, INC=51.6, RAAN=45.0, AOP=0.0, TA=0.0)

    sc_plain = Scenario("m71_state_space_plain", frame="EarthMJ2000Eq")
    sat_plain = sc_plain.spacecraft("m71_plain_sat", epoch="01 Jan 2026 00:00:00.000", keplerian=keplerian)
    sc_plain.propagate(sat_plain, seconds=300, step=60)
    data_plain = sc_plain.build()
    assert [s["id"] for s in data_plain.state_spaces] == [cdm.DEFAULT_STATE_SPACE_ID]
    assert data_plain.spacecraft[0].to_dict()["stateSpaceId"] == cdm.DEFAULT_STATE_SPACE_ID

    sc_att = Scenario("m71_state_space_attitude", frame="EarthMJ2000Eq")
    sat_att = sc_att.spacecraft("m71_att_sat", epoch="01 Jan 2026 00:00:00.000",
                               keplerian=keplerian, Attitude="NadirPointing")
    sc_att.propagate(sat_att, seconds=300, step=60)
    data_att = sc_att.build()
    assert [s["id"] for s in data_att.state_spaces] == [cdm.DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE]
    assert data_att.spacecraft[0].to_dict()["stateSpaceId"] == cdm.DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE
    # And the declared shape (10 components: labels + units) round-trips through the
    # protobuf-JSON transcoding scene.py uses -- not just the bare id string.
    comps = data_att.state_spaces[0]["components"]
    assert [c["label"] for c in comps] == ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z", "q_x", "q_y", "q_z", "q_w"]


def test_scenario_to_cdm_emits_both_state_spaces_when_only_some_trajectories_have_attitude():
    """A mixed scenario (one plain trajectory, one with attitude) gets both declared
    StateSpaces back, sorted by id -- never just the bundle-level default, since
    trajectory_to_cdm's own per-trajectory upgrade can make a trajectory's final
    state_space_id differ from what scenario_to_cdm was called with."""
    plain = _traj("Plain")
    with_attitude = _traj_with_attitude("WithAttitude")
    data = ScenarioData(name="mixed", frame=Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
                        spacecraft=[plain, with_attitude])
    bundle = cdm.scenario_to_cdm(data)
    ids = sorted(tr.state_space_id for tr in bundle.trajectories)
    assert ids == sorted([cdm.DEFAULT_STATE_SPACE_ID, cdm.DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE])
    assert [s.id for s in bundle.state_spaces] == sorted([cdm.DEFAULT_STATE_SPACE_ID, cdm.DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE])


# --------------------------------------------------------------------------- M9.2: DRM round trip (question 94)
def test_a_altavista_emitted_state_space_round_trips_through_the_rust_drm_loader(tmp_path):
    """M9.2 (`docs/open-questions.md` question 94, "What to build" item 4): "altavista's CDM
    adapter emits the same StateSpace message on the CDM path, so a DRM authored from a
    altavista scenario round-trips."

    This feeds the *actual* ``altavista.v1.StateSpace`` protobuf message
    ``cdm.state_space_for()`` returns -- not a hand-copied transcription of it -- through the
    real Rust YAML loader (``crates/av-kernel/src/drm/schema.rs``'s
    ``RawSystemDefinition``/``RawStateSpace``, added for M9.2), by writing it as a
    ``SystemDefinition`` YAML in the same field-for-field authoring format
    ``drms/*.system.yaml`` files use (see ``drms/README.md``), then running it through the
    same ``drm_hash`` example tool that same README documents DRM authors using to compute
    a canonical hash (``crates/av-kernel/examples/drm_hash.rs``, not owned by this worker but
    freely invocable as the shared tool it is). A parse failure here would mean altavista's own
    emitted shape (labels/units/order) disagrees with what the kernel's loader accepts --
    exactly the drift ``crates/av-kernel/src/trajectory.rs``'s module doc comment claims does
    not happen ("a `StateSpace` produced on either side of the Rust/Python boundary for the
    same id is the same declaration, not two independently-invented ones").

    This proves the *wire-shape* round trip (altavista's real output is accepted, byte for
    byte, by the real Rust parser this worker wrote). The *semantic* half -- that this exact
    declared shape then resolves through ``crate::trajectory::resolve_state_space`` and
    classifies per ADR-005 sec 3 -- is proven on the Rust side by
    ``crates/av-kernel/tests/state_space_declaration.rs``'s
    ``a_state_space_shaped_like_altavista_cdm_py_emits_it_round_trips_through_the_kernel``,
    which uses this identical id/label/unit shape (independently pinned here and there by
    each side's own ``state_space_for`` tests) -- split across languages because a single
    process can only execute one language's logic, tied together by that shared, tested
    convention. Together the two constitute the round trip question 94 asks for.

    Same cross-language-subprocess pattern as ``tests/test_dynamics_service_rs.py``. Requires
    a Rust toolchain (`cargo`); this environment has one (`cargo --version` succeeds), unlike
    the older note on ``test_a1_tai_matches_gmat_time_system_converter_post_1972`` above,
    which predates that -- skipped here too if `cargo` is ever unavailable, so this file still
    collects standalone.
    """
    import os
    import shutil
    import subprocess

    env = dict(os.environ)
    env["PATH"] = f"/opt/homebrew/opt/rustup/bin:{env.get('PATH', '')}"
    if shutil.which("cargo", path=env["PATH"]) is None:
        pytest.skip("cargo not available in this environment")

    space = cdm.state_space_for(cdm.DEFAULT_STATE_SPACE_ID)
    assert space.id == "altavista.cartesian_pos_vel_6"  # the id crates/av-kernel/src/
    # trajectory.rs's built-in registry declares under CARTESIAN_POS_VEL_6_ID -- the two
    # sides agree on the id string itself, not just the shape it names.

    lines = [
        "id: sys_from_altavista_round_trip",
        "dynamics_model: native.test",
        f"state_space_id: {space.id}",
        "state_space:",
        f"  id: {space.id}",
        "  components:",
    ]
    for c in space.components:
        lines.append(f"    - label: {c.label}")
        lines.append(f"      unit: {cdm.core_pb2.Unit.Name(c.unit)}")
    lines.append('hash: ""')
    yaml_text = "\n".join(lines) + "\n"

    system_yaml = tmp_path / "altavista_round_trip.system.yaml"
    system_yaml.write_text(yaml_text)

    repo_root = Path(__file__).resolve().parents[1]
    proc = subprocess.run(
        ["cargo", "run", "-p", "av-kernel", "--example", "drm_hash", "--", "system", str(system_yaml)],
        cwd=str(repo_root), env=env, capture_output=True, text=True, timeout=300)
    assert proc.returncode == 0, (
        f"the Rust DRM loader rejected an altavista-emitted StateSpace YAML:\n"
        f"--- yaml ---\n{yaml_text}\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    digest = proc.stdout.strip()
    assert len(digest) == 64 and all(ch in "0123456789abcdef" for ch in digest), f"expected a sha256 hex digest, got {digest!r}"


# --------------------------------------------------------------------------- config hash stability
def _build_scenario_data():
    tr1, tr2 = _traj("Sat"), _traj("Other")
    ev = Event(name="Reboost", t=21545.0, type="maneuver", spacecraft="Sat", detail="dv = 20.00 m/s (VNB)")
    return ScenarioData(name="demo", frame=Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
                        spacecraft=[tr2, tr1], events=[ev])  # deliberately unsorted


def test_config_hash_is_stable_across_independent_builds():
    b1 = cdm.scenario_to_cdm(_build_scenario_data())
    b2 = cdm.scenario_to_cdm(_build_scenario_data())
    assert b1.config_hash == b2.config_hash
    assert len(b1.config_hash) == 64  # sha256 hex digest
    for tr in b1.trajectories:
        assert tr.config_hash == b1.config_hash
        assert tr.provenance.config_hash == b1.config_hash
        assert tr.provenance.author_kind == cdm.core_pb2.AUTHOR_KIND_SERVICE
        assert tr.provenance.tool == cdm.TOOL_NAME


def test_config_hash_changes_when_the_frame_changes():
    a = _build_scenario_data()
    b = _build_scenario_data()
    b.frame = Frame("EarthICRF", "Earth", "ICRF")
    assert cdm.scenario_to_cdm(a).config_hash != cdm.scenario_to_cdm(b).config_hash


def test_scenario_to_cdm_output_order_is_independent_of_input_order():
    b1 = cdm.scenario_to_cdm(_build_scenario_data())
    names = [tr.entity_id for tr in b1.trajectories]
    assert names == sorted(names)


# --------------------------------------------------------------------------- endpoint
@pytest.fixture()
def client(tmp_path):
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)
    return TestClient(app)


def _sample_cdm_trajectory():
    return cdm.trajectory_to_cdm(_traj("Probe"), entity_id="Probe", frame_id="EarthMJ2000Eq",
                                 config_hash="cd" * 32)


def test_endpoint_accepts_binary_protobuf(client):
    cdm_traj = _sample_cdm_trajectory()
    resp = client.post("/api/cdm/trajectory", content=cdm_traj.SerializeToString(),
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["ok"] is True

    got = client.get(f"/api/scenario/{body['name']}")
    assert got.status_code == 200
    sc = got.json()
    assert sc["spacecraft"][0]["name"] == "Probe"
    assert len(sc["spacecraft"][0]["t"]) == 3


def test_endpoint_accepts_json_transcoding(client):
    cdm_traj = _sample_cdm_trajectory()
    payload = json_format.MessageToJson(cdm_traj)
    resp = client.post("/api/cdm/trajectory", content=payload,
                       headers={"content-type": "application/json"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    got = client.get(f"/api/scenario/{name}").json()
    assert got["spacecraft"][0]["name"] == "Probe"


def test_endpoint_rejects_malformed_binary_body(client):
    resp = client.post("/api/cdm/trajectory", content=b"\xff\xff\xff not a protobuf message \x00\x01",
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 400
    assert "malformed" in resp.json()["detail"].lower()


def test_endpoint_rejects_malformed_json_body(client):
    resp = client.post("/api/cdm/trajectory", content="{not valid json",
                       headers={"content-type": "application/json"})
    assert resp.status_code == 400
    assert "malformed" in resp.json()["detail"].lower()


def test_endpoint_rejects_non_hermite_interpolation(client):
    cdm_traj = _sample_cdm_trajectory()
    cdm_traj.interpolation = trajectory_pb2.INTERPOLATION_LINEAR
    resp = client.post("/api/cdm/trajectory", content=cdm_traj.SerializeToString(),
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 400
    assert "interpolation" in resp.json()["detail"].lower()


def test_post_scenario_still_works_unchanged(client):
    """POST /api/scenario (pre-existing endpoint) must keep working exactly as before."""
    scenario = ScenarioData(name="plain", frame=Frame(), spacecraft=[_traj("X")]).to_dict()
    resp = client.post("/api/scenario", json=scenario)
    assert resp.status_code == 200
    assert resp.json()["ok"] is True


# ---------------------------------------------------------------------------
# Shared epoch fixture: the Python half of the Rust<->Python cross-check.
# Added by the engineering manager during review of M1.3.
# ---------------------------------------------------------------------------

def test_python_reproduces_the_shared_epoch_fixture_exactly():
    """Pin the Python epoch conversions to the Rust reference implementation.

    ``data/time/epoch_fixture.json`` is generated from ``crates/av-cdm/src/time.rs``
    (``cargo run -p av-cdm --example gen_epoch_fixture``), which ADR-001 makes the reference
    implementation of the CDM's time rules. ``crates/av-cdm/tests/epoch_fixture.rs`` asserts
    the Rust side against the same file, so the two together pin two independent
    implementations to each other at **exact integer nanoseconds**.

    This is strictly stronger than comparing the two through GMAT: GMAT works in MJD floats,
    which lose microseconds at present-day epochs, and that slack is wide enough to hide a
    genuine disagreement between the two leap-second tables. What GMAT *does* prove -- that
    our table matches an independent authority -- is covered separately by
    ``tests/test_leap_seconds.py``.
    """
    import json
    from altavista import cdm as cdm_mod

    fixture_path = Path(__file__).resolve().parents[1] / "data" / "time" / "epoch_fixture.json"
    entries = json.loads(fixture_path.read_text())["entries"]
    assert len(entries) >= 12, "fixture lost entries"

    worst_a1 = 0.0
    for e in entries:
        label = e["label"]
        # Integer nanoseconds must agree exactly -- no tolerance.
        assert cdm_mod.utc_ns_to_tai_ns(e["utc_ns"]) == e["tai_ns"], f"TAI ns mismatch at {label}"
        assert cdm_mod.tai_ns_to_utc_ns(e["tai_ns"]) == e["utc_ns"], f"TAI->UTC mismatch at {label}"
        # A1 MJD is an f64 of days; ~1e-11 days is ~1 us, the float's own floor at these
        # magnitudes (ADR-001 rejected f64 epochs for exactly this reason).
        a1 = cdm_mod.tai_ns_to_a1mjd(e["tai_ns"])
        worst_a1 = max(worst_a1, abs(a1 - e["a1_mjd"]))
        assert abs(a1 - e["a1_mjd"]) < 1e-11, f"A1 MJD mismatch at {label}: {a1} vs {e['a1_mjd']}"
        # ...and the A1 -> TAI inverse returns the same integer nanosecond.
        resid = cdm_mod.a1mjd_to_tai_ns(e["a1_mjd"]) - e["tai_ns"]
        assert abs(resid) < 1000, f"A1 round trip lost {resid} ns at {label}, beyond the f64 floor"
    print(f"\nworst Python-vs-Rust A1MJD disagreement over the shared fixture: {worst_a1:.3e} days")


def test_shared_fixture_pins_the_leap_second_steps():
    """The 2 s TAI gap across a positive leap second, asserted on the shared fixture.

    One nominal UTC second plus the inserted leap second. A shift applied in the wrong
    direction, or omitted, cannot reproduce this.
    """
    import json
    fixture_path = Path(__file__).resolve().parents[1] / "data" / "time" / "epoch_fixture.json"
    entries = json.loads(fixture_path.read_text())["entries"]
    by = lambda needle: next(e for e in entries if needle in e["label"])["tai_ns"]
    assert by("2006-01-01") - by("2005-12-31") == 2_000_000_000
    assert by("2017-01-01") - by("2016-12-31") == 2_000_000_000
