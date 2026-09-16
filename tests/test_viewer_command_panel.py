"""R3.5b (docs/aiplane-plan.md milestone A5's browser half; docs/open-questions.md
question 201(d)): the command console panel in the tiling layout.

Two independent things this file proves, both against REAL artifacts, never a mock of
either:

1. **Part 1 -- the profile signal.** `altavista/server.py`'s `Hub` now stamps
   `scenario["profileId"]` beside `scenario["imagery"]`, from the exact same `profile`
   string `create_app(profile=...)` already took (R3.5b; no new parameter). Proven here
   through the real `/api/scenario` HTTP route, never by reading `Hub` internals.

2. **Part 3 -- the panel's own headless check.** Follows `tests/test_command_console_
   routes.py`'s own fixture shape exactly (duplicated, not imported -- this repo's
   existing viewer test files each stay self-contained, `tests/test_viewer_feasibility_
   panel.py`'s own module docstring, restated here): builds and starts a REAL `av-command`
   service, seeds real ledger state through the raw gRPC stub (`Propose`, which now checks
   automatically -- question 209(a) -- exactly like a real proposer/policy evaluator would
   trigger), then drives every `/api/command/*` route
   through a REAL `create_app(profile="execution", ...)` app via `fastapi.testclient.
   TestClient` to collect REAL payloads -- a real rationale, a real decision id and policy
   hash, a real transition sequence, real refusal counters, and (the one deliberately
   negative case) a REAL wrong-role authorize refusal, captured with its own real message
   text, never fabricated. Those payloads are written to one JSON file and handed to
   `node web/js/command_panel_check.mjs`, which drives the REAL, shipped
   `web/js/panels/command_panel.js` and `web/js/layout/default_layouts.js` ES modules and
   prints one JSON object of named checks -- this file only reads that JSON back and
   asserts specific check names, exactly like `tests/test_viewer_feasibility_panel.py`.

3. **Part 4 -- the authorize path, proven end to end (question 209(b)).** The same
   fixture also proposes one command that stays genuinely CHECKED (never authorized by
   this fixture itself) and mints a real right-role and a real wrong-role token for it.
   `command_panel_check.mjs`'s own "authorize proof" section renders the REAL panel
   around that command, types each real token into the REAL input `render()` built,
   clicks the REAL Authorize button, and captures the exact `(commandId, token)` pair
   its own click handler hands to `onAuthorize` -- written into its JSON output as
   `capturedAuthorizeCalls`. `test_authorize_captured_from_the_panel_really_authorizes_
   and_refuses` below replays those exact captured pairs through the real HTTP
   authorize route against the SAME real, running `av-command` service: the right-role
   token really authorizes (200, `COMMAND_STATE_AUTHORIZED`), the wrong-role token is
   really refused (403, the real role-gate reason), and neither token appears in
   either response. That chain -- the panel's own button produced the arguments, and
   those arguments really authorize -- is the proof the brief asks for.

No network at test time (question 154) / no environment mutation (question 199): see
`tests/test_command_console_routes.py`'s own module doc, restated here verbatim -- this
file follows the identical rule.
"""
from __future__ import annotations

import base64
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace
from typing import List

import grpc
import pytest
from fastapi.testclient import TestClient

from altavista.pb import authority_pb2, authority_pb2_grpc
from altavista.test_env import drain_after_terminate
from altavista.pb.altavista.v1 import command_pb2
from altavista.server import Hub, create_app

REPO_ROOT = Path(__file__).resolve().parent.parent
COMMAND_PANEL_CHECK = REPO_ROOT / "web" / "js" / "command_panel_check.mjs"
READY_TIMEOUT_S = 90.0
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"

TEST_ISSUER = "https://sso.test.example/"
TEST_AUDIENCE = "av-command"
CONSOLE_ENTITY = "sat-console-1"
EMPTY_ENTITY = "sat-console-empty"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/command_panel_check.mjs "
                     "drives real ES modules and is intentionally not ported to Python.")
    return NODE


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


