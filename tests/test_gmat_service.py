"""Tests for services/gmat-service (M2.2): altavista.v1.DynamicsService hosted over
altavista.

The server runs as a **separate process** (``python -m gmat_service``) for the whole
module, started once and shared by every test here: GMAT is a process-wide singleton
(``altavista/gmat_env.py``'s module docstring, ``docs/adr/002-dynamics-contract.md``), so
the server needs its own process, and this test file's own comparison tests (which load
GMAT directly through ``altavista.scenario``) need theirs -- exactly the split the task
brief calls out ("the test process itself may also load GMAT").

Readiness is awaited with ``grpc.channel_ready_future(...).result(timeout=...)`` -- a
real poll/block, not a bare ``sleep`` guess -- and the subprocess is always terminated in
the fixture's ``finally`` block, including when readiness itself fails.
"""
from __future__ import annotations

import json
import socket
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import grpc
import pytest

from altavista import cdm as cdm_adapter
from altavista.pb import core_pb2, dynamics_service_pb2, dynamics_service_pb2_grpc, trajectory_pb2

REPO_ROOT = Path(__file__).resolve().parents[1]
SERVICE_DIR = REPO_ROOT / "services" / "gmat-service"
GOLDEN_PATH = REPO_ROOT / "goldens" / "leo_1day_jgm2_8x8_sunmoon.json"
GOLDEN = json.loads(GOLDEN_PATH.read_text())

# gmat_service is a plain package (not pip-installed -- see services/gmat-service/README.md),
# so it needs its own directory on sys.path. Only gmat_service.covariance is imported below
# (a pure-numpy leaf module, no altavista/gmatpy dependency at import time -- gmat_service's own
# __init__.py is a docstring only), so this stays cheap and needs no GMAT/subprocess machinery,
# unlike every other test in this file.
if str(SERVICE_DIR) not in sys.path:
    sys.path.insert(0, str(SERVICE_DIR))
from gmat_service import covariance as cov_check  # noqa: E402

READY_TIMEOUT_S = 90.0
MODEL_ID = "gmat.earth.jgm2_8x8.sun_moon"


