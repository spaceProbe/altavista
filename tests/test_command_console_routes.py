"""R3.5a (`docs/aiplane-plan.md` milestone A5's server half): the `/api/command/*` routes
`altavista/server.py` declares, backed by `altavista/command_client.py`'s real gRPC client
against a REAL, running `av-command` service.

Follows `tests/test_dynamics_service_rs.py`'s own fixture shape exactly: `cargo build`s the
Rust service binary once (module-scoped), starts it as a separate process on an OS-assigned
ephemeral loopback port, polls for readiness over a real gRPC channel (no sleeping), and
drives it for real -- here, `Propose` (which now checks automatically, question 209(a)) and
`Authorize` over a raw gRPC stub to seed real ledger state, then every new HTTP route through
`fastapi.testclient.TestClient`.

# Minting a test token from Python (the brief's own open question)

`crates/av-command/src/test_support.rs::TestIssuer` mints real RS256-signed tokens from
Rust; this project's `.venv` has neither `cryptography` nor `pyjwt` installed (only
`grpcio`/`grpcio-tools`, the `[grpc]` extra -- checked directly, see this repo's own
`pyproject.toml`). Minting a token from Python turned out to be entirely practical without
either: `LocalTestIssuer` below shells out to the system `openssl` CLI (`genrsa`, `rsa
-pubout`, `dgst -sha256 -sign`) -- the exact same RS256-over-a-JWS-signing-input construction
`TestIssuer::mint` performs, using the same crypto backend (OpenSSL) this whole workspace's
crypto rule already requires, as a subprocess rather than a new Python dependency. This is
why the negative (wrong-role) authorize case is a REAL test below, never skipped.

# No network at test time (question 154) / no environment mutation (question 199)

Binding/connecting to `127.0.0.1` never leaves the host's own kernel network stack (mirrors
`crates/av-command/tests/grpc_service.rs`'s own module doc, restated for this file). No test
here calls `os.environ`/`monkeypatch.setenv` to configure anything `av-command` or this
server reads -- every setting reaches `create_app`/the subprocess as an explicit argument or
CLI flag, exactly the way `docs/open-questions.md` question 199 requires.
"""
from __future__ import annotations

import base64
import json
import os
import socket
import subprocess
import time
from pathlib import Path
from types import SimpleNamespace
from typing import List, Optional

import grpc
import pytest
from fastapi.testclient import TestClient

from altavista import command_client
from altavista.pb import authority_pb2, authority_pb2_grpc
from altavista.pb.altavista.v1 import command_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parents[1]
READY_TIMEOUT_S = 90.0
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"