class LocalTestIssuer:
    """Duplicated verbatim from tests/test_command_console_routes.py's own class of the
    same name -- see that module's own docstring for the full "why openssl, not a new
    Python dependency" rationale."""

    def __init__(self, tmp_path: Path) -> None:
        self.private_key_path = tmp_path / "issuer_private.pem"
        self.public_key_path = tmp_path / "issuer_public.pem"
        subprocess.run(["openssl", "genrsa", "-out", str(self.private_key_path), "2048"], check=True, capture_output=True)
        subprocess.run(
            ["openssl", "rsa", "-in", str(self.private_key_path), "-pubout", "-out", str(self.public_key_path)], check=True, capture_output=True
        )

    def mint(self, claims: dict) -> str:
        header_b64 = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode("utf-8"))
        payload_b64 = _b64url(json.dumps(claims, separators=(",", ":")).encode("utf-8"))
        signing_input = f"{header_b64}.{payload_b64}"
        proc = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(self.private_key_path)], input=signing_input.encode("ascii"), check=True, capture_output=True
        )
        signature_b64 = _b64url(proc.stdout)
        return f"{signing_input}.{signature_b64}"


def _valid_claims(sub: str, groups: List[str]) -> dict:
    now = int(time.time())
    return {"iss": TEST_ISSUER, "aud": TEST_AUDIENCE, "sub": sub, "iat": now, "exp": now + 3600, "groups": groups, "amr": [], "acr": "", "jti": "test-jti"}


