"""M6.2: tests for `crates/av-dynamics-service` -- `altavista.v1.DynamicsService` hosted
in Rust over `gmat-sys`/`av-dynamics` (ADR-002 depth 2, `"gmat-ffi"`), as decided by
`docs/adr/003-substrate-and-deployment.md`'s 2026-09-02 amendment ("`DynamicsService` is
hosted in Rust... so `grpcio` leaves the deployed runtime entirely").

Mirrors `tests/test_gmat_service.py`'s structure (same golden, same subprocess-with-
readiness-poll pattern, same RPC-by-RPC coverage) but drives the **Rust** server binary
instead of `python -m gmat_service`, over the **same wire protocol** (this file reuses
`altavista.pb`'s generated Python stubs -- the proto is the single source of truth for both
languages, ADR-001).

**The golden `Propagate` comparison (`test_propagate_matches_golden_within_tolerance`) is
the load-bearing test in this file**: it asserts the Rust server's `Propagate` lands within
`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own declared tolerance, unmodified from what
`tests/test_gmat_service.py` asserts for the Python service against the identical golden --
see that test's docstring for why the tolerance is never loosened to make a service pass.

Two things are **deliberately different** from `test_gmat_service.py`'s expectations, not
bugs -- see `crates/av-dynamics-service/src/config.py`'s module doc and this crate's
README:
  - `ModelInfo.depth` / `TrajectorySegment.dynamics_depth` are `"gmat-ffi"`, not
    `"gmat-api"` (ADR-002 depth 2, not depth 1).
  - `ModelInfo.settings_hash` differs from the Python service's own hash (this server
    integrates with `av_dynamics::integrate::Dopri5`, not GMAT's native `PrinceDormand78`
    propagator, so "what determines the physics" is genuinely a different settings
    fingerprint).

Builds `av-dynamics-service` via `cargo build` (module-scoped fixture, like
`tests/test_grpc_tls.py`'s `describe_client_bin`) and starts it as a **separate process**
on an ephemeral loopback port, plaintext (no TLS in this file -- `tests/test_grpc_tls.py`
is where mTLS-through-nginx is proven, for both the Python and this Rust server).
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from types import SimpleNamespace

import grpc
import pytest

from altavista import cdm as cdm_adapter
from altavista.pb import core_pb2, dynamics_service_pb2, dynamics_service_pb2_grpc, trajectory_pb2

REPO_ROOT = Path(__file__).resolve().parents[1]
GOLDEN_PATH = REPO_ROOT / "goldens" / "leo_1day_jgm2_8x8_sunmoon.json"
GOLDEN = json.loads(GOLDEN_PATH.read_text())

READY_TIMEOUT_S = 90.0
MODEL_ID = "gmat.earth.jgm2_8x8.sun_moon"
DEPTH = "gmat-ffi"  # ADR-002 depth 2 -- see this module's docstring.

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
OPENSSL_DIR = "/opt/homebrew/opt/openssl@3"


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    env["OPENSSL_DIR"] = OPENSSL_DIR
    return env


def _free_port() -> int:
    """Same bind-then-close trick as `tests/test_gmat_service.py`'s `_free_port`."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture(scope="module")
