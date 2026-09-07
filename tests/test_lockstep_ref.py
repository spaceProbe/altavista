"""Tests for services/lockstep-ref (M13.2, `docs/open-questions.md` question 107): the
reference `altavista.v1.LockstepService` implementation `crates/av-kernel`'s
`BINDING_KIND_CONTAINER` executor path is tested against.

The server runs as a **separate process** (`python -m lockstep_ref`), exactly like
`tests/test_gmat_service.py` runs `gmat_service` -- readiness is awaited with
`grpc.channel_ready_future(...).result(timeout=...)`, never a bare `sleep`, and the
subprocess is always terminated in the fixture's `finally` block.

These tests exercise the Python reference process directly (a plain Python gRPC client
talking to it), independent of `av-kernel`'s Rust client -- proving on its own terms the
brief's "integrates a SIGNAL input into a SIGNAL output and exposes one named output" claim,
and the process's port-set/refusal/lie-on-demand behaviour the Rust side
(`crates/av-kernel/tests/drm_container.rs`) drives the *same* subprocess through, via the
*same* environment-variable knobs, to prove the executor's own protocol checks.
"""
from __future__ import annotations

import os
import socket
import struct
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import grpc
import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SERVICE_DIR = REPO_ROOT / "services" / "lockstep-ref"

if str(SERVICE_DIR) not in sys.path:
    sys.path.insert(0, str(SERVICE_DIR))
from altavista.pb.altavista.v1 import lockstep_pb2, lockstep_pb2_grpc, system_pb2  # noqa: E402

READY_TIMEOUT_S = 30.0


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _encode_signal(value: float) -> bytes:
    return struct.pack("<d", value)


def _decode_signal(payload: bytes) -> float:
    assert len(payload) == 8
    return struct.unpack("<d", payload)[0]