@pytest.fixture(scope="module")
def command_bin():
    proc = subprocess.run(["cargo", "build", "-p", "av-command", "--bin", "av-command"], cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-command failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "av-command"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def issuer(tmp_path_factory):
    return LocalTestIssuer(tmp_path_factory.mktemp("av_command_panel_issuer"))


@pytest.fixture(scope="module")
def command_service(command_bin, issuer, tmp_path_factory):
    """Same shape as tests/test_command_console_routes.py's own `command_service` fixture
    (duplicated, not imported -- see this module's own docstring)."""
    tmp = tmp_path_factory.mktemp("av_command_panel_service")
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
            "--run-id", "test_viewer_command_panel",
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
            # `proc.stdout.read()` here is a readall on a subprocess that is still running
            # (a readiness wait times out precisely when the child is alive), so it used to
            # block FOREVER -- measured, 34 minutes, in P5 round 3's acceptance gate. See
            # `altavista.test_env.drain_after_terminate`'s own doc for the measurement.
            output = drain_after_terminate(proc)
            pytest.fail(f"av-command subprocess did not become ready within {READY_TIMEOUT_S}s (returncode={returncode}): {e}\n--- subprocess output ---\n{output}")
        channel.close()
        yield SimpleNamespace(grpc_endpoint=grpc_endpoint, admin_endpoint=admin_endpoint, proc=proc, ledger_dir=ledger_dir)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def _propose(command_service, command_id: str, entity_id: str, command_class: str, rationale: str, evidence_ids: List[str]):
    """Question 209(a): `Propose` now runs the check edge automatically, as a separate
    logged transition -- one gRPC call already returns the command at CHECKED (with its
    `PolicyDecision` attached), so this single helper replaces this file's old
    `_propose_and_check`/`_propose_only` pair, which a bare `Propose` made identical."""
    channel = grpc.insecure_channel(command_service.grpc_endpoint)
    try:
        stub = authority_pb2_grpc.CommandAuthorityServiceStub(channel)
        command = command_pb2.Command(id=command_id, entity_id=entity_id, command_class=command_class)
        proposal = command_pb2.CommandProposal(command=command, rationale=rationale, evidence_ids=evidence_ids)
        return stub.Propose(authority_pb2.ProposeRequest(proposal=proposal, principal="model-x"))
    finally:
        channel.close()


def _partition_file(ledger_dir: Path, partition: str) -> Path:
    """Duplicated from `tests/test_command_console_routes.py`'s own helper of the same name
    -- see that module's own docstring for the full "documented on-disk contract" rationale."""
    import hashlib

    digest = hashlib.sha256(partition.encode("utf-8")).hexdigest()
    return ledger_dir / f"{digest}.ledger"


def _propose_leaving_it_proposed_via_a_forced_check_io_failure(command_service, command_id: str, entity_id: str, command_class: str) -> None:
    """Question 209(a)/D4, reproduced here exactly as in `tests/test_command_console_
    routes.py`'s own identically-named helper (duplicated, not imported -- see this module's
    own docstring): the only way a command stays genuinely `PROPOSED` after `Propose` returns
    is the automatic check's own I/O failing, forced here by a deterministic filesystem
    fault -- never `time.sleep`, never a process-environment mutation (question 199)."""
    _propose(command_service, f"{command_id}-warm", entity_id, command_class, "warm the partition's chain-state cache", [])

    partition_file = _partition_file(command_service.ledger_dir, entity_id)
    original_mode = partition_file.stat().st_mode
    os.chmod(partition_file, 0o200)
    try:
        with pytest.raises(grpc.RpcError):
            _propose(command_service, command_id, entity_id, command_class, "unchecked", [])
    finally:
        os.chmod(partition_file, original_mode)


@pytest.fixture()
def client(command_service, tmp_path) -> TestClient:
    """R3.5b: `profile="execution"` -- both to exercise Part 1's real `profileId`
    stamping end to end and because this is the one profile the command console panel's
    default layout actually applies to (question 201(d))."""
    app = create_app(
        texture_dir=tmp_path, web_dir=tmp_path, profile="execution",
        command_endpoint=command_service.grpc_endpoint, command_admin_endpoint=command_service.admin_endpoint,
        command_entities=[CONSOLE_ENTITY, EMPTY_ENTITY],
    )
    return TestClient(app)


# ============================================================================== Part 1
def test_hub_stamps_the_real_profile_id_onto_every_published_scenario(client: TestClient):
    """`create_app(profile="execution")` -> a real `POST /api/scenario` -> the scenario
    read back over `GET /api/scenario/{name}` carries `profileId: "execution"`, beside
    its real `imagery` (unchanged, M19.5). Fails against an implementation that only
    threads `profile` into `load_imagery_config` and never into `Hub` itself."""
    resp = client.post("/api/scenario", json={"name": "cmd-panel-exec-scenario", "spacecraft": []})
    assert resp.status_code == 200, resp.text
    sc = client.get("/api/scenario/cmd-panel-exec-scenario").json()
    assert sc["profileId"] == "execution"
    assert sc["imagery"]["urlTemplate"]  # M19.5's pre-existing signal, unaffected


def test_hub_with_no_profile_id_stamps_nothing_degrade_never_guess():
    """A `Hub` built directly with no `profile_id` (every call site that predates this
    task, and any fixture that constructs one this way) never adds `profileId` at all --
    the exact "no profile key at all" case web/js/layout/default_layouts.js's
    `isExecutionProfile` must degrade on, never guess. Fails against an implementation
    that defaults `profile_id` to something truthy instead of `None`."""
    hub = Hub()
    hub.put({"name": "no-profile-scenario", "spacecraft": []})
    assert "profileId" not in hub.scenarios["no-profile-scenario"]


def test_design_profile_gets_its_own_real_profile_id(tmp_path):
    """The default profile ("design") is not "execution" -- a real, different profile id
    is stamped just the same, proving this is a genuine per-profile signal and not a
    hardcoded "execution" string somewhere."""
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)  # profile defaults to "design"
    client = TestClient(app)
    client.post("/api/scenario", json={"name": "cmd-panel-design-scenario", "spacecraft": []})
    sc = client.get("/api/scenario/cmd-panel-design-scenario").json()
    assert sc["profileId"] == "design"