def server_bin():
    """Builds `crates/av-dynamics-service`'s `av-dynamics-service` binary once for the
    module. A build failure here is a real failure of this task, not something to skip
    over -- same posture as `tests/test_grpc_tls.py`'s `describe_client_bin` fixture."""
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-dynamics-service", "--bin", "av-dynamics-service"],
        cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-dynamics-service failed:\n--- stdout ---\n{proc.stdout}\n"
                     f"--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "av-dynamics-service"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def server(server_bin, tmp_path_factory):
    port = _free_port()
    admin_port = _free_port()
    evidence_path = tmp_path_factory.mktemp("av_dynamics_service") / "evidence.jsonl"
    run_id = "test_dynamics_service_rs"
    proc = subprocess.Popen(
        [str(server_bin), "--port", str(port), "--admin-port", str(admin_port),
         "--evidence-path", str(evidence_path), "--run-id", run_id],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    # Plaintext on loopback only (ADR-004/ADR-003 amendment) -- same as gmat-service; the
    # cross-host/mTLS story is tests/test_grpc_tls.py's, not this file's.
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
                f"av-dynamics-service subprocess did not become ready within {READY_TIMEOUT_S}s "
                f"(returncode={returncode}): {e}\n--- subprocess output ---\n{output}")
        yield SimpleNamespace(channel=channel, port=port, admin_port=admin_port,
                              evidence_path=evidence_path, run_id=run_id, proc=proc)
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
    assert info.depth == DEPTH  # NOT "gmat-api" -- see this module's docstring.
    assert info.frame_id == "EarthMJ2000Eq"
    assert info.state_space_id == "altavista.cartesian_pos_vel_6"
    assert list(info.goldens) == ["leo_1day_jgm2_8x8_sunmoon"]
    assert len(info.settings_hash) == 64  # SHA-256 hex

    caps = set(info.capabilities)
    for expected in (dynamics_service_pb2.MODEL_CAPABILITY_STEP,
                     dynamics_service_pb2.MODEL_CAPABILITY_PROPAGATE,
                     dynamics_service_pb2.MODEL_CAPABILITY_DERIVATIVES,
                     dynamics_service_pb2.MODEL_CAPABILITY_DETERMINISTIC,
                     dynamics_service_pb2.MODEL_CAPABILITY_STM):
        assert expected in caps, f"expected capability {expected} in {caps}"
    assert dynamics_service_pb2.MODEL_CAPABILITY_SOLVE not in caps


def test_describe_rejects_unknown_model_id(stub):
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Describe(dynamics_service_pb2.DescribeRequest(model_id="not.a.real.model"))
    assert exc_info.value.code() == grpc.StatusCode.NOT_FOUND


# --------------------------------------------------------------------------- Step
def test_step_matches_a_single_chunk_propagate_over_the_same_interval(stub):
    """Internal-consistency check specific to this server's implementation (ADR-002 depth
    2 has no separate "reference" propagator to compare against the way
    `test_gmat_service.py`'s `test_step_matches_direct_altavista_propagate` compares Step
    against `altavista.scenario.Scenario.propagate()`): `Propagate` with
    `sample_interval_s == dt_s` over a horizon exactly `dt_s` long produces exactly one
    internal chunk (`crates/av-dynamics-service/src/propagate.rs::sample_epochs_ns`), which
    is the same `GmatModel::step` call `Step` itself makes -- so the two RPCs' answers
    should agree tightly (identical code path, not merely "close").
    """
    epoch_a1mjd = GOLDEN["epoch_a1mjd"]
    state_km = list(GOLDEN["initial_state"])
    state_si = [x * 1000.0 for x in state_km]
    tai_ns = cdm_adapter.a1mjd_to_tai_ns(epoch_a1mjd)
    dt_s = 300.0

    step_resp = stub.Step(dynamics_service_pb2.StepRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns), dt_s=dt_s))

    prop_resp = stub.Propagate(dynamics_service_pb2.PropagateRequest(
        seed=core_pb2.GaussianState(mean=state_si, epoch_ns=tai_ns),
        horizon_tai_ns=tai_ns + int(dt_s * 1e9), sample_interval_s=dt_s, entity_id="step_vs_propagate"))
    last = prop_resp.trajectory.samples[-1]

    for a, b in zip(list(step_resp.state.state), list(last.mean)):
        assert a == pytest.approx(b, rel=1e-9, abs=1e-6), (list(step_resp.state.state), list(last.mean))
    assert step_resp.state.tai_ns == last.tai_ns


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


def test_step_rejects_controls(stub):
    tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    state_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    req = dynamics_service_pb2.StepRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns), dt_s=60.0, controls=[1.0])
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Step(req)
    assert exc_info.value.code() == grpc.StatusCode.INVALID_ARGUMENT