def _start_server(extra_env: "dict[str, str] | None" = None, **serve_kwargs) -> SimpleNamespace:
    port = serve_kwargs.pop("port", None) or _free_port()
    args = [sys.executable, "-m", "lockstep_ref", "--port", str(port)]
    for flag, key in (("--in-port", "in_port"), ("--out-port", "out_port"), ("--output-name", "output_name")):
        if key in serve_kwargs:
            args += [flag, str(serve_kwargs[key])]
    env = dict(os.environ)
    if extra_env:
        env.update(extra_env)
    proc = subprocess.Popen(args, cwd=str(SERVICE_DIR), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    channel = grpc.insecure_channel(f"127.0.0.1:{port}")
    try:
        grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
    except Exception as e:
        channel.close()
        returncode = proc.poll()
        proc.terminate()
        output = proc.stdout.read() if proc.stdout is not None else ""
        pytest.fail(f"lockstep-ref subprocess did not become ready within {READY_TIMEOUT_S}s (returncode={returncode}): {e}\n--- subprocess output ---\n{output}")
    stub = lockstep_pb2_grpc.LockstepServiceStub(channel)
    return SimpleNamespace(channel=channel, stub=stub, port=port, proc=proc)


def _stop_server(s: SimpleNamespace) -> None:
    s.channel.close()
    s.proc.terminate()
    try:
        s.proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        s.proc.kill()
        s.proc.wait(timeout=10)


@pytest.fixture
def server():
    s = _start_server()
    try:
        yield s
    finally:
        _stop_server(s)


def _port(name: str, kind: int, direction: int) -> system_pb2.Port:
    return system_pb2.Port(name=name, kind=kind, direction=direction, schema="signal")


def _good_ports() -> list:
    return [
        _port("in", system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_IN),
        _port("out", system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_OUT),
    ]


def _bind(s, run_id="run-1", instance="sig", ports=None, start_tai_ns=0, base_period_ns=1_000_000_000, step_period_ns=1_000_000_000, seed=42):
    req = lockstep_pb2.LockstepBindRequest(
        run_id=run_id, instance=instance, ports=(ports if ports is not None else _good_ports()),
        start_tai_ns=start_tai_ns, base_period_ns=base_period_ns, step_period_ns=step_period_ns, seed=seed,
    )
    return s.stub.Bind(req)


def test_bind_succeeds_with_the_declared_ports_and_reports_a_stable_hash(server):
    resp = _bind(server)
    assert resp.lockstep_capable is True
    assert resp.refusal_reason == ""
    assert len(resp.binding_hash) == 64, "hex-encoded SHA-256"
    int(resp.binding_hash, 16)  # is actually hex
    assert resp.version == "lockstep-ref/0.1"


def test_bind_refuses_on_a_port_set_mismatch(server):
    wrong_ports = [_port("wrong_name", system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_IN), _good_ports()[1]]
    resp = _bind(server, ports=wrong_ports)
    assert resp.lockstep_capable is False
    assert "port" in resp.refusal_reason.lower()
    assert resp.binding_hash == ""


def test_bind_refuses_on_a_direction_mismatch_even_with_the_right_names(server):
    bad = [_port("in", system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_OUT), _good_ports()[1]]
    resp = _bind(server, ports=bad)
    assert resp.lockstep_capable is False


def test_bind_refuses_unconditionally_when_the_refuse_env_is_set():
    s = _start_server(extra_env={"LOCKSTEP_REF_REFUSE": "1"})
    try:
        resp = _bind(s)  # correct ports, but the process is configured to always refuse
        assert resp.lockstep_capable is False
        assert "refused for testing" in resp.refusal_reason
    finally:
        _stop_server(s)


def test_step_integrates_a_signal_input_into_a_signal_output_and_a_named_output(server):
    bind = _bind(server, start_tai_ns=0, step_period_ns=1_000_000_000)
    assert bind.lockstep_capable

    # Three 1-second steps at constant input 2.0, 3.0, -1.0 -- integral should accumulate
    # value * dt_s at each step (explicit-Euler, held constant over the step -- see
    # server.py's own doc comment).
    running = 0.0
    t = 0
    for seq, value in enumerate([2.0, 3.0, -1.0], start=1):
        until = t + 1_000_000_000
        req = lockstep_pb2.LockstepStepRequest(
            sequence=seq, until_tai_ns=until,
            inputs=[lockstep_pb2.PortMessage(port="in", tai_ns=t, payload=_encode_signal(value))],
        )
        resp = server.stub.Step(req)
        running += value * 1.0
        assert resp.sequence == seq
        assert resp.reached_tai_ns == until
        assert len(resp.outputs) == 1
        assert resp.outputs[0].port == "out"
        assert resp.outputs[0].tai_ns == until
        assert _decode_signal(resp.outputs[0].payload) == pytest.approx(running, abs=1e-12)
        assert resp.named_outputs["integral"] == pytest.approx(running, abs=1e-12)
        t = until


def test_step_with_no_input_message_treats_the_signal_as_zero(server):
    bind = _bind(server)
    assert bind.lockstep_capable
    req = lockstep_pb2.LockstepStepRequest(sequence=1, until_tai_ns=1_000_000_000, inputs=[])
    resp = server.stub.Step(req)
    assert resp.named_outputs["integral"] == 0.0


def test_reset_zeroes_the_running_integral(server):
    bind = _bind(server)
    assert bind.lockstep_capable
    req = lockstep_pb2.LockstepStepRequest(sequence=1, until_tai_ns=1_000_000_000, inputs=[lockstep_pb2.PortMessage(port="in", tai_ns=0, payload=_encode_signal(5.0))])
    resp = server.stub.Step(req)
    assert resp.named_outputs["integral"] == pytest.approx(5.0)

    reset_resp = server.stub.Reset(lockstep_pb2.LockstepResetRequest(sequence=2, tai_ns=1_000_000_000, reason="power_cycle"))
    assert reset_resp.sequence == 2

    req2 = lockstep_pb2.LockstepStepRequest(sequence=3, until_tai_ns=2_000_000_000, inputs=[])
    resp2 = server.stub.Step(req2)
    assert resp2.named_outputs["integral"] == 0.0, "Reset must zero the integral, not merely re-anchor the clock"


def test_step_before_bind_is_refused(server):
    with pytest.raises(grpc.RpcError) as excinfo:
        server.stub.Step(lockstep_pb2.LockstepStepRequest(sequence=1, until_tai_ns=1_000_000_000, inputs=[]))
    assert excinfo.value.code() == grpc.StatusCode.FAILED_PRECONDITION


def test_lie_reached_at_step_env_fires_exactly_once():
    s = _start_server(extra_env={"LOCKSTEP_REF_LIE_REACHED_AT_STEP": "2"})
    try:
        assert _bind(s).lockstep_capable
        t = 0
        for seq in (1, 2, 3):
            until = t + 1_000_000_000
            resp = s.stub.Step(lockstep_pb2.LockstepStepRequest(sequence=seq, until_tai_ns=until, inputs=[]))
            if seq == 2:
                assert resp.reached_tai_ns == until + 1, "the lie fires on exactly the named step"
            else:
                assert resp.reached_tai_ns == until, f"step {seq} must be correct -- the lie fires only once"
            t = until
    finally:
        _stop_server(s)


def test_lie_sequence_at_step_env_fires_exactly_once():
    s = _start_server(extra_env={"LOCKSTEP_REF_LIE_SEQUENCE_AT_STEP": "2"})
    try:
        assert _bind(s).lockstep_capable
        t = 0
        for seq in (1, 2, 3):
            until = t + 1_000_000_000
            resp = s.stub.Step(lockstep_pb2.LockstepStepRequest(sequence=seq, until_tai_ns=until, inputs=[]))
            if seq == 2:
                assert resp.sequence == seq + 1, "the lie fires on exactly the named step"
            else:
                assert resp.sequence == seq, f"step {seq} must be correct -- the lie fires only once"
            t = until
    finally:
        _stop_server(s)


def test_two_fresh_servers_given_the_identical_bind_and_step_sequence_produce_byte_identical_responses():
    """Determinism at the reference-process level (mirrors the Rust side's byte-identical
    `RunProducts` test, one layer down): the same `Bind` parameters and the same ordered
    `Step` inputs against two independently started processes must serialize to exactly the
    same bytes -- no wall clock, no unseeded randomness anywhere in this reference model."""
    s1 = _start_server()
    s2 = _start_server()
    try:
        b1 = _bind(s1, run_id="det", instance="sig", seed=7)
        b2 = _bind(s2, run_id="det", instance="sig", seed=7)
        assert b1.SerializeToString(deterministic=True) == b2.SerializeToString(deterministic=True)

        t = 0
        for seq, value in enumerate([1.5, -2.5, 0.25], start=1):
            until = t + 500_000_000
            req = lockstep_pb2.LockstepStepRequest(sequence=seq, until_tai_ns=until, inputs=[lockstep_pb2.PortMessage(port="in", tai_ns=t, payload=_encode_signal(value))])
            r1 = s1.stub.Step(req)
            r2 = s2.stub.Step(req)
            assert r1.SerializeToString(deterministic=True) == r2.SerializeToString(deterministic=True)
            t = until
    finally:
        _stop_server(s1)
        _stop_server(s2)


def test_shutdown_stops_the_server(server):
    server.stub.Shutdown(lockstep_pb2.LockstepShutdownRequest(run_id="run-1"))
    # The process should exit on its own shortly after Shutdown -- poll rather than sleep a
    # fixed guess.
    returncode = server.proc.wait(timeout=10)
    assert returncode == 0


# ==============================================================================================
# Docker image lifecycle (M15.3, `docs/open-questions.md` question 118): build this package's
# own Dockerfile, push it to a throwaway loopback-only local `registry:2` container this test
# starts and stops itself (never a real/external registry), recover the real digest Docker
# assigned it on push, pull that exact image *by digest*, run it published on loopback, Bind,
# and stop + remove it -- all with the plain `docker` CLI, independent of
# `av_lockstep::docker::ManagedContainer` (that Rust module's own lifecycle is exercised end to
# end by `crates/av-lockstep/tests/docker_lifecycle.rs` and
# `crates/av-kernel/tests/drm_container.rs::docker_image_lifecycle_through_execute_...`) -- this
# test is this package's own, Rust-independent proof that the image it builds actually behaves
# correctly when pulled by digest and run as a container: a real bind, a real digest-dependent
# `binding_hash`, a real stop + remove.
#
# Gated on `docker info` succeeding -- question 118: "tests run only when docker info succeeds,
# and otherwise skip with a recorded reason." `pytest.skip(reason)` plus this repository's
# `pyproject.toml` now running pytest with `-rs` (added by this same task) is what makes that
# reason actually show up in the terminal summary under a *plain* `pytest -q` invocation --
# verified directly (this task's own report): without `-rs`, `pytest -q` prints only a bare "s"
# per skipped test and a "N skipped" count, never the reason text itself.
# ==============================================================================================


def _docker_unavailable_reason() -> "str | None":
    """`None` iff `docker info` succeeds. Otherwise a short, human-readable reason -- never
    raises: a missing `docker` binary and a present-but-unreachable daemon are both just
    "not available" to a caller, exactly `av_lockstep::docker::docker_available`'s own contract
    on the Rust side (mirrored here rather than shelled out to Cargo, since this test has no
    other reason to build any Rust code)."""
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"docker is not installed or could not be launched: {e}"
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip().splitlines()
        return f"`docker info` exited {result.returncode}: {detail[-1] if detail else '(no stderr)'}"
    return None


def _docker(*args: str) -> str:
    result = subprocess.run(["docker", *args], cwd=REPO_ROOT, capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, f"`docker {' '.join(args)}` failed: {result.stderr}"
    return result.stdout.strip()


def test_docker_image_lifecycle_pull_by_digest_run_bind_and_remove():
    reason = _docker_unavailable_reason()
    if reason is not None:
        pytest.skip(f"Docker not available: {reason}")

    local_tag = "lockstep-ref:pytest-docker-lifecycle"
    registry_id = None
    pushed_ref = None
    container_id = None
    try:
        # 1. Build the image exactly as a human operator would (this package's own Dockerfile
        #    doc comment: "Build from the REPOSITORY ROOT").
        _docker("build", "-f", "services/lockstep-ref/Dockerfile", "-t", local_tag, ".")

        # 2. A throwaway, loopback-only local registry (question 118's own "no real/external
        #    registry" rule) -- removed in the `finally` block below.
        registry_id = _docker("run", "-d", "-p", "127.0.0.1::5000", "registry:2")
        registry_port_line = _docker("port", registry_id, "5000")
        registry_port = int(registry_port_line.splitlines()[0].rsplit(":", 1)[-1])

        # 3. Tag and push -- a real registry round trip: `docker pull` below resolves this exact
        #    `<repo>@<digest>` reference against the registry this test just stood up.
        pushed_ref = f"127.0.0.1:{registry_port}/lockstep-ref:test"
        _docker("tag", local_tag, pushed_ref)
        _docker("push", pushed_ref)

        # 4. Recover the real digest Docker assigned on push (never invented/assumed).
        repo_digests = _docker("inspect", "--format={{index .RepoDigests 0}}", pushed_ref)
        assert "@sha256:" in repo_digests, repo_digests
        real_digest = repo_digests.rsplit("@", 1)[-1]
        image_ref = f"127.0.0.1:{registry_port}/lockstep-ref"

        # 5. Pull by digest and run, published on loopback -- twice, with two different
        #    IMAGE_DIGEST env values, to prove `binding_hash` actually varies with the digest
        #    (question 118: "binding_hash includes the digest") and is not just a Bind that
        #    happens to succeed. What this would fail against: an implementation that never
        #    threads `image_digest` into the running container's own environment at all --
        #    `hash_a == hash_b` regardless of what `IMAGE_DIGEST` is set to.
        def bind_hash_for_digest_env(digest_env: str) -> "tuple[str, str]":
            nonlocal container_id
            image_at_digest = f"{image_ref}@{real_digest}"
            _docker("pull", image_at_digest)
            cid = _docker("run", "-d", "-p", "127.0.0.1::50070", "-e", f"IMAGE_DIGEST={digest_env}", image_at_digest)
            container_id = cid
            try:
                host_port_line = _docker("port", cid, "50070")
                host_port = int(host_port_line.splitlines()[0].rsplit(":", 1)[-1])
                channel = grpc.insecure_channel(f"127.0.0.1:{host_port}")
                grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
                stub = lockstep_pb2_grpc.LockstepServiceStub(channel)
                resp = _bind(SimpleNamespace(stub=stub))
                assert resp.lockstep_capable, resp.refusal_reason
                assert len(resp.binding_hash) == 64
                return resp.binding_hash, cid
            finally:
                _docker("rm", "-f", cid)
                container_id = None

        hash_a, _ = bind_hash_for_digest_env("sha256:" + "a" * 64)
        hash_b, _ = bind_hash_for_digest_env("sha256:" + "b" * 64)
        assert hash_a != hash_b, "binding_hash must differ when IMAGE_DIGEST differs -- question 118's own 'binding_hash includes the digest' rule"
    finally:
        # Never leave a container, the throwaway registry, or the pushed/built image tags
        # behind, success or failure.
        if container_id is not None:
            subprocess.run(["docker", "rm", "-f", container_id], capture_output=True)
        if registry_id is not None:
            subprocess.run(["docker", "rm", "-f", registry_id], capture_output=True)
        if pushed_ref is not None:
            subprocess.run(["docker", "rmi", "-f", pushed_ref], capture_output=True)
        subprocess.run(["docker", "rmi", "-f", local_tag], capture_output=True)