# ============================================================================== Part 3
@pytest.fixture(scope="module")
def command_panel_check_input_path(tmp_path_factory, command_service, issuer):
    """Seeds real ledger state (`Propose`, which now checks automatically -- question
    209(a) -- over the raw gRPC stub, `_propose` above -- never through an HTTP route,
    exactly like tests/test_command_console_routes.py's own convention), then drives EVERY
    `/api/command/*` route through a real `create_app(profile="execution", ...)` app to
    collect the real payloads web/js/command_panel_check.mjs needs. Module-scoped: one
    real server, one real set of seeded commands, shared by every test function below
    (mirrors tests/test_viewer_feasibility_panel.py's own `panels_check_input_path`)."""
    with tempfile.TemporaryDirectory() as d:
        app = create_app(
            texture_dir=Path(d), web_dir=Path(d), profile="execution",
            command_endpoint=command_service.grpc_endpoint, command_admin_endpoint=command_service.admin_endpoint,
            command_entities=[CONSOLE_ENTITY, EMPTY_ENTITY],
        )
        http = TestClient(app)

        # cmd-view: a real, ordinary Propose -- question 209(a) checks it automatically, so
        # it lands at CHECKED, and the proposals route lists it anyway (commands awaiting a
        # human are PROPOSED *and* CHECKED now) -- still with its real rationale/evidence.
        # `state` (question 209(b)) is the real CommandState enum name this same real
        # Propose call actually left it at -- asserted below against the real route
        # response, never assumed.
        expected_proposal = {
            "commandId": "cmd-view", "entityId": CONSOLE_ENTITY, "commandClass": "mode",
            "state": "COMMAND_STATE_CHECKED",
            "rationale": "scored radius drifted past the execution-profile threshold",
            "evidenceIds": ["run-42/query-3", "run-42/query-5"],
        }
        checked_view = _propose(command_service, expected_proposal["commandId"], expected_proposal["entityId"],
                                 expected_proposal["commandClass"], expected_proposal["rationale"], expected_proposal["evidenceIds"])
        assert checked_view.decision.allow, "sanity: mode is unconditionally allowed, so Propose's automatic Check lands cmd-view at CHECKED"

        # cmd-a: proposed (auto-Checked, real decision), then Authorized with a REAL valid
        # operator token over the real HTTP route -- the command this file's decision/
        # trail/authorize sections are all about.
        checked_a = _propose(command_service, "cmd-a", CONSOLE_ENTITY, "mode", "reason for cmd-a", [])
        assert checked_a.decision.allow, "sanity: mode is unconditionally allowed by the shipped policy"
        operator_token = issuer.mint(_valid_claims("operator-ok", ["operators"]))
        authorize_resp = http.post("/api/command/commands/cmd-a/authorize", json={"principalToken": operator_token})
        assert authorize_resp.status_code == 200, authorize_resp.text
        authorize_success_body = authorize_resp.json()

        # cmd-wrong-role: proposed (auto-Checked), then a REAL wrong-role authorize refusal
        # over the real HTTP route -- both the refusal message AND the resulting counter are
        # real artifacts of this one real call. It stays CHECKED (the refusal never advances
        # the state machine), so -- question 209(a) -- it now DOES appear on the proposals
        # route too (still awaiting a human), unlike before this round.
        _propose(command_service, "cmd-wrong-role", CONSOLE_ENTITY, "mode", "reason for cmd-wrong-role", [])
        wrong_role_token = issuer.mint(_valid_claims("operator-wrong-role", ["nobody"]))
        refusal_resp = http.post("/api/command/commands/cmd-wrong-role/authorize", json={"principalToken": wrong_role_token})
        assert refusal_resp.status_code == 403, refusal_resp.text
        authorize_refusal_error = {"status": refusal_resp.status_code, "message": refusal_resp.json()["detail"]}
        assert wrong_role_token not in refusal_resp.text  # question 201(b)'s own rule, re-checked here too

        # cmd-authorize-proof: proposed (auto-Checked), then deliberately left CHECKED --
        # never authorized by this fixture itself. This is the ONE command Part 4's
        # end-to-end proof exists for: web/js/command_panel_check.mjs's own "authorize
        # proof" section renders the REAL panel around it, types the two REAL tokens
        # minted below into the REAL input, clicks the REAL Authorize button, and captures
        # the exact (commandId, token) pairs the panel's own click handler produces --
        # tests/test_viewer_command_panel.py then replays those captured pairs through the
        # real HTTP authorize route below (test_authorize_captured_from_the_panel_really_
        # authorizes_and_refuses), against this SAME still-CHECKED command.
        authorize_proof_command_id = "cmd-authorize-proof"
        checked_proof = _propose(command_service, authorize_proof_command_id, CONSOLE_ENTITY, "mode", "reason for the authorize proof", [])
        assert checked_proof.decision.allow, "sanity: mode is unconditionally allowed, so this command is really CHECKED, not stuck PROPOSED"
        authorize_proof_right_token = issuer.mint(_valid_claims("operator-authorize-proof-ok", ["operators"]))
        authorize_proof_wrong_token = issuer.mint(_valid_claims("operator-authorize-proof-wrong-role", ["nobody"]))

        # Real proposals -- commands awaiting a human, question 209(a): PROPOSED and CHECKED
        # both (cmd-view, cmd-wrong-role and cmd-authorize-proof, all still CHECKED); cmd-a
        # has moved on to AUTHORIZED and correctly does not appear here. A real empty list
        # for the other entity.
        proposals = http.get(f"/api/command/proposals?entity_id={CONSOLE_ENTITY}").json()
        view_row = next((p for p in proposals["proposals"] if p["commandId"] == expected_proposal["commandId"]), None)
        assert view_row is not None
        assert view_row["state"] == expected_proposal["state"]
        assert any(p["commandId"] == authorize_proof_command_id and p["state"] == "COMMAND_STATE_CHECKED" for p in proposals["proposals"]), proposals
        empty_proposals = http.get(f"/api/command/proposals?entity_id={EMPTY_ENTITY}").json()
        assert empty_proposals["proposals"] == []

        decision = http.get("/api/command/commands/cmd-a/decision").json()
        trail = http.get("/api/command/commands/cmd-a/trail").json()
        assert [t["state"] for t in trail["transitions"]] == [
            "COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED",
        ]

        # cmd-proposed-only: question 209(a)/D4 -- the only way a command stays genuinely
        # PROPOSED now is the automatic check's own I/O failing, forced deterministically --
        # the route's own real 404 for "not yet Checked".
        _propose_leaving_it_proposed_via_a_forced_check_io_failure(command_service, "cmd-proposed-only", CONSOLE_ENTITY, "mode")
        not_yet_checked_resp = http.get("/api/command/commands/cmd-proposed-only/decision")
        assert not_yet_checked_resp.status_code == 404, not_yet_checked_resp.text
        decision_not_yet_checked_error = {"status": 404, "message": not_yet_checked_resp.json()["detail"]}

        counters = http.get("/api/command/counters").json()
        assert counters["refusals"].get("authz_role_not_granted", 0) >= 1

        # Degraded case: no command service configured at all -- a SEPARATE app, no
        # av-command subprocess involved.
        with tempfile.TemporaryDirectory() as d2:
            unconfigured_app = create_app(texture_dir=Path(d2), web_dir=Path(d2))
            unconfigured_client = TestClient(unconfigured_app)
            not_configured_resp = unconfigured_client.get("/api/command/proposals?entity_id=sat-1")
            assert not_configured_resp.status_code == 503, not_configured_resp.text
            not_configured_error = {"status": 503, "message": not_configured_resp.json()["detail"]}

        payload = {
            "proposals": proposals,
            "emptyProposals": empty_proposals,
            "expectedProposal": expected_proposal,
            "decision": decision,
            "expectedDecisionId": checked_a.decision.decision_id,
            "trail": trail,
            "expectedAuthorizedPrincipal": "operator-ok",
            "counters": counters,
            "selectedCommandId": "cmd-a",
            "notYetCheckedCommandId": "cmd-proposed-only",
            "decisionNotYetCheckedError": decision_not_yet_checked_error,
            "notConfiguredError": not_configured_error,
            "authorizeRefusalError": authorize_refusal_error,
            "authorizeSuccessState": authorize_success_body["state"],
            "authorizeProofCommandId": authorize_proof_command_id,
            "rightRoleToken": authorize_proof_right_token,
            "wrongRoleToken": authorize_proof_wrong_token,
        }
        out_dir = tmp_path_factory.mktemp("command_panel_check")
        path = out_dir / "command_panel_input.json"
        path.write_text(json.dumps(payload))
        return path