# --------------------------------------------------------------------------- Propagate
@pytest.mark.slow
def test_propagate_matches_golden_within_tolerance(stub):
    """The load-bearing test in this file -- see the module docstring. Tolerance is read
    straight from the golden and never loosened."""
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
    assert traj.provenance.tool == "av-dynamics-service"
    assert traj.provenance.run_id  # non-empty
    assert len(traj.segments) == 1
    assert traj.segments[0].dynamics_model == MODEL_ID
    assert traj.segments[0].dynamics_depth == DEPTH  # "gmat-ffi", not "gmat-api"
    assert len(traj.samples) > 100  # 86400s / 600s + 1 = 145 samples expected

    last = traj.samples[-1]
    expected_si = [x * 1000.0 for x in GOLDEN["final_state"]]
    pos_err = max(abs(a - b) for a, b in zip(list(last.mean)[0:3], expected_si[0:3]))
    vel_err = max(abs(a - b) for a, b in zip(list(last.mean)[3:6], expected_si[3:6]))
    print(f"\n[Rust av-dynamics-service] Propagate vs golden {GOLDEN_PATH.name}: "
         f"pos_err={pos_err:.6e} m (tolerance_m={GOLDEN['tolerance_m']}), "
         f"vel_err={vel_err:.6e} m/s (tolerance_mps={GOLDEN['tolerance_mps']})")
    assert pos_err <= GOLDEN["tolerance_m"], (
        f"position error {pos_err} m exceeds golden tolerance {GOLDEN['tolerance_m']} m")
    assert vel_err <= GOLDEN["tolerance_mps"], (
        f"velocity error {vel_err} m/s exceeds golden tolerance {GOLDEN['tolerance_mps']} m/s")


@pytest.mark.slow
def test_propagate_covariance_true_matches_golden_stm(stub):
    """Pinned against `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `"stm"` block, exactly
    like `test_gmat_service.py::test_propagate_covariance_true_matches_golden_stm` -- both
    services reproduce the *same* reference (GMAT's own propagator run once to generate the
    golden), independently, at their own ADR-002 depth. This server's depth-2 mechanism
    (`av_dynamics::stm::StmAugmented` integrating `d(Phi)/dt = A Phi` alongside the state,
    `crates/av-dynamics-service/src/propagate.rs::run_with_covariance`) is a genuinely
    different computation path from depth 1's "read GMAT's own STM back after stepping its
    native propagator" -- so tight agreement here is a real independent-implementation
    check, not the near-determinism gmat-service's own README notes for its own comparison.
    """
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

    assert len(traj.samples) > 100
    first = traj.samples[0]
    assert len(first.cov) == 36
    # Phi(t0,t0) = I exactly -> P(t0) = P0 exactly.
    for a, b in zip(first.cov, p0_si):
        assert a == pytest.approx(b, rel=1e-9, abs=1e-9)

    last = traj.samples[-1]
    assert len(last.cov) == 36
    expected_cov = GOLDEN["stm"]["cov_t1_si"]
    cov_abs_err = max(abs(a - b) for a, b in zip(last.cov, expected_cov))
    cov_norm = max(abs(v) for v in expected_cov)
    cov_rel_err = cov_abs_err / cov_norm
    print(f"\n[Rust av-dynamics-service] Propagate covariance vs golden {GOLDEN_PATH.name}: "
         f"max abs err {cov_abs_err:.6e}, rel err {cov_rel_err:.3e}")
    assert cov_rel_err < 1e-6, f"covariance relative error {cov_rel_err} exceeds 1e-6 of the golden's own covariance norm"

    expected_si = [x * 1000.0 for x in GOLDEN["final_state"]]
    pos_err = max(abs(a - b) for a, b in zip(list(last.mean)[0:3], expected_si[0:3]))
    vel_err = max(abs(a - b) for a, b in zip(list(last.mean)[3:6], expected_si[3:6]))
    assert pos_err <= GOLDEN["tolerance_m"]
    assert vel_err <= GOLDEN["tolerance_mps"]


def test_propagate_covariance_true_without_seed_cov_is_invalid_argument(stub):
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


def test_propagate_rejects_impulses_as_unimplemented(stub):
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    req = dynamics_service_pb2.PropagateRequest(
        seed=core_pb2.GaussianState(mean=seed_mean_si, epoch_ns=epoch_tai_ns),
        horizon_tai_ns=epoch_tai_ns + int(3600 * 1e9), sample_interval_s=600.0,
        impulses=[dynamics_service_pb2.Impulse(tai_ns=epoch_tai_ns, delta_v_mps=[1.0, 0.0, 0.0])])
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Propagate(req)
    assert exc_info.value.code() == grpc.StatusCode.UNIMPLEMENTED


def test_propagate_rejects_a_different_output_frame_as_unimplemented(stub):
    epoch_tai_ns = cdm_adapter.a1mjd_to_tai_ns(GOLDEN["epoch_a1mjd"])
    seed_mean_si = [x * 1000.0 for x in GOLDEN["initial_state"]]
    req = dynamics_service_pb2.PropagateRequest(
        seed=core_pb2.GaussianState(mean=seed_mean_si, epoch_ns=epoch_tai_ns),
        horizon_tai_ns=epoch_tai_ns + int(3600 * 1e9), sample_interval_s=600.0,
        output_frame_id="SomeOtherFrame")
    with pytest.raises(grpc.RpcError) as exc_info:
        stub.Propagate(req)
    assert exc_info.value.code() == grpc.StatusCode.UNIMPLEMENTED