def _free_port() -> int:
    """An ephemeral localhost port, free at the moment of the check (the usual
    bind-then-close trick; a small unavoidable race, but adequate for a test fixture)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    port = _free_port()
    evidence_path = tmp_path_factory.mktemp("gmat_service") / "evidence.jsonl"
    run_id = "test_gmat_service"
    proc = subprocess.Popen(
        [sys.executable, "-m", "gmat_service", "--port", str(port),
         "--evidence-path", str(evidence_path), "--run-id", run_id],
        cwd=str(SERVICE_DIR), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    channel = grpc.insecure_channel(f"127.0.0.1:{port}")
    try:
        try:
            grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
        except Exception as e:
            channel.close()
            returncode = proc.poll()
            output = ""
            try:
                if proc.stdout is not None:
                    output = proc.stdout.read()
            except Exception:
                pass
            pytest.fail(
                f"gmat-service subprocess did not become ready within {READY_TIMEOUT_S}s "
                f"(returncode={returncode}): {e}\n--- subprocess output ---\n{output}")
        yield SimpleNamespace(channel=channel, port=port, evidence_path=evidence_path, run_id=run_id, proc=proc)
        channel.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@pytest.fixture(scope="module")
def stub(server):
    return dynamics_service_pb2_grpc.DynamicsServiceStub(server.channel)


# --------------------------------------------------------------------------- Describe
def test_describe_returns_expected_model_info(stub):
    info = stub.Describe(dynamics_service_pb2.DescribeRequest())
    assert info.id == MODEL_ID
    assert info.depth == "gmat-api"
    assert info.frame_id == "EarthMJ2000Eq"
    assert info.state_space_id == "altavista.cartesian_pos_vel_6"
    assert list(info.goldens) == ["leo_1day_jgm2_8x8_sunmoon"]
    assert len(info.settings_hash) == 64  # SHA-256 hex

    caps = set(info.capabilities)
    for expected in (dynamics_service_pb2.MODEL_CAPABILITY_STEP,
                     dynamics_service_pb2.MODEL_CAPABILITY_PROPAGATE,
                     dynamics_service_pb2.MODEL_CAPABILITY_DERIVATIVES,
                     dynamics_service_pb2.MODEL_CAPABILITY_DETERMINISTIC,
                     # M3.2: Propagate(covariance=true) now propagates a declared P0 through
                     # GMAT's own STM (PropagationStateManager.SetProperty("STM", sc)).
                     dynamics_service_pb2.MODEL_CAPABILITY_STM):
        assert expected in caps
    # Solve is still UNIMPLEMENTED.
    assert dynamics_service_pb2.MODEL_CAPABILITY_SOLVE not in caps


def test_describe_rejects_unknown_model_id(stub):
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Describe(dynamics_service_pb2.DescribeRequest(model_id="not.a.real.model"))
    assert exc_info.value.code() == grpc.StatusCode.NOT_FOUND


# --------------------------------------------------------------------------- Step
def test_step_matches_direct_altavista_propagate(stub):
    """One Step RPC call vs. a direct altavista.scenario.Scenario.propagate() of the same
    duration from the same initial state, same force model and integrator settings.

    dt_s is chosen well under this model's MaxStep (600 s) so the *reference* path
    (Scenario.propagate(step=dt_s)) is itself correct -- see
    services/gmat-service/gmat_service/model.py's module docstring for why a dt/step
    larger than MaxStep is not a safe way to drive GMAT's Propagator.Step().
    """
    from altavista.scenario import Scenario, Spacecraft

    epoch_a1mjd = GOLDEN["epoch_a1mjd"]
    state_km = list(GOLDEN["initial_state"])
    dt_s = 300.0

    sc = Scenario(name="test_gmat_service_step_ref", frame="EarthMJ2000Eq")
    fm = sc.force_model(degree=8, order=8, point_masses=("Luna", "Sun"))
    prop = sc.propagator(force_model=fm, integrator="PrinceDormand78", max_step=600.0,
                         min_step=0.0, initial_step=60.0, accuracy=1e-13)
    obj = sc.gmat.Construct("Spacecraft", "test_gmat_service_step_ref_sat")
    ref_sat = Spacecraft(sc, obj)
    ref_sat.epoch = epoch_a1mjd
    ref_sat.state = list(state_km)
    ref_sat.central_body = "Earth"
    sc.propagate([ref_sat], seconds=dt_s, step=dt_s, propagator=prop)
    expected_si = [x * 1000.0 for x in ref_sat.state]

    tai_ns = cdm_adapter.a1mjd_to_tai_ns(epoch_a1mjd)
    req = dynamics_service_pb2.StepRequest(
        state=dynamics_service_pb2.StateVector(state=[x * 1000.0 for x in state_km], tai_ns=tai_ns),
        dt_s=dt_s)
    resp = stub.Step(req)
    got_si = list(resp.state.state)

    pos_err = max(abs(a - b) for a, b in zip(got_si[0:3], expected_si[0:3]))
    vel_err = max(abs(a - b) for a, b in zip(got_si[3:6], expected_si[3:6]))
    assert pos_err < 1e-3, f"Step position disagrees with direct altavista propagate by {pos_err} m"
    assert vel_err < 1e-6, f"Step velocity disagrees with direct altavista propagate by {vel_err} m/s"


def test_step_rejects_covariance_request(stub):
    tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    state_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    req = dynamics_service_pb2.StepRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns),
        dt_s=60.0, cov=[1.0] * 36)
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Step(req)
    err = exc_info.value
    assert err.code() == grpc.StatusCode.FAILED_PRECONDITION
    assert "STM" in err.details()


# --------------------------------------------------------------------------- Propagate
@pytest.mark.slow
def test_propagate_matches_golden_within_tolerance(stub):
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    horizon_tai_ns = epoch_tai_ns + int(round(GOLDEN["duration_s"] * 1e9))

    req = dynamics_service_pb2.PropagateRequest(
        model_id=MODEL_ID,
        seed=core_pb2.GaussianState(mean=seed_mean_si, epoch_ns=epoch_tai_ns),
        horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0, entity_id="golden_arc")
    resp = stub.Propagate(req)
    traj = resp.trajectory

    assert traj.interpolation == trajectory_pb2.INTERPOLATION_HERMITE_VELOCITY
    assert traj.entity_id == "golden_arc"
    assert traj.provenance.tool == "gmat-service"
    assert traj.provenance.run_id  # non-empty
    assert len(traj.segments) == 1
    assert traj.segments[0].dynamics_model == MODEL_ID
    assert traj.segments[0].dynamics_depth == "gmat-api"
    assert len(traj.samples) > 100  # 86400s / 600s + 1 = 145 samples expected

    last = traj.samples[-1]
    expected_si = [x * 1000.0 for x in GOLDEN["final_state"]]
    pos_err = max(abs(a - b) for a, b in zip(list(last.mean)[0:3], expected_si[0:3]))
    vel_err = max(abs(a - b) for a, b in zip(list(last.mean)[3:6], expected_si[3:6]))
    print(f"\nPropagate vs golden {GOLDEN_PATH.name}: "
         f"pos_err={pos_err:.6e} m (tolerance_m={GOLDEN['tolerance_m']}), "
         f"vel_err={vel_err:.6e} m/s (tolerance_mps={GOLDEN['tolerance_mps']})")
    assert pos_err <= GOLDEN["tolerance_m"], (
        f"position error {pos_err} m exceeds golden tolerance {GOLDEN['tolerance_m']} m")
    assert vel_err <= GOLDEN["tolerance_mps"], (
        f"velocity error {vel_err} m/s exceeds golden tolerance {GOLDEN['tolerance_mps']} m/s")


@pytest.mark.slow
def test_propagate_covariance_true_matches_golden_stm(stub):
    """M3.2: covariance=true now returns a real, GMAT-STM-propagated covariance (previously
    FAILED_PRECONDITION) -- pinned against goldens/leo_1day_jgm2_8x8_sunmoon.json's "stm"
    block (GMAT's own PropagationStateManager("STM") propagation of the same arc, the
    reference this service's own STM-requesting Propagate call reproduces bit-for-bit, since
    depth 1's own mechanism *is* GMAT's propagator -- see model.GmatModel.propagate_covariance's
    docstring)."""
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    horizon_tai_ns = epoch_tai_ns + int(round(GOLDEN["duration_s"] * 1e9))
    p0_si = GOLDEN["stm"]["p0_si"]

    req = dynamics_service_pb2.PropagateRequest(
        model_id=MODEL_ID,
        seed=core_pb2.GaussianState(mean=seed_mean_si, cov=p0_si, epoch_ns=epoch_tai_ns),
        horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0, entity_id="golden_arc_cov",
        covariance=True)
    resp = stub.Propagate(req)
    traj = resp.trajectory

    assert len(traj.samples) > 100  # same cadence as test_propagate_matches_golden_within_tolerance
    first = traj.samples[0]
    assert len(first.cov) == 36
    # Phi(t0,t0) = I (exact identity, ADR-002 second amendment) -> P(t0) = P0 exactly.
    for a, b in zip(first.cov, p0_si):
        assert a == pytest.approx(b, rel=1e-9, abs=1e-9)

    last = traj.samples[-1]
    assert len(last.cov) == 36
    expected_cov = GOLDEN["stm"]["cov_t1_si"]
    cov_abs_err = max(abs(a - b) for a, b in zip(last.cov, expected_cov))
    cov_norm = max(abs(v) for v in expected_cov)
    cov_rel_err = cov_abs_err / cov_norm
    print(f"\nPropagate covariance vs golden {GOLDEN_PATH.name}: "
         f"max abs err {cov_abs_err:.6e}, rel err {cov_rel_err:.3e}")
    assert cov_rel_err < 1e-6, f"covariance relative error {cov_rel_err} exceeds 1e-6 of the golden's own covariance norm"

    # The mean (first 6 components) is unaffected by requesting covariance -- same tolerance
    # as the plain (non-covariance) Propagate test.
    expected_si = [x * 1000.0 for x in GOLDEN["final_state"]]
    pos_err = max(abs(a - b) for a, b in zip(list(last.mean)[0:3], expected_si[0:3]))
    vel_err = max(abs(a - b) for a, b in zip(list(last.mean)[3:6], expected_si[3:6]))
    assert pos_err <= GOLDEN["tolerance_m"]
    assert vel_err <= GOLDEN["tolerance_mps"]


def test_propagate_covariance_true_without_seed_cov_is_invalid_argument(stub):
    """Question 11: covariance is always explicitly requested, never a silent default -- a
    covariance=true request with no declared P0 (an empty seed.cov) must fail loudly, not
    fall back to an invented identity or zero covariance."""
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    horizon_tai_ns = epoch_tai_ns + int(3600 * 1e9)
    req = dynamics_service_pb2.PropagateRequest(
        seed=core_pb2.GaussianState(mean=seed_mean_si, epoch_ns=epoch_tai_ns),  # no cov
        horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0, covariance=True)
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Propagate(req)
    err = exc_info.value
    assert err.code() == grpc.StatusCode.INVALID_ARGUMENT
    assert "seed.cov" in err.details()


# --------------------------------------------------------------------------- Derivatives
def test_derivatives_matches_finite_difference_of_step(stub):
    """Cheap self-consistency check: state_dot's velocity components must equal the
    input state's velocity exactly (kinematic identity, any force model), and the
    acceleration components must roughly match the secant slope Step() itself produces
    over a short interval (loose tolerance: this is a first-order finite-difference
    check of a smooth, slowly varying acceleration field, not a golden-tolerance
    physics check)."""
    epoch_a1mjd = GOLDEN["epoch_a1mjd"]
    tai_ns = cdm_adapter.a1mjd_to_tai_ns(epoch_a1mjd)
    state_si = [x * 1000.0 for x in GOLDEN["initial_state"]]

    deriv_resp = stub.Derivatives(dynamics_service_pb2.DerivativesRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns)))
    state_dot = list(deriv_resp.state_dot)
    assert len(state_dot) == 6
    assert state_dot[0:3] == pytest.approx(state_si[3:6], rel=1e-12)

    # dt_s must be short enough that the secant (average acceleration over the interval)
    # is close to the tangent (instantaneous acceleration at t=0), but for a LEO orbit the
    # acceleration vector itself is rotating at the orbital angular rate (~1.1e-3 rad/s
    # here), so the secant/tangent gap shrinks only linearly with dt_s, not quadratically
    # -- measured empirically at dt_s=10s: ~0.037 m/s^2, at dt_s=1s: ~0.0037 m/s^2. 0.01
    # m/s^2 at dt_s=1s leaves a comfortable margin while still catching a gross bug (wrong
    # units, wrong sign, wrong force model).
    dt_s = 1.0
    step_resp = stub.Step(dynamics_service_pb2.StepRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns), dt_s=dt_s))
    secant_accel = [(b - a) / dt_s for a, b in zip(state_si[3:6], list(step_resp.state.state)[3:6])]
    accel_err = max(abs(a - b) for a, b in zip(state_dot[3:6], secant_accel))
    assert accel_err < 1e-2, f"Derivatives acceleration disagrees with Step's secant slope by {accel_err} m/s^2"


def test_derivatives_rejects_controls(stub):
    tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    state_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    req = dynamics_service_pb2.DerivativesRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns), controls=[1.0])
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Derivatives(req)
    assert exc_info.value.code() == grpc.StatusCode.INVALID_ARGUMENT


# --------------------------------------------------------------------------- Solve
def test_solve_is_unimplemented(stub):
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Solve(dynamics_service_pb2.SolveRequest())
    assert exc_info.value.code() == grpc.StatusCode.UNIMPLEMENTED


# --------------------------------------------------------------------------- Covariance hygiene
#
# gmat_service.covariance (docs/open-questions.md question 80): pure unit tests of the module
# itself, no server/subprocess/GMAT required -- these mirror crates/av-cdm/src/covariance.rs's
# own required-tests list exactly, at the same fixtures where that makes equivalence-checking
# meaningful.

def test_golden_p0_and_propagated_cov_t1_both_pass_the_check():
    """Required test: a propagated P from the golden passes the check. Uses the same golden
    (and the same two arrays: the declared P0 and GMAT's own propagated P(t1)) the Rust side's
    crates/av-cdm/src/covariance.rs::tests::the_golden_arcs_own_p0_and_propagated_cov_t1_both_
    pass test uses, so the two language implementations are checked against the identical
    numbers, not merely "some golden or other"."""
    p0 = GOLDEN["stm"]["p0_si"]
    cov_t1 = GOLDEN["stm"]["cov_t1_si"]
    assert len(p0) == 36 and len(cov_t1) == 36

    p0_diag = cov_check.check_spd(p0, 6, "golden.p0_si")
    cov_t1_diag = cov_check.check_spd(cov_t1, 6, "golden.cov_t1_si")
    print(f"\n[gmat-service covariance] golden {GOLDEN_PATH.name}: "
          f"P0 min Cholesky diag^2 proxy = {p0_diag.min_cholesky_diag_sq:.6e}, "
          f"P(t1) min Cholesky diag^2 proxy = {cov_t1_diag.min_cholesky_diag_sq:.6e} "
          f"(golden's own recorded pre-symmetrization asymmetry at t1: "
          f"{GOLDEN['stm']['cov_t1_pre_symmetrization_asymmetry']})")


def test_hand_built_asymmetric_matrix_fails_with_the_typed_error_and_is_counted():
    """Required test: a hand-built asymmetric matrix fails with the typed error. Same fixture
    as the Rust side's equivalence_asymmetric_matrix_is_rejected_by_both."""
    cov = [1.0, 0.3, 0.2, 1.0]
    before = cov_check.spd_check_failures()
    with pytest.raises(cov_check.NotSymmetricError) as exc_info:
        cov_check.check_spd(cov, 2, "test")
    assert exc_info.value.i == 0 and exc_info.value.j == 1
    # `>`, not `== before + 1`: this module-level counter is process-wide within the pytest
    # process, and pytest-xdist/parallel test ordering aside, other tests in this file also
    # call check_spd and can race with this one under any future parallel test runner --
    # monotonic increase by at least our own call is the safe claim, matching the same
    # reasoning crates/av-cdm/src/covariance.rs's own counter tests use.
    assert cov_check.spd_check_failures() > before, "a failed check must be counted"


def test_hand_built_indefinite_matrix_fails_with_the_typed_error_and_is_counted():
    """Required test: a hand-built indefinite matrix fails with the typed error. Same fixture
    as the Rust side's equivalence_indefinite_matrix_is_rejected_by_both (eigenvalues -1, 3)."""
    cov = [1.0, 2.0, 2.0, 1.0]
    before = cov_check.spd_check_failures()
    with pytest.raises(cov_check.NotPositiveDefiniteError):
        cov_check.check_spd(cov, 2, "test")
    assert cov_check.spd_check_failures() > before, "a failed check must be counted"


def test_non_finite_and_dimension_mismatch_are_also_typed_errors():
    with pytest.raises(cov_check.NotFiniteError) as exc_info:
        cov_check.check_spd([float("nan"), 0.0, 0.0, 1.0], 2, "test")
    assert exc_info.value.index == 0

    with pytest.raises(cov_check.DimensionMismatchError) as exc_info:
        cov_check.check_spd([1.0, 0.0, 0.0], 2, "test")
    assert exc_info.value.expected == 4 and exc_info.value.actual == 3


def test_well_conditioned_matrix_passes():
    cov = [4.0, 0.0, 0.0, 0.0, 9.0, 0.0, 0.0, 0.0, 1.0]
    diag = cov_check.check_spd(cov, 3, "test")
    assert diag.n == 3
    assert diag.min_cholesky_diag_sq == pytest.approx(1.0, abs=1e-9)


def test_symmetry_tolerance_is_relative_to_magnitude():
    """Same fixture as the Rust side's equivalence_symmetry_tolerance_is_relative_to_magnitude_
    on_both_sides: large-magnitude round-off asymmetry passes, small-magnitude genuine
    asymmetry fails."""
    c = 1e9
    eps = c * 1e-12
    healthy = [c, c / 2.0, c / 2.0 + eps, c]
    cov_check.check_spd(healthy, 2, "test")  # must not raise

    tiny_but_real = [1e-6, 1e-7, 2e-7, 1e-6]
    with pytest.raises(cov_check.NotSymmetricError):
        cov_check.check_spd(tiny_but_real, 2, "test")


def test_nearest_spd_of_an_indefinite_matrix_passes_the_check_afterward():
    cov = [1.0, 2.0, 2.0, 1.0]  # eigenvalues -1, 3
    with pytest.raises(cov_check.CovarianceHygieneError):
        cov_check.check_spd(cov, 2, "test")

    before = cov_check.nearest_spd_projections_applied()
    projected = cov_check.nearest_spd(cov, 2)
    assert cov_check.nearest_spd_projections_applied() > before, "an application must be counted"

    diag = cov_check.check_spd(projected, 2, "test")  # must not raise
    assert diag.n == 2
    # The negative eigenvalue (-1) is floored near zero; the positive one (3) survives close
    # to unchanged -- checked via the trace, dominated by the surviving eigenvalue.
    trace = projected[0] + projected[3]
    assert trace == pytest.approx(3.0, abs=1e-3)


def test_nearest_spd_leaves_an_already_spd_matrix_close_to_unchanged():
    flat = [4.0, 1.0, 0.0, 1.0, 9.0, 2.0, 0.0, 2.0, 1.0]
    cov_check.check_spd(flat, 3, "test")  # fixture must already be SPD; must not raise
    projected = cov_check.nearest_spd(flat, 3)
    for got, want in zip(projected, flat):
        assert got == pytest.approx(want, abs=1e-6)


# --------------------------------------------------------------------------- M5.1 wiring:
# nearest_spd_projection (question 83) and accept_missing_stm_terms (question 82), driven
# directly through gmat_service.model.GmatModel in this test process (not through the wire --
# PropagateRequest has no DrmOptions field to carry either flag; see
# GmatModel.propagate_covariance's own docstring for why). This test process already loads GMAT
# directly for other comparison tests in this file (module docstring: "the test process itself
# may also load GMAT").
#
# GMAT is a process-wide singleton (module docstring), and GmatModel.warm_up() builds its
# force-model/propagator objects under fixed names -- so, unlike the *server* subprocess (which
# only ever builds exactly one GmatModel), this test process must reuse a single GmatModel across
# every test below, not build a fresh one per test: a second independent GmatModel().warm_up()
# call in the same process re-`Construct`s the same fixed object names GMAT's own Moderator
# already has configured from the first call and fails with "already a GravityField force in
# place for that body". `direct_model` is therefore a module-scoped fixture, not a per-test
# helper.
#
# A short (one output interval) horizon keeps these fast -- unlike the full-day golden
# comparisons above, nothing here needs to match a golden's numbers, only exercise the
# hygiene/capability wiring itself.

@pytest.fixture(scope="module")
def direct_model():
    """The one gmat_service.model.GmatModel this test process builds, warmed up once and
    shared by every test in this section (see the comment above for why one, not one per
    test)."""
    from gmat_service.model import GmatModel
    model = GmatModel(run_id="test_gmat_service_direct")
    model.warm_up()
    return model


def test_nearest_spd_projection_end_to_end_through_propagate_covariance(direct_model):
    """Question 83: a covariance whose propagated P(t) is indefinite is a typed
    FAILED_PRECONDITION by default, and is repaired (counted) when nearest_spd_projection=True.

    A real invertible congruence (`P(t) = Phi P0 Phi^T`, Phi from an actual STM-requesting GMAT
    propagation) preserves the *signature* of P0 (Sylvester's law of inertia): seeding a
    negative-definite P0 (eigenvalues all -100) guarantees P(t) is negative-definite too,
    regardless of what Phi turned out to be -- a deterministic way to reach the hygiene-check
    failure without needing to know Phi in advance.
    """
    from gmat_service.model import ModelError

    model = direct_model
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    horizon_tai_ns = epoch_tai_ns + int(600 * 1e9)  # one interval, not the full day
    bad_p0 = [0.0] * 36
    for i in range(6):
        bad_p0[i * 6 + i] = -100.0  # negative-definite

    with pytest.raises(ModelError) as exc_info:
        model.propagate_covariance(
            seed_mean_si=seed_mean_si, seed_epoch_tai_ns=epoch_tai_ns,
            horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0,
            seed_cov_si=bad_p0, entity_id="nearest_spd_off")
    assert exc_info.value.code == "FAILED_PRECONDITION"
    assert "SPD hygiene" in str(exc_info.value)

    before = cov_check.nearest_spd_projections_applied()
    traj, covariances = model.propagate_covariance(
        seed_mean_si=seed_mean_si, seed_epoch_tai_ns=epoch_tai_ns,
        horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0,
        seed_cov_si=bad_p0, entity_id="nearest_spd_on", nearest_spd_projection=True)
    assert cov_check.nearest_spd_projections_applied() > before, "the projection must be counted"
    assert len(covariances) == len(traj.t)
    for cov in covariances:
        cov_check.check_spd(cov, 6, "test")  # must not raise: the repaired covariance passes


def test_relativistic_correction_withholds_covariance_unless_accepted(direct_model, monkeypatch):
    """Question 82: propagate_covariance refuses with a typed, named FAILED_PRECONDITION when
    gmat_service.config.HAS_RELATIVISTIC_CORRECTION is set, unless accept_missing_stm_terms is
    also set -- exercised by monkeypatching the config flag this service's fixed force model
    never sets on its own (see config.HAS_RELATIVISTIC_CORRECTION's own comment for why an
    actual RelativisticCorrection force cannot be built through altavista.scenario today), the
    same way the check itself reads it, so this is a real exercise of model.py's own code path,
    not a re-implementation of the check being tested against itself."""
    from gmat_service import config as svc_config
    from gmat_service.model import ModelError

    model = direct_model
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    horizon_tai_ns = epoch_tai_ns + int(600 * 1e9)
    p0_si = GOLDEN["stm"]["p0_si"]

    monkeypatch.setattr(svc_config, "HAS_RELATIVISTIC_CORRECTION", True)

    with pytest.raises(ModelError) as exc_info:
        model.propagate_covariance(
            seed_mean_si=seed_mean_si, seed_epoch_tai_ns=epoch_tai_ns,
            horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0,
            seed_cov_si=p0_si, entity_id="relcorr_refused")
    assert exc_info.value.code == "FAILED_PRECONDITION"
    assert "RelativisticCorrection" in str(exc_info.value)
    assert "accept_missing_stm_terms" in str(exc_info.value)

    # Same request, accept_missing_stm_terms=True -> proceeds (no longer refused for this
    # reason; the underlying force model never actually contains RelativisticCorrection, so
    # this just exercises that the override lifts the refusal, not a golden comparison).
    traj, covariances = model.propagate_covariance(
        seed_mean_si=seed_mean_si, seed_epoch_tai_ns=epoch_tai_ns,
        horizon_tai_ns=horizon_tai_ns, sample_interval_s=600.0,
        seed_cov_si=p0_si, entity_id="relcorr_accepted", accept_missing_stm_terms=True)
    assert len(covariances) == len(traj.t)


def test_capabilities_excludes_stm_when_relativistic_correction_is_set(monkeypatch):
    """Question 82 at Describe()'s own layer: gmat_service.service._capabilities() must not
    list MODEL_CAPABILITY_STM once config.HAS_RELATIVISTIC_CORRECTION is set, and must list it
    otherwise (today's real, always-False state) -- a pure function call, no server/subprocess
    needed."""
    from gmat_service import config as svc_config
    from gmat_service import service as svc_module

    assert dynamics_service_pb2.MODEL_CAPABILITY_STM in svc_module._capabilities()

    monkeypatch.setattr(svc_config, "HAS_RELATIVISTIC_CORRECTION", True)
    assert dynamics_service_pb2.MODEL_CAPABILITY_STM not in svc_module._capabilities()


# --------------------------------------------------------------------------- Evidence log
def test_evidence_log_gets_a_line_per_response(server, stub):
    before = server.evidence_path.read_text().splitlines() if server.evidence_path.exists() else []
    stub.Describe(dynamics_service_pb2.DescribeRequest())
    after = server.evidence_path.read_text().splitlines()
    assert len(after) == len(before) + 1

    entry = json.loads(after[-1])
    for key in ("epoch", "method", "request_hash", "response_hash", "settings_hash", "run_id"):
        assert key in entry, f"evidence line missing {key!r}: {entry}"
    assert entry["method"] == "Describe"
    assert entry["run_id"] == server.run_id
    assert isinstance(entry["epoch"], int) and entry["epoch"] > 0
    for hash_key in ("request_hash", "response_hash", "settings_hash"):
        h = entry[hash_key]
        assert len(h) == 64 and all(c in "0123456789abcdef" for c in h), f"{hash_key} not a SHA-256 hex digest: {h!r}"

    # Every earlier successful (non-error) RPC in this module also logged a line -- errors
    # are aborted before a response exists to log, so only the successful calls count:
    # Describe x2 (the model-info test + this one), Step x2 (the comparison test + the
    # Derivatives self-consistency test's secant), Propagate x2 (the plain golden test + the
    # M3.2 covariance test), Derivatives x1 = 7. `>=` rather than `==` since test order is
    # not guaranteed and this need only be a lower bound.
    assert len(after) >= 7