@pytest.fixture(scope="module")
def command_panel_data(command_panel_check_input_path) -> dict:
    node = _require_node()
    proc = subprocess.run([node, str(COMMAND_PANEL_CHECK), str(command_panel_check_input_path)],
                           cwd=str(COMMAND_PANEL_CHECK.parent), capture_output=True, text=True, timeout=30)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"command_panel_check.mjs did not print valid JSON (exit {proc.returncode})\n"
                              f"stdout: {proc.stdout!r}\nstderr: {proc.stderr}")
    return data


def _failed(data: dict, substring: str) -> list[str]:
    return [c["name"] for c in data["checks"] if substring in c["name"] and not c["pass"]]


def test_proposal_rows_show_the_real_rationale_and_evidence(command_panel_data):
    failed = _failed(command_panel_data, "proposalRows:")
    assert not failed, f"proposalRows checks failed: {failed}"


def test_decision_view_shows_the_real_decision_id_and_policy_hash(command_panel_data):
    failed = _failed(command_panel_data, "decisionView:")
    assert not failed, f"decisionView checks failed: {failed}"


def test_trail_rows_preserve_the_real_transition_order_never_re_sorted(command_panel_data):
    failed = _failed(command_panel_data, "trailRows:")
    assert not failed, f"trailRows checks failed: {failed}"