# --------------------------------------------------------------------------- Derivatives
def test_derivatives_matches_finite_difference_of_step(stub):
    """Same self-consistency check as `test_gmat_service.py`'s test of the same name (loose
    tolerance by the same reasoning: a first-order finite-difference check of a smooth,
    slowly-varying acceleration field, not a golden-tolerance physics check)."""
    epoch_a1mjd = GOLDEN["epoch_a1mjd"]
    tai_ns = cdm_adapter.a1mjd_to_tai_ns(epoch_a1mjd)
    state_si = [x * 1000.0 for x in GOLDEN["initial_state"]]

    deriv_resp = stub.Derivatives(dynamics_service_pb2.DerivativesRequest(
        state=dynamics_service_pb2.StateVector(state=state_si, tai_ns=tai_ns)))
    state_dot = list(deriv_resp.state_dot)
    assert len(state_dot) == 6
    assert state_dot[0:3] == pytest.approx(state_si[3:6], rel=1e-12)

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


# --------------------------------------------------------------------------- Evidence log
def test_evidence_log_records_one_sorted_key_jsonl_line_per_successful_rpc(stub, server):
    """`crates/av-dynamics-service/src/evidence.rs`: mirrors
    `gmat_service.evidence.EvidenceLog`'s record shape (same nine keys -- the original six
    plus M7.3's `seq`/`prev_hash`/`hash` chain fields) but hashes with the `openssl` crate,
    not `hashlib`/`sha2` -- this test only checks the on-the-wire JSONL shape (which is
    language-agnostic), not which hashing library produced it."""
    before = server.evidence_path.read_text().splitlines() if server.evidence_path.exists() else []
    stub.Describe(dynamics_service_pb2.DescribeRequest())
    after = server.evidence_path.read_text().splitlines()
    assert len(after) == len(before) + 1, "Describe must append exactly one evidence line"

    entry = json.loads(after[-1])
    assert set(entry.keys()) == {
        "epoch", "hash", "method", "prev_hash", "request_hash", "response_hash",
        "run_id", "seq", "settings_hash",
    }
    assert entry["method"] == "Describe"
    assert entry["run_id"] == server.run_id
    assert len(entry["settings_hash"]) == 64
    assert len(entry["request_hash"]) == 64
    assert len(entry["response_hash"]) == 64
    assert len(entry["hash"]) == 64
    assert all(c in "0123456789abcdef" for c in entry["request_hash"])
    assert all(c in "0123456789abcdef" for c in entry["hash"])
    assert isinstance(entry["seq"], int) and entry["seq"] >= 1
    # The raw text itself is already key-sorted (json.dumps(..., sort_keys=True)'s Python
    # contract) -- assert on the *serialized line*, not just the parsed dict, since a dict
    # comparison can't see key order.
    assert list(json.loads(after[-1]).keys()) == sorted(json.loads(after[-1]).keys())


def test_first_evidence_record_chains_from_genesis(server_bin, tmp_path):
    """A fresh evidence file's very first record must carry the literal `"GENESIS"`
    `prev_hash` sentinel (`envelope.proto`'s `SignedBatch.prev_hash` convention, verbatim) --
    started as its own short-lived server/evidence file (not the shared `server` fixture,
    whose evidence file may already have earlier tests' records in it by the time this
    test runs, since pytest does not guarantee ordering across the module's other tests)."""
    port = _free_port()
    admin_port = _free_port()
    evidence_path = tmp_path / "fresh_evidence.jsonl"
    proc = subprocess.Popen(
        [str(server_bin), "--port", str(port), "--admin-port", str(admin_port),
         "--evidence-path", str(evidence_path), "--run-id", "genesis_test"],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    channel = grpc.insecure_channel(f"127.0.0.1:{port}")
    try:
        grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
        stub = dynamics_service_pb2_grpc.DynamicsServiceStub(channel)
        stub.Describe(dynamics_service_pb2.DescribeRequest())
        channel.close()

        lines = evidence_path.read_text().splitlines()
        assert len(lines) == 1
        entry = json.loads(lines[0])
        assert entry["seq"] == 1
        assert entry["prev_hash"] == "GENESIS"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


# --------------------------------------------------------------------------- Admin API
def _get_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=10) as resp:  # noqa: S310 (fixed localhost URL, test-only)
        return json.loads(resp.read().decode("utf-8"))


