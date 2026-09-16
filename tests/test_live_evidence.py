"""AltaVista P5 track, round 3, task D4b: tests for `scripts/kit/live_evidence.py`, the LIVE
half of the evidence bundle (companion to `tests/test_evidence_bundle.py`, which covers the
offline half, `scripts/kit/evidence.py`, D4a).

What this file proves:

1. `secdeploy deploy macos` really does not touch any AltaVista component (the measured answer
   to D4's own deployment question) -- `test_deploy_macos_dry_run_never_mentions_an_altavista_component`.
2. Every component `live_evidence.py` brings up cheaply returns a REAL `/admin/api/evidence/verify`
   response shaped `{"ok", "checked", "broken_at_seq"/"broken_at_sequence", "detail"}` (or the
   per-shard map `av-ingest`'s own admin returns): `av-dynamics-service` (a native subprocess, no
   docker) and `gmat-service` (in-process, `gmat_service.admin.serve_admin` reused directly) are
   gated only on their own cheap prerequisites; `av-ingest` (docker, from the kit) is gated the
   same way `tests/test_kit_zero_egress_install.py` already gates its own docker-based proof.
3. The live tampered-ledger variant (D4b(f)): a tampered `EvidenceLog` served by a REAL
   `gmat_service.admin.serve_admin` HTTP server reports the break over real loopback HTTP.
4. Every component `live_evidence.py` could NOT bring up cheaply is named, with a reason, never
   silently omitted (`NOT_COLLECTED_COMPONENTS`).
5. `secdeploy evidence`'s real output and `secdeploy audit verify`'s real result both plug into
   `scripts/kit/evidence.py::assemble_bundle`'s `secdeploy_evidence` parameter verbatim, the same
   way `ledger_verify_live` already does (`tests/test_evidence_bundle.py`'s own coverage of that
   parameter is not repeated here).
6. Determinism (D4b(e)): two `assemble_bundle` calls fed the SAME saved live-input dict produce
   byte-identical bytes, exactly `tests/test_evidence_bundle.py::
   test_two_runs_over_the_same_state_are_byte_identical`'s own proof, extended to live inputs.

No test mutates `os.environ` (question 199). No test writes anywhere but `tmp_path`/its own
labelled docker resources (cleaned up and asserted gone, same discipline as
`tests/test_kit_zero_egress_install.py`). No test needs network access at test time (question
154) except the two gated real `secdeploy`/docker proofs, which are exactly the two places this
whole task's own charter grants a live-collection exception -- everything else here drives
already-running, already-local processes over loopback.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))
import evidence  # noqa: E402
import live_evidence  # noqa: E402

SECDEPLOY_DIR = Path("/Users/probe/code/secdeploy")
BASE_MANIFEST = SECDEPLOY_DIR / "suite.toml"
FRAGMENT_PATH = REPO_ROOT / "deploy" / "secdeploy" / "suite.altavista.toml"
EVAL_SITE = REPO_ROOT / "deploy" / "secdeploy" / "secsite.altavista-eval.toml"

ALTAVISTA_COMPONENTS = [
    "av-ingest", "av-command", "av-gateway", "av-proposer",
    "av-dynamics-service", "gmat-service", "av-edge-plugin", "av-viewer",
]


# =================================================================================================
# Gating -- each computed once, at import time, a typed reason asserted by name (question 194).
# =================================================================================================

def _secdeploy_skip_reason() -> "str | None":
    missing = []
    if shutil.which("uv") is None:
        missing.append("`uv` is not on PATH")
    if not BASE_MANIFEST.exists():
        missing.append(f"{BASE_MANIFEST} does not exist (no secdeploy checkout)")
    if missing:
        return "secdeploy round-trip tests need " + " and ".join(missing)
    return None


_SECDEPLOY_SKIP_REASON = _secdeploy_skip_reason()
requires_secdeploy = pytest.mark.skipif(_SECDEPLOY_SKIP_REASON is not None, reason=_SECDEPLOY_SKIP_REASON or "")


def _dynamics_binary_skip_reason() -> "str | None":
    if not live_evidence.DEFAULT_DYNAMICS_BINARY.is_file():
        return (
            f"{live_evidence.DEFAULT_DYNAMICS_BINARY} does not exist -- build it first with "
            f"`cargo build -p av-dynamics-service`"
        )
    return None


_DYNAMICS_SKIP_REASON = _dynamics_binary_skip_reason()
requires_dynamics_binary = pytest.mark.skipif(_DYNAMICS_SKIP_REASON is not None, reason=_DYNAMICS_SKIP_REASON or "")


def _docker_unavailable_reason() -> "str | None":
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"docker is not installed or could not be launched: {e}"
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip().splitlines()
        return f"`docker info` exited {result.returncode}: {detail[-1] if detail else '(no stderr)'}"
    return None


def _kit_skip_reason() -> "str | None":
    manifest_path = live_evidence.DEFAULT_KIT_DIR / "KIT_MANIFEST"
    if not manifest_path.is_file():
        return (
            f"the proof kit has not been built at {live_evidence.DEFAULT_KIT_DIR} -- see "
            f"tests/test_kit_zero_egress_install.py's own _PROOF_KIT_BUILD_CMD"
        )
    doc = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not doc.get("binaries", {}).get("results", {}).get("av-ingest-server", {}).get("included"):
        return f"{live_evidence.DEFAULT_KIT_DIR} carries no binaries.av-ingest-server"
    return None


def _docker_ingest_skip_reason() -> "str | None":
    return _docker_unavailable_reason() or _kit_skip_reason()


_DOCKER_INGEST_SKIP_REASON = _docker_ingest_skip_reason()
requires_docker_ingest = pytest.mark.skipif(
    _DOCKER_INGEST_SKIP_REASON is not None, reason=_DOCKER_INGEST_SKIP_REASON or ""
)


# =================================================================================================
# 1. The deployment question, measured (D4's own text)
# =================================================================================================

@requires_secdeploy
def test_deploy_macos_dry_run_never_mentions_an_altavista_component(tmp_path):
    """The measured proof behind this module's own top-doc claim: `secdeploy deploy macos
    --dry-run`, run for real over OUR merged manifest, prints steps for secdeploy's own native
    components only -- never one of ours, even though `plan macos` (a separate, generic manifest
    echo -- see `tests/test_suite_declarations.py::
    test_secdeploy_plan_macos_lists_every_altavista_component`) lists every one of them. This is
    why `live_evidence.py` stands up our own compose from the kit instead of a real
    `secdeploy deploy`."""
    sys.path.insert(0, str(REPO_ROOT / "deploy" / "secdeploy"))
    import merge as merge_mod

    out = tmp_path / "out"
    merged = merge_mod.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out)
    site_copy = out / "secsite.merged.toml"
    work_dir = tmp_path / "work"
    result_dir = tmp_path / "result"

    result = subprocess.run(
        ["uv", "run", "--offline", "--project", str(SECDEPLOY_DIR), "secdeploy",
         "--manifest", str(merged), "--out", str(result_dir), "--work", str(work_dir),
         "deploy", "macos", "--dry-run", "--site", str(site_copy)],
        capture_output=True, text=True, timeout=180,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    for name in ALTAVISTA_COMPONENTS:
        assert name not in result.stdout, (
            f"secdeploy deploy macos --dry-run unexpectedly mentioned {name!r} -- if this now "
            f"fails, secdeploy has grown a generic per-manifest-component deploy dispatch and "
            f"this module's own top-doc claim (and D4's deployment answer) needs revisiting"
        )
    # Sanity: it DOES mention secdeploy's own native components -- proves this is a real,
    # non-empty dry-run output, not an accidentally-empty one that would trivially pass the
    # assertion above.
    assert "secrouter" in result.stdout or "seccert" in result.stdout


@requires_secdeploy
def test_plan_macos_lists_every_altavista_component_but_deploy_does_not_know_them(tmp_path):
    """The two behaviours side by side, in one test, so the contrast (display vs. actual deploy
    mechanics) is provable in a single run rather than inferred across two files."""
    sys.path.insert(0, str(REPO_ROOT / "deploy" / "secdeploy"))
    import merge as merge_mod

    out = tmp_path / "out"
    merged = merge_mod.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out)
    site_copy = out / "secsite.merged.toml"

    def _uv_run(*args):
        return subprocess.run(
            ["uv", "run", "--offline", "--project", str(SECDEPLOY_DIR), "secdeploy", *args],
            capture_output=True, text=True, timeout=180,
        )

    plan = _uv_run("--manifest", str(merged), "plan", "macos", "--site", str(site_copy))
    assert plan.returncode == 0, plan.stdout + plan.stderr
    for name in ALTAVISTA_COMPONENTS:
        assert name in plan.stdout, f"{name} missing from plan output"

    deploy = _uv_run(
        "--manifest", str(merged), "--out", str(out / "deployresult"), "--work", str(out / "work"),
        "deploy", "macos", "--dry-run", "--site", str(site_copy),
    )
    assert deploy.returncode == 0, deploy.stdout + deploy.stderr
    for name in ALTAVISTA_COMPONENTS:
        assert name not in deploy.stdout, f"deploy --dry-run unexpectedly mentioned {name!r}"


# =================================================================================================
# 2. Components brought up cheaply, real verify results
# =================================================================================================

@requires_dynamics_binary
def test_av_dynamics_service_returns_a_real_verify_result(tmp_path):
    result = live_evidence.bring_up_av_dynamics_service(tmp_path / "evidence.jsonl", "test_run")
    entry = result["entry"]
    assert entry["status"] == "collected", entry
    assert entry["url"].endswith("/admin/api/evidence/verify")
    assert entry["result"] == {"ok": True, "checked": 0, "broken_at_seq": None, "detail": "chain intact"}
    assert result["evidence"]["result"]["run_id"] == "test_run"
    assert "fips" in result["evidence"]["result"]


def test_gmat_service_admin_returns_a_real_verify_result(tmp_path):
    result = live_evidence.bring_up_gmat_service_admin(tmp_path / "evidence.jsonl", "test_run", n_records=3)
    entry = result["entry"]
    assert entry["status"] == "collected", entry
    assert entry["result"] == {"ok": True, "checked": 3, "broken_at_seq": None, "detail": "chain intact"}
    assert result["evidence"]["result"]["entries"] == 3
    assert result["evidence"]["result"]["run_id"] == "test_run"


def test_gmat_service_verify_over_tampered_ledger_reports_the_break_live(tmp_path):
    """D4b(f): the live tampered-ledger variant. A REAL `gmat_service.admin.serve_admin` HTTP
    server, over a REAL tampered `EvidenceLog`, reports the break through
    `/admin/api/evidence/verify` -- fetched over real loopback HTTP, not the bare
    `EvidenceLog.verify()` call alone (that bare-call proof already exists,
    `tests/test_compliance.py::test_verify_detects_a_tampered_record_and_reports_its_sequence_number`;
    this is its live-HTTP-surface twin, restated through `live_evidence.py`'s own collector so the
    SAME function this task's bundle uses is what is being proven)."""
    result = live_evidence.gmat_service_verify_over_tampered_ledger(tmp_path / "evidence.jsonl", "test_run")
    assert result["url"].endswith("/admin/api/evidence/verify")
    assert result["result"]["ok"] is False
    assert result["result"]["broken_at_seq"] == 3
    assert "tampered" in result["result"]["detail"]


@requires_docker_ingest
def test_av_ingest_server_returns_a_real_verify_result_from_the_kit():
    """The docker-gated `av-ingest` bring-up -- the one D4b names as the floor ('at minimum').
    Also asserts the host is left exactly as found (question 156/207): `bring_up_av_ingest_server`
    already asserts nothing labelled by its own run_id remains; this test additionally re-checks
    with the repo-wide label alone, so a leak this run's OWN run_id filter might miss (a label
    typo, say) is still caught."""
    import uuid

    run_id = uuid.uuid4().hex[:12]
    from altavista.container_hardening import prune_stale_labelled_resources

    prune_stale_labelled_resources()
    from altavista.docker_test_lock import lock_docker_tests

    with lock_docker_tests():
        result = live_evidence.bring_up_av_ingest_server(live_evidence.DEFAULT_KIT_DIR, run_id)
    entry = result["entry"]
    assert entry["status"] == "collected", entry
    assert entry["url"].endswith("/admin/api/evidence/verify")
    # A freshly-started av-ingest-server with no batches delivered reports an empty per-shard map
    # -- a real, correctly-shaped response, not a placeholder (crates/av-ingest/src/admin.rs's own
    # verify_body returns {shard_key: {...}} per shard that has EVER received a batch).
    assert entry["result"] == {}
    assert result["evidence"]["image_provenance"]["loaded_digest"] == result["evidence"]["image_provenance"]["recorded_digest"]

    remaining = subprocess.run(
        ["docker", "ps", "-a", "--filter", "label=av.test=1", "--filter", f"label=av.test.run_id={run_id}", "-q"],
        capture_output=True, text=True, timeout=30,
    ).stdout.strip()
    assert remaining == "", f"container(s) from run {run_id} still exist: {remaining!r}"


# =================================================================================================
# 3. Nothing brought up cheaply is silently omitted
# =================================================================================================

def test_not_collected_components_are_named_with_a_reason():
    assert set(live_evidence.NOT_COLLECTED_COMPONENTS) == {"av-command", "av-edge-plugin", "av-gateway"}
    for name, reason in live_evidence.NOT_COLLECTED_COMPONENTS.items():
        assert isinstance(reason, str) and len(reason) > 20, f"{name}: reason too thin to be a real explanation"


def test_the_six_control_matrix_components_are_all_accounted_for_in_the_live_section():
    """Every component `scripts/kit/evidence.py::discover_components` finds a control matrix for
    appears SOMEWHERE in `live_evidence.py`'s own accounting -- either brought up for real
    (`av-dynamics-service`, `gmat-service`, `av-ingest`) or named as not-collected
    (`av-command`, `av-edge-plugin`, `av-gateway`) -- never silently absent from both."""
    six = set(evidence.discover_components(evidence.REPO_ROOT))
    brought_up = {"av-dynamics-service", "gmat-service", "av-ingest"}
    accounted_for = brought_up | set(live_evidence.NOT_COLLECTED_COMPONENTS)
    assert six == accounted_for, (six, accounted_for)


# =================================================================================================
# 4. secdeploy evidence / audit verify plug into assemble_bundle's secdeploy_evidence verbatim
# =================================================================================================

def test_secdeploy_evidence_default_is_a_declared_not_collected_slot():
    bundle = evidence.assemble_bundle(repo_root=evidence.REPO_ROOT)
    section = bundle["secdeploy_evidence"]
    assert section["status"] == "not_collected"
    assert section["reason"]
    assert "plug_in" in section and "assemble_bundle" in section["plug_in"]


def test_secdeploy_evidence_parameter_is_stored_verbatim():
    fake = {"evidence": {"product": "secdeploy suite evidence"}, "audit_verify": {"returncode": 0}}
    bundle = evidence.assemble_bundle(repo_root=evidence.REPO_ROOT, secdeploy_evidence=fake)
    assert bundle["secdeploy_evidence"] == fake


@requires_secdeploy
def test_collect_secdeploy_evidence_real_invocation(tmp_path):
    """Runs the REAL `secdeploy evidence`/`secdeploy audit verify` CLIs over our own merged
    manifest (via `live_evidence.collect_secdeploy_evidence`) and asserts the shape D4(c) itself
    predicts: `secdeploy evidence`'s own five-name `COMPONENTS` constant
    (docs/secdeploy-upstream.md Proposal 2) can never name an AltaVista component, so every one of
    its five components reports `skipped`/`not_in_topology` -- never `ok` (nothing we run answers
    to those names) -- plus a real, structurally valid `deploy_audit_chain` result."""
    sys.path.insert(0, str(REPO_ROOT / "deploy" / "secdeploy"))
    import merge as merge_mod

    merge_dir = tmp_path / "merged"
    merged = merge_mod.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=merge_dir)
    site_copy = merge_dir / "secsite.merged.toml"

    result = live_evidence.collect_secdeploy_evidence(
        SECDEPLOY_DIR, merged, site_copy, tmp_path / "secdeploy_out",
    )
    assert result["evidence_cli"]["returncode"] == 0, result["evidence_cli"]["stderr"]
    assert result["audit_verify_cli"]["returncode"] == 0, result["audit_verify_cli"]["stderr"]

    bundle = result["evidence"]
    assert bundle["product"] == "secdeploy suite evidence"
    assert set(bundle["components"]) == {"secrouter", "seccert", "secllm", "secchat", "secrecorder"}
    for name, info in bundle["components"].items():
        assert info["status"] in ("skipped", "not_in_topology", "error"), (
            f"{name}: status={info['status']!r} -- secdeploy evidence's own COMPONENTS constant "
            f"cannot name an AltaVista component, so 'ok' here would be a surprise worth "
            f"investigating, not the expected result"
        )
    assert isinstance(bundle["deploy_audit_chain"]["ok"], bool)
    assert isinstance(bundle["deploy_audit_chain"]["checked"], int)

    # Feeds cleanly into assemble_bundle's own plug-in point.
    secdeploy_evidence = {"evidence": bundle, "audit_verify": result["audit_verify_cli"]}
    assembled = evidence.assemble_bundle(repo_root=evidence.REPO_ROOT, secdeploy_evidence=secdeploy_evidence)
    assert assembled["secdeploy_evidence"] == secdeploy_evidence


# =================================================================================================
# 5. Determinism with live inputs (D4b(e)): the SAME saved live input -> byte-identical bundles
# =================================================================================================

def test_two_bundles_over_the_same_saved_live_input_are_byte_identical(tmp_path):
    """Live responses are INPUTS, not live re-fetches, inside `assemble_bundle` -- feed the exact
    same recorded dict (as `tests/test_kit_zero_egress_install.py`-style fixtures would save it)
    to two separate `assemble_bundle` calls and assert byte-identical output, extending
    `tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical` to the
    live half."""
    saved_ledger_verify_live = {
        "av-command": {"status": "not_collected", "url": None, "result": None, "reason": "needs OIDC config"},
        "av-dynamics-service": {
            "status": "collected", "url": "http://127.0.0.1:51071/admin/api/evidence/verify",
            "result": {"ok": True, "checked": 0, "broken_at_seq": None, "detail": "chain intact"},
            "reason": None,
        },
    }
    saved_secdeploy_evidence = {
        "evidence": {"product": "secdeploy suite evidence", "generated_at": "2026-09-15", "components": {}},
        "audit_verify": {"returncode": 0, "stdout": "overall: 0 checked - OK\n", "stderr": ""},
    }

    bundle_a = evidence.assemble_bundle(
        repo_root=evidence.REPO_ROOT, ledger_verify_live=saved_ledger_verify_live,
        secdeploy_evidence=saved_secdeploy_evidence,
    )
    bundle_b = evidence.assemble_bundle(
        repo_root=evidence.REPO_ROOT, ledger_verify_live=saved_ledger_verify_live,
        secdeploy_evidence=saved_secdeploy_evidence,
    )
    path_a = evidence.write_bundle(bundle_a, tmp_path / "run_a")
    path_b = evidence.write_bundle(bundle_b, tmp_path / "run_b")
    assert path_a.read_bytes() == path_b.read_bytes()
    assert bundle_a["bundle_sha256"] == bundle_b["bundle_sha256"]
    assert bundle_a["ledger_verify"]["live"] == saved_ledger_verify_live
    assert bundle_a["secdeploy_evidence"] == saved_secdeploy_evidence