def test_counter_rows_are_sorted_and_show_the_real_refusal(command_panel_data):
    failed = _failed(command_panel_data, "counterRows:") + _failed(command_panel_data, "counterMeta:")
    assert not failed, f"counter checks failed: {failed}"


def test_error_line_never_invents_or_drops_the_servers_own_message(command_panel_data):
    failed = _failed(command_panel_data, "errorLine:")
    assert not failed, f"errorLine checks failed: {failed}"


def test_degraded_cases_render_the_real_server_message_never_a_generic_one(command_panel_data):
    """Question 148's own required shape ("a failure that leaves no trace") is exactly
    what this guards against: "no command service configured" and "not yet Checked"
    both show the REAL server text this fixture actually captured, not a hardcoded
    string, and never a blank panel."""
    failed = _failed(command_panel_data, 'render: "no command service configured"') + \
        _failed(command_panel_data, 'render: "not yet Checked"') + \
        _failed(command_panel_data, 'no commands awaiting a human')
    assert not failed, f"degraded-case rendering checks failed: {failed}"


def test_render_binds_every_real_data_source_into_visible_text(command_panel_data):
    failed = (
        _failed(command_panel_data, 'render: the real')
        + _failed(command_panel_data, 'render: every real')
        + _failed(command_panel_data, 'render: the trail table')
        + _failed(command_panel_data, 'render: authorize control')
        + _failed(command_panel_data, 'render: with no command selected')
    )
    assert not failed, f"render binding checks failed: {failed}"


def test_state_column_shows_the_real_command_state_and_the_section_says_what_it_lists(command_panel_data):
    """Question 209(b): the proposals table's real State column carries the REAL
    `CommandState` enum name the server sent (verbatim, never re-guessed), shown as a
    readable, shortened label with the full raw value still reachable via the cell's
    `title`; the section's own heading stops calling these rows "Proposals" now that a
    CHECKED command is the normal listed row, not the rare one."""
    failed = (
        _failed(command_panel_data, "proposalRows: real proposal's state")
        + _failed(command_panel_data, "proposalRows: a row with no state key")
        + _failed(command_panel_data, "shortCommandState:")
        + _failed(command_panel_data, "render: the proposals table's State column")
        + _failed(command_panel_data, 'render: the proposals section heading')
    )
    assert not failed, f"state-column/heading checks failed: {failed}"


def test_authorize_proof_captured_from_the_real_panel_button(command_panel_data):
    """Question 209(b), Part 4: web/js/command_panel_check.mjs's own "authorize proof"
    section rendered the REAL panel around a REAL still-CHECKED command, typed two REAL
    tokens (one right-role, one wrong-role) into the REAL input `render()` built, and
    clicked the REAL Authorize button -- this test only checks that capture itself
    succeeded; `test_authorize_captured_from_the_panel_really_authorizes_and_refuses`
    below is the one that replays the captured pairs through the real HTTP route."""
    failed = _failed(command_panel_data, "authorize proof:")
    assert not failed, f"authorize-proof capture checks failed: {failed}"
    calls = {c["label"]: c for c in command_panel_data["capturedAuthorizeCalls"]}
    assert set(calls) == {"wrong-role", "right-role"}, command_panel_data["capturedAuthorizeCalls"]