TEST_ISSUER = "https://sso.test.example/"
TEST_AUDIENCE = "av-command"
CONSOLE_ENTITY = "sat-console-1"


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _free_port() -> int:
    """Same bind-then-close trick as `tests/test_dynamics_service_rs.py`'s `_free_port`."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


class LocalTestIssuer:
    """A local RSA-2048 key pair + RS256 signer, generated and driven through the system
    `openssl` CLI -- the Python-side mirror of `crates/av-command/src/test_support.rs::
    TestIssuer`. See this module's own docstring for why this is the answer to the brief's
    "if minting a token from Python is impractical" question: it was not impractical."""

    def __init__(self, tmp_path: Path) -> None:
        self.private_key_path = tmp_path / "issuer_private.pem"
        self.public_key_path = tmp_path / "issuer_public.pem"
        subprocess.run(["openssl", "genrsa", "-out", str(self.private_key_path), "2048"], check=True, capture_output=True)
        subprocess.run(
            ["openssl", "rsa", "-in", str(self.private_key_path), "-pubout", "-out", str(self.public_key_path)], check=True, capture_output=True
        )

    def mint(self, claims: dict) -> str:
        """Mints a real RS256-signed compact JWS over `claims`, mirroring `TestIssuer::mint`'s
        own construction (header `{"alg": "RS256", "typ": "JWT"}`, `.`-joined base64url
        segments, the third segment a real OpenSSL signature over the first two)."""
        header_b64 = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode("utf-8"))
        payload_b64 = _b64url(json.dumps(claims, separators=(",", ":")).encode("utf-8"))
        signing_input = f"{header_b64}.{payload_b64}"
        proc = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(self.private_key_path)], input=signing_input.encode("ascii"), check=True, capture_output=True
        )
        signature_b64 = _b64url(proc.stdout)
        return f"{signing_input}.{signature_b64}"


def _valid_claims(sub: str, groups: List[str]) -> dict:
    """Matches every check `crates/av-command/src/oidc.rs::verify` makes, against the REAL
    wall clock (`av-command`'s own binary always constructs a `SystemClock` -- there is no
    `--clock` flag to inject a `TestClock` into a separate process), never a synthetic epoch."""
    now = int(time.time())
    return {"iss": TEST_ISSUER, "aud": TEST_AUDIENCE, "sub": sub, "iat": now, "exp": now + 3600, "groups": groups, "amr": [], "acr": "", "jti": "test-jti"}


@pytest.fixture(scope="module")
def command_bin():
    """Builds `crates/av-command`'s `av-command` binary once for the module. A build failure
    here is a real failure of this task, not something to skip over -- same posture as
    `tests/test_dynamics_service_rs.py`'s `server_bin` fixture."""
    proc = subprocess.run(["cargo", "build", "-p", "av-command", "--bin", "av-command"], cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-command failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "av-command"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def issuer(tmp_path_factory):
    return LocalTestIssuer(tmp_path_factory.mktemp("av_command_issuer"))


@pytest.fixture(scope="module")
def command_service(command_bin, issuer, tmp_path_factory):
    """Starts a real `av-command` subprocess with its own temporary ledger, the shipped
    policy bundle (`profiles/policies/authority`, exactly what `profiles/execution.yaml`
    itself names), and a minimal profile file carrying just the `authority.roles` this
    file's tests need (`"operators"` grants `"mode"`) -- no `service_roles`/`delegations`,
    since none of the new routes exercise `Dispatch`/`Ack`/`Expire`/`Fail` or delegations."""
    tmp = tmp_path_factory.mktemp("av_command_service")
    ledger_dir = tmp / "ledger"
    policy_dir = REPO_ROOT / "profiles" / "policies" / "authority"
    profile_path = tmp / "profile.yaml"
    profile_path.write_text(
        "authority:\n"
        "  roles:\n"
        "    operators: [\"mode\"]\n"
        "  mfa_amr_methods: []\n"
        "  mfa_acr: \"\"\n"
        "  service_roles: {}\n"
        "  delegations_path: \"\"\n"
    )
    grpc_port = _free_port()
    admin_port = _free_port()
    proc = subprocess.Popen(
        [
            str(command_bin),
            "--bind", f"127.0.0.1:{grpc_port}",
            "--admin-bind", f"127.0.0.1:{admin_port}",
            "--ledger-dir", str(ledger_dir),
            "--policy-dir", str(policy_dir),
            "--profile-path", str(profile_path),
            "--oidc-issuer", TEST_ISSUER,
            "--oidc-audience", TEST_AUDIENCE,
            "--oidc-public-key-path", str(issuer.public_key_path),
            "--run-id", "test_command_console_routes",
        ],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    grpc_endpoint = f"127.0.0.1:{grpc_port}"
    admin_endpoint = f"127.0.0.1:{admin_port}"
    channel = grpc.insecure_channel(grpc_endpoint)
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
            pytest.fail(f"av-command subprocess did not become ready within {READY_TIMEOUT_S}s (returncode={returncode}): {e}\n--- subprocess output ---\n{output}")
        channel.close()
        # `profile_path`/`policy_dir` are exposed alongside the endpoints so a second,
        # independent `av-command` process can be started later against the exact same
        # on-disk configuration (see `test_console_authorized_trail_replays_identically_
        # from_a_second_process_over_the_same_ledger` below) -- never a second, divergently
        # constructed profile file.
        yield SimpleNamespace(
            grpc_endpoint=grpc_endpoint, admin_endpoint=admin_endpoint, proc=proc, ledger_dir=ledger_dir,
            profile_path=profile_path, policy_dir=policy_dir,
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def _propose(command_service, command_id: str, entity_id: str, command_class: str, rationale: str, evidence_ids: List[str]):
    """Seeds real ledger state through a raw gRPC stub -- never through the HTTP routes under
    test (there is no HTTP Propose route; this is exactly `Propose` the way a real proposer
    would call it). Question 209(a): `Propose` now runs the check edge automatically, as a
    separate logged transition, so the response this returns already carries the command at
    `CHECKED` (with its `PolicyDecision` attached) -- there is no more separate "propose, then
    check" helper pair; a bare `Propose` already yields CHECKED, which is exactly why this
    single function replaces this file's old `_propose_and_check`/`_propose_only` pair. Raises
    `grpc.RpcError` (never returns) for a refused proposal -- a policy denial (typed
    `PERMISSION_DENIED`, D3) included."""
    channel = grpc.insecure_channel(command_service.grpc_endpoint)
    try:
        stub = authority_pb2_grpc.CommandAuthorityServiceStub(channel)
        command = command_pb2.Command(id=command_id, entity_id=entity_id, command_class=command_class)
        proposal = command_pb2.CommandProposal(command=command, rationale=rationale, evidence_ids=evidence_ids)
        return stub.Propose(authority_pb2.ProposeRequest(proposal=proposal, principal="model-x"))
    finally:
        channel.close()


def _partition_file(ledger_dir: Path, partition: str) -> Path:
    """Recomputes `av-command`'s own documented on-disk partition filename
    (`crates/av-command/src/ledger.rs`'s module doc: SHA-256 hex of the partition name, plus a
    `.ledger` suffix) -- the same documented, public on-disk contract
    `crates/av-command/tests/grpc_service.rs::ledger_file_path` exercises, restated here a
    second time, in Python, rather than reached into as a private implementation detail."""
    import hashlib

    digest = hashlib.sha256(partition.encode("utf-8")).hexdigest()
    return ledger_dir / f"{digest}.ledger"


def _propose_leaving_it_proposed_via_a_forced_check_io_failure(command_service, command_id: str, entity_id: str, command_class: str) -> None:
    """Question 209(a)/D4, reproduced at the console layer: forces `Propose`'s own automatic
    check to fail with I/O (never a policy denial), which is the one path that still leaves a
    command genuinely `PROPOSED` after `Propose` returns. Forced by a **deterministic
    filesystem fault**, never `time.sleep` and never a mutation of this process's own
    environment (question 199) -- the identical technique
    `crates/av-command/tests/grpc_service.rs::proposes_automatic_check_io_failure_leaves_the_
    command_proposed_and_check_then_retries_it` already proves at the Rust layer: a first,
    unrelated command warms `av-command`'s in-memory chain-state cache for this partition (so
    the SECOND command's own `PROPOSED` append needs only WRITE access to the partition file,
    not a read), then the partition file is made write-only so the automatic check's own
    rate-source read (a fresh, independent file open in READ mode) fails deterministically --
    no race, no timing dependency: the permission changes entirely between two separate gRPC
    calls, never mid-call. Permissions are restored before this returns, so no later test in
    this module is affected.
    """
    _propose(command_service, f"{command_id}-warm", entity_id, command_class, "warm the partition's chain-state cache", [])

    partition_file = _partition_file(command_service.ledger_dir, entity_id)
    original_mode = partition_file.stat().st_mode
    os.chmod(partition_file, 0o200)  # write-only: see this function's own doc.
    try:
        with pytest.raises(grpc.RpcError) as exc_info:
            _propose(command_service, command_id, entity_id, command_class, "reason", [])
    finally:
        os.chmod(partition_file, original_mode)
    assert exc_info.value.code() == grpc.StatusCode.INTERNAL, exc_info.value
    assert "remains PROPOSED" in exc_info.value.details(), exc_info.value.details()


@pytest.fixture()
def client(command_service, tmp_path) -> TestClient:
    app = create_app(
        texture_dir=tmp_path, web_dir=tmp_path, command_endpoint=command_service.grpc_endpoint,
        command_admin_endpoint=command_service.admin_endpoint, command_entities=[CONSOLE_ENTITY],
    )
    return TestClient(app)


# --------------------------------------------------------------------------- proposals
def test_proposals_route_returns_the_real_rationale_and_evidence_ids(client: TestClient, command_service):
    _propose(command_service, "cmd-console-a", CONSOLE_ENTITY, "mode", "scored radius drifted past threshold", ["run-1/query-7", "run-1/query-9"])

    resp = client.get(f"/api/command/proposals?entity_id={CONSOLE_ENTITY}")
    assert resp.status_code == 200, resp.text
    body = resp.json()
    entry = next(p for p in body["proposals"] if p["commandId"] == "cmd-console-a")
    assert entry["rationale"] == "scored radius drifted past threshold"
    assert entry["evidenceIds"] == ["run-1/query-7", "run-1/query-9"]
    assert entry["entityId"] == CONSOLE_ENTITY
    assert entry["commandClass"] == "mode"
    # Question 209(a)/D9: a bare Propose already checks automatically, so this row's real
    # state -- read off the actual Command, never inferred -- is CHECKED, not PROPOSED.
    assert entry["state"] == "COMMAND_STATE_CHECKED"

    # No ?entity_id= at all -- must sweep the configured entity (CONSOLE_ENTITY) by default.
    resp_default = client.get("/api/command/proposals")
    assert resp_default.status_code == 200, resp_default.text
    default_ids = {p["commandId"] for p in resp_default.json()["proposals"]}
    assert "cmd-console-a" in default_ids


def test_a_policy_denied_class_is_refused_with_a_typed_message_naming_the_decision_and_never_lists(client: TestClient, command_service):
    """Question 209(a)/D10(iv): `"payload"` is unconditionally denied by the shipped policy
    bundle this fixture loads. `Propose` itself refuses it (D3, typed `PERMISSION_DENIED`,
    naming the decision id and deny reasons) -- and the REJECTED record this still leaves on
    the real ledger (durable, queryable -- D3's own point) must never appear among the
    commands `/api/command/proposals` lists as awaiting a human: a REJECTED command needs no
    human step at all."""
    channel = grpc.insecure_channel(command_service.grpc_endpoint)
    try:
        stub = authority_pb2_grpc.CommandAuthorityServiceStub(channel)
        command = command_pb2.Command(id="cmd-console-denied", entity_id=CONSOLE_ENTITY, command_class="payload")
        proposal = command_pb2.CommandProposal(command=command, rationale="flagged for review", evidence_ids=[])
        with pytest.raises(grpc.RpcError) as exc_info:
            stub.Propose(authority_pb2.ProposeRequest(proposal=proposal, principal="model-x"))
    finally:
        channel.close()

    err = exc_info.value
    assert err.code() == grpc.StatusCode.PERMISSION_DENIED, err
    assert "policy denied" in err.details().lower(), err.details()
    assert "decision_id" in err.details(), err.details()
    assert "payload is not admitted by policy" in err.details(), err.details()

    resp = client.get(f"/api/command/proposals?entity_id={CONSOLE_ENTITY}")
    assert resp.status_code == 200, resp.text
    ids = {p["commandId"] for p in resp.json()["proposals"]}
    assert "cmd-console-denied" not in ids, "a REJECTED command needs no human step and must not be listed as awaiting one"


# --------------------------------------------------------------------------- decision
def test_decision_route_returns_the_real_decision_id_and_policy_hash(client: TestClient, command_service):
    checked = _propose(command_service, "cmd-console-decision", CONSOLE_ENTITY, "mode", "reason", [])
    expected = checked.decision
    assert expected.allow, "sanity: mode is unconditionally allowed by the shipped policy"

    resp = client.get("/api/command/commands/cmd-console-decision/decision")
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["decisionId"] == expected.decision_id
    assert body["policyHash"] == expected.policy_hash
    assert len(body["policyHash"]) == 64, "SHA-256 hex"
    assert body["allow"] is True
    assert body["input"]["commandId"] == "cmd-console-decision"
    assert body["input"]["commandClass"] == "mode"


def test_decision_route_is_404_for_a_still_proposed_command(client: TestClient, command_service):
    # Question 209(a): a bare Propose now checks automatically, so the ONLY way a command
    # stays genuinely PROPOSED is the automatic check's own I/O failing (D4) -- forced here
    # deterministically, mirroring the Rust integration test of the identical name's technique.
    _propose_leaving_it_proposed_via_a_forced_check_io_failure(command_service, "cmd-console-proposed-only", CONSOLE_ENTITY, "mode")
    resp = client.get("/api/command/commands/cmd-console-proposed-only/decision")
    assert resp.status_code == 404, resp.text
    assert "Checked" in resp.text

    # It is still queryable, genuinely PROPOSED, on the proposals route awaiting a human.
    proposals_resp = client.get(f"/api/command/proposals?entity_id={CONSOLE_ENTITY}")
    entry = next(p for p in proposals_resp.json()["proposals"] if p["commandId"] == "cmd-console-proposed-only")
    assert entry["state"] == "COMMAND_STATE_PROPOSED"


def test_decision_route_is_404_for_an_unknown_command(client: TestClient):
    resp = client.get("/api/command/commands/cmd-console-never-seen/decision")
    assert resp.status_code == 404, resp.text
    assert "cmd-console-never-seen" in resp.text


# --------------------------------------------------------------------------- trail
def test_trail_route_returns_the_real_transition_sequence_in_order(client: TestClient, command_service, issuer):
    _propose(command_service, "cmd-console-trail", CONSOLE_ENTITY, "mode", "reason", [])
    token = issuer.mint(_valid_claims("operator-trail", ["operators"]))
    auth_resp = client.post("/api/command/commands/cmd-console-trail/authorize", json={"principalToken": token})
    assert auth_resp.status_code == 200, auth_resp.text

    resp = client.get("/api/command/commands/cmd-console-trail/trail")
    assert resp.status_code == 200, resp.text
    transitions = resp.json()["transitions"]
    assert [t["state"] for t in transitions] == ["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED"]
    assert transitions[-1]["principal"] == "operator-trail"
    assert transitions[-1]["ackLevel"] == "ACK_LEVEL_UNSPECIFIED"


# --------------------------------------------------------------------------- A6: the console's replay
class _ReplayServiceStartupError(RuntimeError):
    pass


def _start_replay_service(command_bin: Path, command_service, issuer, run_id: str):
    """Starts a genuine SECOND `av-command` OS process pointed at the SAME `--ledger-dir`
    `command_service` already wrote to, with the identical `--policy-dir`/`--profile-path`/
    OIDC configuration (`command_service.profile_path`/`command_service.policy_dir`, exposed
    by the `command_service` fixture precisely so a second process need not diverge from the
    first). Its own constructor (`crate::service::CommandAuthorityServiceImpl::new`) rebuilds
    every in-memory index -- `commands` included -- from `Ledger::scan_commands` before this
    process serves a single RPC: this is the real production replay path, exercised the same
    way an operator restart would exercise it, never a Python-side re-reader of the ledger
    files. Returns `(endpoint, proc)`; the caller is responsible for terminating `proc`.
    Mirrors the `command_service` fixture's own Popen-then-poll-for-readiness shape, but is
    not itself a fixture (only one test needs a replay process, and it must not start until
    the live trail it will be compared against already exists on disk)."""
    grpc_port = _free_port()
    admin_port = _free_port()
    proc = subprocess.Popen(
        [
            str(command_bin),
            "--bind", f"127.0.0.1:{grpc_port}",
            "--admin-bind", f"127.0.0.1:{admin_port}",
            "--ledger-dir", str(command_service.ledger_dir),
            "--policy-dir", str(command_service.policy_dir),
            "--profile-path", str(command_service.profile_path),
            "--oidc-issuer", TEST_ISSUER,
            "--oidc-audience", TEST_AUDIENCE,
            "--oidc-public-key-path", str(issuer.public_key_path),
            "--run-id", run_id,
        ],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    endpoint = f"127.0.0.1:{grpc_port}"
    channel = grpc.insecure_channel(endpoint)
    try:
        grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
    except Exception as e:
        returncode = proc.poll()
        output = ""
        try:
            if proc.stdout is not None:
                output = proc.stdout.read()
        except Exception:
            pass
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)
        raise _ReplayServiceStartupError(
            f"replay av-command subprocess did not become ready within {READY_TIMEOUT_S}s (returncode={returncode}): {e}\n"
            f"--- subprocess output ---\n{output}"
        ) from e
    finally:
        channel.close()
    return endpoint, proc


def test_console_authorized_trail_replays_identically_from_a_second_process_over_the_same_ledger(
    client: TestClient, command_service, command_bin, issuer
):
    """Milestone A6's remaining gap (`docs/aiplane-plan.md`): `crates/av-gateway/tests/
    ledger_decision_trail_replay.rs` already proves "a replayed run reproduces every
    transition, decision id and proposal from the ledger" for a trail built programmatically
    through the gateway's own propose path. It never drove the CONSOLE's own path -- the only
    place a human's token turns into a transition (`POST /api/command/commands/{id}/
    authorize`, proven end to end by `test_authorize_route_end_to_end_with_a_real_minted_
    token` above). This test closes that gap: a command is driven to `AUTHORIZED` through the
    console's own HTTP route, exactly like a real operator would, then the SAME command's
    trail is read back from a genuinely independent second process and the two trails are
    compared transition for transition -- not just the final state, which would be "a
    guarantee that leaves no trace" if the middle of the sequence silently diverged.

    # Which replay path this reuses, and why

    "Replay" here is `crate::service::CommandAuthorityServiceImpl::new`
    (`crates/av-command/src/service.rs`) rebuilding its `commands` index from `Ledger::
    scan_commands` at construction time -- the exact mechanism that module's own doc comment
    describes ("a second `CommandAuthorityServiceImpl` constructed over the *same* ledger
    directory refuses the same key `Dispatch::dispatch` would have refused in the first
    process, before this process has ever handled a single RPC of its own") and the same
    mechanism `ledger_decision_trail_replay.rs` exercises via a second, independent `Ledger::
    open` in-process. This test exercises it through a genuine second OS process instead (see
    `_start_replay_service`), because that is the only way to reach it without a Rust
    test harness: `av-command`'s own binary is what real operators restart, so starting a
    second one against the same `--ledger-dir` is not a simulation of the replay path, it IS
    the replay path. The replayed trail is then read back through `command_client.get_trail`
    -- the identical function `altavista/server.py`'s own `/trail` route already calls --
    pointed at the replay process's endpoint instead of the live one, so this test adds no
    second implementation of "how to ask for a trail" either.

    `command_client.get_trail`'s live path (against `command_service`, the module's one
    already-running process) answers from `CommandAuthorityServiceImpl.commands`, an
    in-memory `BTreeMap` kept live by every RPC as it happens -- NOT re-read from disk on
    every call (`crate::service`'s own module doc, "In-memory index" section). Calling it a
    second time against the SAME process would therefore only prove the in-memory map matches
    itself, not that the ledger records were enough to reconstruct the trail -- which is why
    this test's replay must be, and is, a second process.
    """
    command_id = "cmd-console-replay"
    _propose(command_service, command_id, CONSOLE_ENTITY, "mode", "reason", [])
    token = issuer.mint(_valid_claims("operator-replay", ["operators"]))
    auth_resp = client.post(f"/api/command/commands/{command_id}/authorize", json={"principalToken": token})
    assert auth_resp.status_code == 200, auth_resp.text
    assert auth_resp.json()["state"] == "COMMAND_STATE_AUTHORIZED"

    # The LIVE trail, through the console's own route -- exactly what an operator's browser
    # would see right after authorizing.
    live_trail = client.get(f"/api/command/commands/{command_id}/trail").json()["transitions"]
    assert [t["state"] for t in live_trail] == [
        "COMMAND_STATE_PROPOSED",
        "COMMAND_STATE_CHECKED",
        "COMMAND_STATE_AUTHORIZED",
    ], live_trail
    assert live_trail[-1]["principal"] == "operator-replay"
    # The CHECKED transition's `reason` carries the decision id (`authority.proto`'s
    # `CommandTransition.reason` doc: "Policy decision id, authorization reason, failure
    # text.", built by `crate::authority::format_reason` as `decision_id=... policy_hash=...
    # allow=...`) -- there is no separate `decisionId` field on a transition, so comparing
    # `reason` verbatim (done below, for the whole trail) is what proves the decision id
    # itself replays, not just the state.
    checked = next(t for t in live_trail if t["state"] == "COMMAND_STATE_CHECKED")
    assert checked["reason"].startswith("decision_id="), checked

    # Only NOW -- after the console has finished authorizing, so the ledger already holds the
    # full three-transition history -- start the second, independent process that will
    # rebuild its own view of this command purely from what is on disk.
    replay_endpoint, replay_proc = _start_replay_service(command_bin, command_service, issuer, "test_command_console_routes_replay")
    try:
        replay_config = command_client.CommandServiceConfig(grpc_endpoint=replay_endpoint)
        replayed_trail = command_client.get_trail(replay_config, command_id)
    finally:
        replay_proc.terminate()
        try:
            replay_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            replay_proc.kill()
            replay_proc.wait(timeout=10)

    assert replayed_trail == live_trail, (
        "a command authorized through the console's own /authorize route must replay "
        "IDENTICALLY from the ledger alone -- every transition, in order, with its state, "
        "principal, reason (which carries the decision id for the CHECKED transition) and "
        f"ack level/delegation id -- not just its final state:\nlive:     {live_trail}\n"
        f"replayed: {replayed_trail}"
    )


# --------------------------------------------------------------------------- authorize
def test_authorize_route_end_to_end_with_a_real_minted_token(client: TestClient, command_service, issuer):
    """Question 209(a)'s own point, proven end to end: `_propose` below issues exactly ONE
    gRPC call (`Propose`) -- it never calls `Check` at all -- and the command still authorizes
    successfully with a right-role token. This is "the human step completing": before this
    ruling, the console could never reach this state at all (the lead's own browser drive
    found `Authorize` on a still-PROPOSED command is by design an illegal edge, and nothing
    called `Check` on a human's behalf)."""
    _propose(command_service, "cmd-console-authorize-ok", CONSOLE_ENTITY, "mode", "reason", [])
    token = issuer.mint(_valid_claims("operator-ok", ["operators"]))

    resp = client.post("/api/command/commands/cmd-console-authorize-ok/authorize", json={"principalToken": token})
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["state"] == "COMMAND_STATE_AUTHORIZED"
    assert body["transitions"][-1]["principal"] == "operator-ok"

    trail = client.get("/api/command/commands/cmd-console-authorize-ok/trail").json()["transitions"]
    assert trail[-1]["state"] == "COMMAND_STATE_AUTHORIZED"
    assert trail[-1]["principal"] == "operator-ok"


def test_authorize_route_refuses_a_wrong_role_token_and_never_leaks_it(client: TestClient, command_service, issuer, caplog):
    """Question 209(a)/D10(iii): the same never-explicitly-checked command shape as the
    end-to-end test above -- `_propose` issues exactly ONE gRPC call -- refused with the
    role-gate reason for a wrong-role token."""
    _propose(command_service, "cmd-console-authorize-wrong-role", CONSOLE_ENTITY, "mode", "reason", [])
    # "nobody" grants no command class at all -- a real, syntactically-valid, correctly-signed
    # token that the profile's role table simply does not list.
    token = issuer.mint(_valid_claims("operator-wrong-role", ["nobody"]))

    with caplog.at_level("DEBUG"):
        resp = client.post("/api/command/commands/cmd-console-authorize-wrong-role/authorize", json={"principalToken": token})

    assert resp.status_code == 403, resp.text
    assert "authz" in resp.text.lower() or "role" in resp.text.lower(), resp.text

    # The token must appear NOWHERE: not in the HTTP response body, and not in any log record
    # this test captured (question 34/201(b)'s "no route logs a token" rule).
    assert token not in resp.text
    for record in caplog.records:
        assert token not in record.getMessage(), f"token leaked into a log record: {record.getMessage()!r}"

    # The command must still be exactly CHECKED (not AUTHORIZED) -- the refusal must not have
    # silently advanced the state machine.
    trail = client.get("/api/command/commands/cmd-console-authorize-wrong-role/trail").json()["transitions"]
    assert trail[-1]["state"] == "COMMAND_STATE_CHECKED"


def test_authorize_route_rejects_a_malformed_body_as_400(client: TestClient, command_service):
    _propose(command_service, "cmd-console-authorize-malformed", CONSOLE_ENTITY, "mode", "reason", [])
    resp = client.post("/api/command/commands/cmd-console-authorize-malformed/authorize", json={})
    assert resp.status_code == 400, resp.text
    assert "principalToken" in resp.text


# --------------------------------------------------------------------------- counters
def test_counters_route_proxies_the_real_admin_evidence_endpoint(client: TestClient, command_service, issuer):
    _propose(command_service, "cmd-console-counters", CONSOLE_ENTITY, "mode", "reason", [])
    token = issuer.mint(_valid_claims("operator-counters", ["nobody"]))  # provokes authz_role_not_granted
    client.post("/api/command/commands/cmd-console-counters/authorize", json={"principalToken": token})

    resp = client.get("/api/command/counters")
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert set(body.keys()) == {"fips", "partitions", "refusals", "run_id", "version"}
    assert body["run_id"] == "test_command_console_routes"
    assert body["refusals"].get("authz_role_not_granted", 0) >= 1, body["refusals"]

    # Cross-check against the real admin endpoint directly, byte for byte on the count.
    import urllib.request

    with urllib.request.urlopen(f"http://{command_service.admin_endpoint}/admin/api/evidence", timeout=10) as raw:  # noqa: S310
        direct = json.loads(raw.read().decode("utf-8"))
    assert direct["refusals"] == body["refusals"]


# --------------------------------------------------------------------------- degraded mode
# These need no Rust binary at all -- see the module doc's "no network at test time" section
# and this task's own brief: "these need no Rust binary and must never be skipped."

def test_no_command_endpoint_configured_answers_a_typed_503_and_other_routes_still_work(tmp_path):
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)  # no command_endpoint at all
    client = TestClient(app)

    resp = client.get("/api/command/proposals?entity_id=sat-1")
    assert resp.status_code == 503, resp.text
    assert "not configured" in resp.text.lower() or "no command" in resp.text.lower(), resp.text

    resp = client.get("/api/command/commands/cmd-1/decision")
    assert resp.status_code == 503, resp.text

    resp = client.get("/api/command/commands/cmd-1/trail")
    assert resp.status_code == 503, resp.text

    resp = client.post("/api/command/commands/cmd-1/authorize", json={"principalToken": "x"})
    assert resp.status_code == 503, resp.text

    resp = client.get("/api/command/counters")
    assert resp.status_code == 503, resp.text

    # Every pre-existing route must still work -- the whole point of "configuration, not a
    # hard dependency" (question 85's own rule, applied to av-command the same way it already
    # applies to grpcio itself).
    assert client.get("/api/health").status_code == 200
    assert client.get("/api/scenarios").status_code == 200


def test_command_endpoint_configured_but_unreachable_answers_a_typed_503(tmp_path):
    unused_port = _free_port()  # bound-then-closed: nothing is listening here
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, command_endpoint=f"127.0.0.1:{unused_port}", command_admin_endpoint=f"127.0.0.1:{unused_port}")
    client = TestClient(app)

    resp = client.get("/api/command/proposals?entity_id=sat-1")
    assert resp.status_code == 503, resp.text
    assert "unreachable" in resp.text.lower(), resp.text

    resp = client.get("/api/command/counters")
    assert resp.status_code == 503, resp.text
    assert "unreachable" in resp.text.lower(), resp.text

    # Every pre-existing route must still work.
    assert client.get("/api/health").status_code == 200


def test_grpcio_absent_answers_a_typed_503_and_other_routes_still_work(tmp_path, monkeypatch):
    """Simulates "grpcio is not installed" without actually uninstalling it (which would take
    every other test in this project's suite down with it) -- `altavista.command_client`'s
    own module-level `grpc` binding is exactly what a real absent-grpcio interpreter would
    leave `None` at (see that module's own `try`/`except ImportError`), so monkeypatching it
    to `None` here exercises the identical code path a real absent install would take."""
    monkeypatch.setattr(command_client, "grpc", None)
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, command_endpoint="127.0.0.1:1")
    client = TestClient(app)

    resp = client.get("/api/command/proposals?entity_id=sat-1")
    assert resp.status_code == 503, resp.text
    assert "grpc" in resp.text.lower() and "extra" in resp.text.lower(), resp.text

    assert client.get("/api/health").status_code == 200