def test_admin_evidence_endpoint_reports_version_settings_hash_chain_head_and_fips(stub, server):
    """`crates/av-dynamics-service/src/admin.rs`'s `/admin/api/evidence` (ADR-004 question
    63), localhost-only. Issues a real RPC first so `chain_head`/`entries` are provably not
    hard-coded zeros."""
    stub.Describe(dynamics_service_pb2.DescribeRequest())
    body = _get_json(f"http://127.0.0.1:{server.admin_port}/admin/api/evidence")

    assert set(body.keys()) == {"chain_head", "entries", "evidence_path", "fips", "run_id", "settings_hash", "version"}
    assert body["run_id"] == server.run_id
    assert len(body["settings_hash"]) == 64
    assert body["entries"] >= 1
    assert body["chain_head"] != "GENESIS", "at least one record has been written by now"
    assert len(body["chain_head"]) == 64

    fips_posture = body["fips"]
    assert set(fips_posture.keys()) == {"openssl_version", "openssl_version_number", "fips_provider_loadable", "detail"}
    assert fips_posture["openssl_version"].startswith("OpenSSL")
    assert isinstance(fips_posture["fips_provider_loadable"], bool)
    assert fips_posture["detail"]  # non-empty: this is the field a reviewer should read


def test_admin_evidence_verify_endpoint_reports_ok_on_an_untampered_chain(stub, server):
    stub.Describe(dynamics_service_pb2.DescribeRequest())
    body = _get_json(f"http://127.0.0.1:{server.admin_port}/admin/api/evidence/verify")
    assert body["ok"] is True
    assert body["broken_at_seq"] is None
    assert body["detail"] == "chain intact"
    assert body["checked"] >= 1


def test_admin_unknown_path_is_404(server):
    req = urllib.request.Request(f"http://127.0.0.1:{server.admin_port}/nope")
    with pytest.raises(urllib.error.HTTPError) as exc_info:
        urllib.request.urlopen(req, timeout=10)  # noqa: S310
    assert exc_info.value.code == 404


def test_admin_evidence_verify_detects_a_tampered_record_on_disk_and_reports_its_seq(server_bin, tmp_path):
    """The end-to-end tamper-detection proof this task's brief asks for, over the live HTTP
    admin endpoint (not just the Rust unit tests in `crates/av-dynamics-service/src/
    evidence.rs`): starts its own short-lived server/evidence file, writes several genuine
    records through real RPCs, tampers with one record's content directly on disk while the
    server is still running (`EvidenceLog::verify` always re-reads from disk, never trusts
    in-memory state), and proves `/admin/api/evidence/verify` reports the break at the exact
    sequence number that was tampered with.
    """
    port = _free_port()
    admin_port = _free_port()
    evidence_path = tmp_path / "tamper_evidence.jsonl"
    proc = subprocess.Popen(
        [str(server_bin), "--port", str(port), "--admin-port", str(admin_port),
         "--evidence-path", str(evidence_path), "--run-id", "tamper_test"],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    channel = grpc.insecure_channel(f"127.0.0.1:{port}")
    try:
        grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
        stub = dynamics_service_pb2_grpc.DynamicsServiceStub(channel)
        for _ in range(4):
            stub.Describe(dynamics_service_pb2.DescribeRequest())

        clean = _get_json(f"http://127.0.0.1:{admin_port}/admin/api/evidence/verify")
        assert clean["ok"] is True
        assert clean["checked"] == 4

        lines = evidence_path.read_text().splitlines()
        assert len(lines) == 4
        tampered = json.loads(lines[2])
        assert tampered["seq"] == 3
        tampered["method"] = "Propagate"  # content edited; "hash" field left stale
        lines[2] = json.dumps(tampered, sort_keys=True)
        evidence_path.write_text("\n".join(lines) + "\n")

        broken = _get_json(f"http://127.0.0.1:{admin_port}/admin/api/evidence/verify")
        assert broken["ok"] is False
        assert broken["broken_at_seq"] == 3, broken
        assert broken["checked"] == 2, broken
        assert "tampered" in broken["detail"]
    finally:
        channel.close()
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)