def test_authorize_captured_from_the_panel_really_authorizes_and_refuses(command_panel_data, client: TestClient):
    """Question 209(b)'s own required end-to-end proof, honest and without a browser:
    the panel's REAL click handler (captured by web/js/command_panel_check.mjs's own
    "authorize proof" section, off a REAL CHECKED command and two REAL tokens this
    file's `command_panel_check_input_path` fixture minted) produced these exact
    (commandId, token) pairs -- replayed HERE through the real `/api/command/commands/
    {id}/authorize` HTTP route of a real `create_app(profile="execution", ...)` app
    against the SAME real, still-running `av-command` service the fixture started.
    This is the chain the brief asks for: the panel's own button produced the
    arguments, and those arguments really authorize (or are really refused).

    Order matters: the wrong-role replay runs FIRST, while the command is still
    genuinely CHECKED, so its refusal is the real role-gate refusal (not some later
    "already AUTHORIZED" illegal-edge refusal); the right-role replay runs second and
    is the one that actually advances the state machine.
    """
    calls = {c["label"]: c for c in command_panel_data["capturedAuthorizeCalls"]}
    assert set(calls) == {"wrong-role", "right-role"}, command_panel_data["capturedAuthorizeCalls"]
    wrong, right = calls["wrong-role"], calls["right-role"]
    assert wrong["commandId"] == right["commandId"], "both captured calls must target the SAME still-CHECKED command"

    refusal = client.post(f"/api/command/commands/{wrong['commandId']}/authorize", json={"principalToken": wrong["token"]})
    assert refusal.status_code == 403, refusal.text
    assert "authz" in refusal.text.lower() or "role" in refusal.text.lower(), refusal.text
    assert wrong["token"] not in refusal.text

    still_checked = client.get(f"/api/command/commands/{wrong['commandId']}/trail").json()["transitions"]
    assert still_checked[-1]["state"] == "COMMAND_STATE_CHECKED", "a refusal must never silently advance the state machine"

    success = client.post(f"/api/command/commands/{right['commandId']}/authorize", json={"principalToken": right["token"]})
    assert success.status_code == 200, success.text
    assert success.json()["state"] == "COMMAND_STATE_AUTHORIZED"
    assert right["token"] not in success.text

    trail = client.get(f"/api/command/commands/{right['commandId']}/trail").json()["transitions"]
    assert trail[-1]["state"] == "COMMAND_STATE_AUTHORIZED"


def test_authorize_token_is_sent_once_cleared_synchronously_and_never_resurfaces(command_panel_data):
    """The brief's own non-negotiable rule (question 201(b)): the token reaches
    `onAuthorize` exactly once, the input is cleared before the promise even settles, and
    it never appears in any later render's text -- success or refusal alike -- proven
    against the REAL refusal message this fixture's own wrong-role authorize attempt
    produced."""
    failed = _failed(command_panel_data, "token rule:") + _failed(command_panel_data, 'render: an authorize refusal')
    assert not failed, f"authorize token-safety checks failed: {failed}"


def test_command_panel_source_has_no_storage_cookie_or_other_state_change_control(command_panel_data):
    failed = _failed(command_panel_data, "command_panel.js source:")
    assert not failed, f"source-inspection checks failed: {failed}"


def test_execution_profile_default_layout_gains_the_command_panel_exactly_once(command_panel_data):
    """Part 1 + Part 3 joined at the one point that matters end to end: `isExecutionProfile`
    and `defaultLayoutTreeForScenario` (web/js/layout/default_layouts.js) are proven
    against every shape this task's own brief names -- ordinary/RPO/sweep -- for both an
    execution-profile scenario (gains the command panel exactly once, at the documented
    0.78/0.22 share) and every other profile (byte-identical leaf counts to before this
    task: 5/7/4, never regressed)."""
    failed = _failed(command_panel_data, "layout:") + _failed(command_panel_data, "isExecutionProfile:")
    assert not failed, f"layout checks failed: {failed}"


def test_command_panel_check_report(command_panel_data, capsys):
    with capsys.disabled():
        print(f"\ncommand_panel_check.mjs: {len(command_panel_data['checks'])} checks, allPass={command_panel_data['allPass']}")
        for c in command_panel_data["checks"]:
            mark = "PASS" if c["pass"] else "FAIL"
            print(f"  [{mark}] {c['name']}")
    assert command_panel_data["allPass"] is True, "command_panel_check.mjs reported at least one failing check -- see the printed table above (-s)"
