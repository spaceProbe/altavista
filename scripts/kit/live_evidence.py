"""scripts/kit/live_evidence.py -- D4 (docs/p5-plan.md), the LIVE half of the evidence bundle
(P5 track round 3, task D4b, commit 2). Companion to `scripts/kit/evidence.py` (the offline half,
D4a, task D4b's first commit) -- this module supplies the two plug-in points that module's own
`assemble_bundle` documents: `ledger_verify_live` and `secdeploy_evidence`. Library-first, same
shape as `evidence.py`/`sbom.py`/`manifest.py`: the functions below never print or `sys.exit`;
`main` (the CLI) decides, and even `main` never fabricates a result for a component it could not
reach -- see `NOT_COLLECTED_COMPONENTS`.

# The deployment question, settled with evidence, not opinion

D4's own text asks for "`secdeploy evidence` run over a deployed evaluation placement (the macos
target's compose, or our own compose from the kit if secdeploy's deploy cannot run our tiers yet;
say which and why)". Measured answer: **`secdeploy deploy macos` cannot stand up any AltaVista
component, so this module stands up our own compose from the kit instead.**

Read `/Users/probe/code/secdeploy/src/secdeploy/targets/macos.py::deploy` (starts ~line 829): every
step it builds is hand-written for a FIXED set of secdeploy-NATIVE component names --
`seccert`/`secrouter` via `docker compose up -d <name>`, `secdns`/`secllm`/`secagent`/
`secrecorder`/`secproxy` via bespoke launchd-plist generation, `secchat`/`secsso` as fixed
`[[stack]]` bootstraps. There is no generic per-manifest-component dispatch keyed by
`kind`/`runtime`/`tier` anywhere in that function -- an unrecognized component NAME (any of ours)
is simply never referenced. `cli.py::cmd_plan` (`secdeploy plan macos`) DOES list every AltaVista
component in our merged manifest -- easy to mistake for evidence that `deploy` also knows what to
do with them, but `cmd_plan` is a pure, generic manifest echo (`Manifest.select`/
`topology.components_on`, then a FIXED, hard-coded `mod.PLAN` prose list), entirely disconnected
from `deploy()`'s own hand-written steps.

Measured directly: running `secdeploy deploy macos --dry-run` over our own merged manifest
(`deploy/secdeploy/merge.py`'s own output, `deploy/secdeploy/secsite.altavista-eval.toml` as the
site) prints steps for exactly `secdns, seccert, secrouter, secrecorder, secproxy` (the eval
site's own single-host placement) plus the always-present `secchat`/`secsso` stack bootstraps --
not one of our eight AltaVista components (`av-ingest`, `av-command`, `av-gateway`, `av-proposer`,
`av-dynamics-service`, `gmat-service`, `av-edge-plugin`, `av-viewer`) appears anywhere in that
output. This module therefore never attempts a real, state-writing `secdeploy deploy` against the
user's own secdeploy checkout (forbidden regardless -- secdeploy is read-and-run only) and instead
brings up an evaluation placement we fully control, from the kit alone or from a plain native
process, exactly the charter's own sanctioned fallback.

# What this module brings up, and how cheaply -- see `collect_live_ledger_verify`

- **av-ingest-server** (D4b's own floor: "at minimum") -- docker, from the kit's own cross-built
  `binaries/av-ingest-server` and the kit's own `av-edge-plugin:local` image (loaded and its
  digest compared against what `KIT_MANIFEST` records BEFORE anything trusts it -- question 212),
  inside a freshly created, LABELLED, `--internal` bridge network. The same `lock_docker_tests`/
  `prune_stale_labelled_resources`/labelled-resource discipline
  `tests/test_kit_zero_egress_install.py` already establishes -- reused here by direct import of
  `altavista.container_hardening`/`altavista.docker_test_lock` (real, non-test library modules),
  never re-invented; `_docker`/network/container mechanics below restate that file's OWN
  conventions for this module's own, simpler topology (no install, no egress proof -- this module
  only needs the binary running and answering its own admin routes).
- **av-dynamics-service** -- a plain native subprocess (`target/debug/av-dynamics-service`, a
  Mach-O binary on THIS host already built -- no cross-build, no docker, no network at collection
  time), a real GMAT warm-up against `$GMAT_ROOT`, admin HTTP on loopback.
- **gmat-service** -- in-process: `gmat_service.admin.serve_admin` (already exists in this
  workspace, reused directly, not re-implemented) over a real `gmat_service.evidence.EvidenceLog`.

Every component this module could NOT bring up cheaply is recorded BY NAME, never silently
omitted -- see `NOT_COLLECTED_COMPONENTS`:

- **av-command** -- needs a real OIDC issuer/public key this task cannot supply (and this task's
  own rules forbid editing `crates/av-command` to add a test one); the same gap
  `tests/test_kit_zero_egress_install.py` already documents for this exact binary (it proves only
  the binary's own well-defined missing-OIDC-config refusal, never the full admin service).
- **av-edge-plugin** -- a one-shot CLI client (delivers a signed batch to av-ingest-server, then
  exits); it has no persistent admin HTTP server of its own to dial at all.
- **av-gateway** -- its one admin route is `GET /admin/api/evidence/bundle`
  (`crates/av-gateway/src/admin.rs`) -- a Bearer-token-gated AGGREGATOR of OTHER components' own
  evidence bundles, not a ledger `verify` endpoint, and needs a live catalogue/proposer connection
  this task's "cheaply" qualifier does not extend to standing up.

# Determinism (question 199/154's discipline, extended to live results)

Live responses are INPUTS to `scripts/kit/evidence.py::assemble_bundle`, recorded VERBATIM --
never re-fetched inside that module, and that module's own determinism proof
(`tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical`) still
holds when fed the SAME saved live-input dict twice (see
`tests/test_live_evidence.py::test_two_bundles_over_the_same_saved_live_input_are_byte_identical`).
`collect_live_ledger_verify`/`collect_secdeploy_evidence`, below, are the ONLY functions in this
whole D4b task that touch a network socket or spawn a process; every byte downstream of their
return values is a pure function of what they returned, exactly like `--kit`/`--ledger-dir`
already are for the offline half. The one field this module does NOT strip or normalise is
`fips.detail` in `av-dynamics-service`'s own `/admin/api/evidence` body -- it names this host's
OpenSSL install path/version, which is real, reproducible on THIS host, and not a wall-clock
value; it is recorded as-is (and would legitimately differ on a different host, which is exactly
what makes it evidence, not noise).
"""
from __future__ import annotations

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
KIT_SCRIPT_DIR = Path(__file__).resolve().parent
if str(KIT_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_SCRIPT_DIR))
import evidence  # noqa: E402  (path insert must precede this import)
import sbom  # noqa: E402

sys.path.insert(0, str(REPO_ROOT / "services" / "gmat-service"))
from gmat_service.admin import serve_admin  # noqa: E402
from gmat_service.evidence import EvidenceLog  # noqa: E402

from altavista.container_hardening import (  # noqa: E402
    TEST_LABEL_KEY,
    TEST_LABEL_VALUE,
    prune_stale_labelled_resources,
)
from altavista.docker_test_lock import lock_docker_tests  # noqa: E402

DEFAULT_KIT_DIR = REPO_ROOT / "out" / "kit" / "proof"
DEFAULT_DYNAMICS_BINARY = REPO_ROOT / "target" / "debug" / "av-dynamics-service"
GROUND_SEGMENT_FIXTURES = REPO_ROOT / "crates" / "av-edge" / "tests" / "fixtures" / "ground_segment"
VERIFY_PUB_PEM = GROUND_SEGMENT_FIXTURES.parent / "test_signing_key.pub.pem"
PROBE_IMAGE = "python:3.13-slim"
PRODUCER_ID = "demo-ground-segment-flight-plugin"
CLEARANCE_LADDER = "UNCLASSIFIED,CUI"
MAX_BATCH_AGE_NS = 10_000_000_000_000
CLOCK_TAI_NS = 1_767_225_637_000_000_000

#: D4b(c): components this module deliberately does NOT bring up, and exactly why -- named, never
#: omitted (see this module's own top doc for the full reasoning behind each).
NOT_COLLECTED_COMPONENTS: dict[str, str] = {
    "av-command": (
        "needs a real OIDC issuer/public key this task cannot supply, and this task's own rules "
        "forbid editing crates/av-command to add a test one -- the same gap "
        "tests/test_kit_zero_egress_install.py already documents for this exact cross-built "
        "binary (it proves only the binary's own well-defined missing-OIDC-config refusal, "
        "crates/av-command/src/bin/av-command.rs A2.1, never the full gRPC/admin service)."
    ),
    "av-edge-plugin": (
        "a one-shot CLI client (delivers a signed batch to av-ingest-server, then exits) -- it "
        "has no persistent admin HTTP server of its own to dial at all."
    ),
    "av-gateway": (
        "its one admin route is GET /admin/api/evidence/bundle (crates/av-gateway/src/admin.rs) "
        "-- a Bearer-token-gated AGGREGATOR of OTHER components' own evidence bundles, not a "
        "ledger `verify` endpoint, and needs a live catalogue/proposer connection this task's "
        "'cheaply' qualifier does not extend to standing up."
    ),
}


# =================================================================================================
# Small docker helpers -- the same shape tests/test_kit_zero_egress_install.py already
# establishes, restated here for this module's own, simpler topology. No process-environment
# mutation anywhere (question 199).
# =================================================================================================

def _docker(*args: str, timeout: float = 60.0, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        raise RuntimeError(
            f"`docker {' '.join(args)}` failed (rc={result.returncode}):\n"
            f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
        )
    return result


def _label_args(run_id: str) -> list[str]:
    return ["--label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--label", f"av.test.run_id={run_id}"]


def _assert_nothing_labelled_left(run_id: str) -> None:
    label_filter = f"label=av.test.run_id={run_id}"
    for kind, args in (
        ("container", ["ps", "-a", "--filter", label_filter, "-q"]),
        ("network", ["network", "ls", "--filter", label_filter, "-q"]),
        ("volume", ["volume", "ls", "--filter", label_filter, "-q"]),
    ):
        remaining = _docker(*args).stdout.strip()
        assert remaining == "", f"{kind}(s) labelled {label_filter} still exist after cleanup: {remaining!r}"


_EVIDENCE_FETCH_SCRIPT = (
    "import urllib.request, json\n"
    "data = json.loads(urllib.request.urlopen('http://127.0.0.1:50071/admin/api/evidence', timeout=5).read())\n"
    "print(json.dumps(data))\n"
)
_VERIFY_FETCH_SCRIPT = (
    "import urllib.request, json\n"
    "data = json.loads(urllib.request.urlopen('http://127.0.0.1:50071/admin/api/evidence/verify', timeout=5).read())\n"
    "print(json.dumps(data))\n"
)


def _fetch_json_via_container_network(run_id: str, target_container: str, script: str) -> dict:
    """Runs `script` inside a fresh, labelled `PROBE_IMAGE` container sharing `target_container`'s
    network namespace (`--network container:<target>`) -- the identical idiom
    `tests/test_kit_zero_egress_install.py::_run_edge_plugin_from_kit_image`'s own evidence probe
    uses, restated here so this module never needs to publish a port to the host at all."""
    probe_name = f"av-live-evidence-probe-{run_id}-{abs(hash(script)) % 100000}"
    try:
        raw = _docker(
            "run", "--rm", "--name", probe_name, "--network", f"container:{target_container}",
            *_label_args(run_id), PROBE_IMAGE, "python3", "-c", script,
        )
    finally:
        subprocess.run(["docker", "rm", "-f", probe_name], capture_output=True, timeout=30)
    return json.loads(raw.stdout.strip().splitlines()[-1])


def _wait_for_container_log_substring(name: str, substrings: tuple[str, ...], timeout_s: float) -> list[str]:
    deadline = time.monotonic() + timeout_s
    while True:
        result = _docker("logs", name, check=False)
        logs = result.stdout + result.stderr
        if all(s in logs for s in substrings):
            return logs.splitlines()
        if time.monotonic() > deadline:
            raise RuntimeError(f"container {name} never printed all of {substrings} within {timeout_s}s -- logs:\n{logs}")
        time.sleep(0.1)


def _load_and_verify_image(kit_dir: Path, component: str) -> dict:
    """Question 212: `docker load -i` the kit's own tarball for `component` and compare the
    loaded image id to the digest `KIT_MANIFEST` records for it -- trusted only after this
    comparison, both digests returned so the caller can quote them (question 148)."""
    manifest_doc = json.loads((kit_dir / "KIT_MANIFEST").read_text(encoding="utf-8"))
    entry = manifest_doc["images"][component]
    tarball = kit_dir / "images" / f"{component}.tar"
    if not tarball.is_file():
        raise RuntimeError(f"{component}: KIT_MANIFEST claims images.{component}.collected but {tarball} is missing")
    _docker("load", "-i", str(tarball), timeout=120)
    loaded = _docker("image", "inspect", entry["tag"], "--format", "{{.Id}}")
    actual_digest = loaded.stdout.strip()
    if actual_digest != entry["recorded_digest"]:
        raise RuntimeError(
            f"{component}: KIT_MANIFEST records {entry['recorded_digest']}, but the image loaded "
            f"from {tarball} has id {actual_digest} -- refusing to trust it (question 212)"
        )
    return {"tag": entry["tag"], "recorded_digest": entry["recorded_digest"], "loaded_digest": actual_digest}


# =================================================================================================
# av-ingest-server, from the kit alone, in docker
# =================================================================================================

def bring_up_av_ingest_server(kit_dir: Path, run_id: str) -> dict:
    """Brings up av-ingest-server from the kit's own cross-built binary and loaded image, fetches
    its real `/admin/api/evidence` and `/admin/api/evidence/verify`, tears everything down, and
    asserts nothing labelled by this run is left (question 156/207). Returns the
    `ledger_verify_live`-shaped entry for `av-ingest` PLUS the raw `/admin/api/evidence` body
    (returned separately -- callers decide whether/where to record it; `evidence.py`'s own bundle
    shape only ever wants the `verify` result at `ledger_verify['live']`)."""
    binaries_dir = kit_dir / "binaries" / "av-ingest-server"
    if not binaries_dir.is_file():
        return {
            "entry": {
                "status": "not_collected", "url": None, "result": None,
                "reason": f"{kit_dir} carries no binaries/av-ingest-server -- build the proof kit first",
            },
            "evidence": None,
        }

    prune_stale_labelled_resources()
    image_provenance = _load_and_verify_image(kit_dir, "edge-plugin-image")
    network_name = f"av-live-evidence-net-{run_id}"
    ingest_container = f"av-live-evidence-ingest-{run_id}"
    admin_url = "http://127.0.0.1:50071"
    try:
        _docker("network", "create", "--internal", *_label_args(run_id), network_name)
        _docker(
            "run", "-d", "--name", ingest_container, "--network", network_name, *_label_args(run_id),
            "--user", "0:0",  # the runtime base image bakes a non-root USER; --log-dir needs root
            "-v", f"{kit_dir}/binaries:/kit/binaries:ro",
            "-v", f"{VERIFY_PUB_PEM}:/keys/verify.pub.pem:ro",
            "--entrypoint", "/kit/binaries/av-ingest-server",
            image_provenance["tag"],
            "--grpc-bind", "127.0.0.1:50070", "--admin-bind", "127.0.0.1:50071",
            "--log-dir", "/data/ingest-log",
            "--no-require-client-cert", "--verify-key", f"{PRODUCER_ID}:/keys/verify.pub.pem",
            "--clearance-ladder", CLEARANCE_LADDER, "--max-batch-age-ns", str(MAX_BATCH_AGE_NS),
            "--clock-tai-ns", str(CLOCK_TAI_NS),
        )
        _wait_for_container_log_substring(ingest_container, ("GRPC_LISTENING", "ADMIN_LISTENING"), timeout_s=15.0)

        evidence_body = _fetch_json_via_container_network(run_id, ingest_container, _EVIDENCE_FETCH_SCRIPT)
        verify_body = _fetch_json_via_container_network(run_id, ingest_container, _VERIFY_FETCH_SCRIPT)
    finally:
        subprocess.run(["docker", "rm", "-f", ingest_container], capture_output=True, timeout=30)
        subprocess.run(["docker", "network", "rm", network_name], capture_output=True, timeout=30)
        _assert_nothing_labelled_left(run_id)

    return {
        "entry": {
            "status": "collected", "url": f"{admin_url}/admin/api/evidence/verify",
            "result": verify_body, "reason": None,
        },
        "evidence": {
            "image_provenance": image_provenance, "url": f"{admin_url}/admin/api/evidence",
            "result": evidence_body,
        },
    }


# =================================================================================================
# av-dynamics-service, a plain native subprocess (no docker, no cross-build)
# =================================================================================================

def bring_up_av_dynamics_service(evidence_path: Path, run_id: str, binary: Path = DEFAULT_DYNAMICS_BINARY) -> dict:
    if not binary.is_file():
        return {
            "entry": {
                "status": "not_collected", "url": None, "result": None,
                "reason": f"{binary} does not exist -- `cargo build -p av-dynamics-service` first",
            },
            "evidence": None,
        }
    admin_port = 51071
    grpc_port = 51070
    admin_url = f"http://127.0.0.1:{admin_port}"
    proc = subprocess.Popen(
        [str(binary), "--port", str(grpc_port), "--admin-port", str(admin_port),
         "--evidence-path", str(evidence_path), "--run-id", run_id],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    try:
        deadline = time.monotonic() + 30.0
        warmed = False
        while time.monotonic() < deadline:
            line = proc.stdout.readline()
            if not line:
                if proc.poll() is not None:
                    break
                continue
            if "warmed up" in line:
                warmed = True
                break
        if not warmed:
            return {
                "entry": {
                    "status": "error", "url": None, "result": None,
                    "reason": f"{binary} never printed its own 'warmed up' readiness line within 30s",
                },
                "evidence": None,
            }
        evidence_body = json.loads(urllib.request.urlopen(f"{admin_url}/admin/api/evidence", timeout=5).read())
        verify_body = json.loads(urllib.request.urlopen(f"{admin_url}/admin/api/evidence/verify", timeout=5).read())
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)
    return {
        "entry": {
            "status": "collected", "url": f"{admin_url}/admin/api/evidence/verify",
            "result": verify_body, "reason": None,
        },
        "evidence": {"url": f"{admin_url}/admin/api/evidence", "result": evidence_body},
    }


# =================================================================================================
# gmat-service admin, in-process (gmat_service.admin.serve_admin, reused directly)
# =================================================================================================

def bring_up_gmat_service_admin(evidence_path: Path, run_id: str, n_records: int = 4) -> dict:
    """Real `gmat_service.evidence.EvidenceLog`, `n_records` real chained entries, a real
    `gmat_service.admin.serve_admin` HTTP server over it (already exists in this workspace --
    reused, not re-implemented), fetched over real loopback HTTP, then shut down cleanly."""
    from google.protobuf.message import Message  # noqa: F401  (type-check only, already a dep)

    class _FakeMessage:
        def __init__(self, payload: str):
            self._payload = payload.encode("utf-8")

        def SerializeToString(self, deterministic: bool = False) -> bytes:  # noqa: N802
            return self._payload

    log = EvidenceLog(evidence_path)
    for _ in range(n_records):
        log.record(
            method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
            settings_hash="live_evidence_settings_hash", run_id=run_id,
        )
    httpd = serve_admin(host="127.0.0.1", port=0, evidence=log, run_id=run_id)
    admin_url = f"http://127.0.0.1:{httpd.server_address[1]}"
    try:
        evidence_body = json.loads(urllib.request.urlopen(f"{admin_url}/admin/api/evidence", timeout=5).read())
        verify_body = json.loads(urllib.request.urlopen(f"{admin_url}/admin/api/evidence/verify", timeout=5).read())
    finally:
        httpd.shutdown()
        httpd.server_close()
    return {
        "entry": {
            "status": "collected", "url": f"{admin_url}/admin/api/evidence/verify",
            "result": verify_body, "reason": None,
        },
        "evidence": {"url": f"{admin_url}/admin/api/evidence", "result": evidence_body},
        "log_path": str(evidence_path),
    }


def gmat_service_verify_over_tampered_ledger(evidence_path: Path, run_id: str, n_records: int = 4) -> dict:
    """D4b(f): the live tampered-ledger variant. Builds a real `EvidenceLog`, tampers record 3's
    content on disk (the identical mutation `tests/test_evidence_bundle.py::
    test_tampered_ledger_makes_verify_and_the_bundle_say_so` and
    `tests/test_compliance.py::test_verify_detects_a_tampered_record_and_reports_its_sequence_number`
    already use), starts a REAL `gmat_service.admin.serve_admin` HTTP server over the TAMPERED log,
    and fetches `/admin/api/evidence/verify` from it over real loopback HTTP -- proving the break
    is visible through the live HTTP surface, not just the bare `EvidenceLog.verify()` call."""

    class _FakeMessage:
        def __init__(self, payload: str):
            self._payload = payload.encode("utf-8")

        def SerializeToString(self, deterministic: bool = False) -> bytes:  # noqa: N802
            return self._payload

    log = EvidenceLog(evidence_path)
    for _ in range(n_records):
        log.record(
            method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
            settings_hash="live_evidence_tamper_test", run_id=run_id,
        )
    lines = log.path.read_text().splitlines()
    tampered = json.loads(lines[2])
    assert tampered["seq"] == 3
    tampered["method"] = "Propagate"
    lines[2] = json.dumps(tampered, sort_keys=True)
    log.path.write_text("\n".join(lines) + "\n")

    # A fresh EvidenceLog instance over the now-tampered file -- the admin server always verifies
    # against what's on disk, never a cached in-memory copy, exactly like a real restart would.
    tampered_log = EvidenceLog(evidence_path)
    httpd = serve_admin(host="127.0.0.1", port=0, evidence=tampered_log, run_id=run_id)
    admin_url = f"http://127.0.0.1:{httpd.server_address[1]}"
    try:
        verify_body = json.loads(urllib.request.urlopen(f"{admin_url}/admin/api/evidence/verify", timeout=5).read())
    finally:
        httpd.shutdown()
        httpd.server_close()
    return {"url": f"{admin_url}/admin/api/evidence/verify", "result": verify_body}


# =================================================================================================
# secdeploy evidence / secdeploy audit verify -- real invocations over our own merged manifest
# =================================================================================================

def collect_secdeploy_evidence(
    secdeploy_dir: Path, merged_manifest: Path, merged_site: Path, out_dir: Path,
) -> dict:
    """Runs the real `secdeploy evidence`/`secdeploy audit verify` CLIs (via `uv run --offline
    --project <secdeploy_dir> secdeploy ...`, exactly `tests/test_suite_declarations.py::_uv_run`'s
    own documented invocation) against OUR merged manifest, with `--out`/`--work` pointed inside
    `out_dir` (never secdeploy's own checkout -- secdeploy is read-and-run only). Returns the
    `secdeploy_evidence`-shaped dict `evidence.SECDEPLOY_EVIDENCE_NOT_COLLECTED`'s own `"plug_in"`
    field describes."""
    out_dir.mkdir(parents=True, exist_ok=True)
    work_dir = out_dir / "work"
    work_dir.mkdir(parents=True, exist_ok=True)

    evidence_run = subprocess.run(
        ["uv", "run", "--offline", "--project", str(secdeploy_dir), "secdeploy",
         "--manifest", str(merged_manifest), "--out", str(out_dir), "--work", str(work_dir),
         "evidence", "--site", str(merged_site)],
        capture_output=True, text=True, timeout=180,
    )
    written = out_dir / "evidence"
    bundle_files = sorted(written.glob("suite-evidence-*.json")) if written.is_dir() else []
    evidence_bundle = json.loads(bundle_files[-1].read_text()) if bundle_files else None

    audit_run = subprocess.run(
        ["uv", "run", "--offline", "--project", str(secdeploy_dir), "secdeploy",
         "--manifest", str(merged_manifest), "--out", str(out_dir), "--work", str(work_dir),
         "audit", "verify"],
        capture_output=True, text=True, timeout=60,
    )

    return {
        "evidence": evidence_bundle,
        "evidence_cli": {
            "returncode": evidence_run.returncode,
            "stdout": evidence_run.stdout,
            "stderr": evidence_run.stderr,
        },
        "audit_verify_cli": {
            "returncode": audit_run.returncode,
            "stdout": audit_run.stdout,
            "stderr": audit_run.stderr,
        },
    }


# =================================================================================================
# Orchestration
# =================================================================================================

def collect_live_ledger_verify(
    kit_dir: Path = DEFAULT_KIT_DIR,
    dynamics_binary: Path = DEFAULT_DYNAMICS_BINARY,
    scratch_dir: Optional[Path] = None,
) -> dict:
    """Brings up every cheaply-reachable component, fetches real `/admin/api/evidence/verify`
    from each, tears everything down, and returns the `ledger_verify_live`-shaped dict
    `scripts/kit/evidence.py::LIVE_LEDGER_VERIFY_NOT_COLLECTED`'s own `"plug_in"` field describes.
    Components this module could not bring up cheaply are named via `NOT_COLLECTED_COMPONENTS`,
    never omitted. `scratch_dir` holds the EvidenceLog files this run writes (gmat-service's,
    av-dynamics-service's) -- defaults to a fresh dir under `REPO_ROOT/.av-test-tmp`."""
    import uuid

    run_id = uuid.uuid4().hex[:12]
    scratch_dir = scratch_dir or (REPO_ROOT / ".av-test-tmp" / "live_evidence" / run_id)
    scratch_dir.mkdir(parents=True, exist_ok=True)

    result: dict = {}
    raw_evidence: dict = {}

    for name, reason in NOT_COLLECTED_COMPONENTS.items():
        result[name] = {"status": "not_collected", "url": None, "result": None, "reason": reason}

    dyn = bring_up_av_dynamics_service(scratch_dir / "av-dynamics-service-evidence.jsonl", run_id, dynamics_binary)
    result["av-dynamics-service"] = dyn["entry"]
    if dyn["evidence"] is not None:
        raw_evidence["av-dynamics-service"] = dyn["evidence"]

    gmat = bring_up_gmat_service_admin(scratch_dir / "gmat-service-evidence.jsonl", run_id)
    result["gmat-service"] = gmat["entry"]
    raw_evidence["gmat-service"] = gmat["evidence"]

    with lock_docker_tests():
        ingest = bring_up_av_ingest_server(kit_dir, run_id)
    result["av-ingest"] = ingest["entry"]
    if ingest["evidence"] is not None:
        raw_evidence["av-ingest"] = ingest["evidence"]

    return {"ledger_verify_live": result, "raw_evidence": raw_evidence, "run_id": run_id}


def main(argv: Optional[list] = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="output directory for the live bundle + captured raw responses")
    parser.add_argument("--kit", default=str(DEFAULT_KIT_DIR), help="the proof kit directory (default: out/kit/proof)")
    parser.add_argument(
        "--secdeploy-dir", default="/Users/probe/code/secdeploy", help="the secdeploy checkout (read/run only)",
    )
    parser.add_argument(
        "--merged-manifest", default=None,
        help="an already-merged suite.merged.toml (deploy/secdeploy/merge.py's own output); "
             "defaults to merging deploy/secdeploy/suite.altavista.toml fresh under --out",
    )
    parser.add_argument("--merged-site", default=None, help="the matching secsite.merged.toml")
    parser.add_argument("--skip-secdeploy", action="store_true", help="skip the secdeploy evidence/audit verify calls")
    args = parser.parse_args(argv)

    out_dir = Path(args.out)
    if not out_dir.is_absolute():
        out_dir = REPO_ROOT / out_dir
    out_dir.mkdir(parents=True, exist_ok=True)
    kit_dir = Path(args.kit)

    live = collect_live_ledger_verify(kit_dir=kit_dir, scratch_dir=out_dir / "scratch")
    (out_dir / "raw_live_evidence.json").write_text(
        json.dumps(live["raw_evidence"], indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )
    (out_dir / "ledger_verify_live.json").write_text(
        json.dumps(live["ledger_verify_live"], indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )

    secdeploy_evidence = None
    if not args.skip_secdeploy:
        merge_dir = out_dir / "secdeploy_merged"
        if args.merged_manifest and args.merged_site:
            merged_manifest = Path(args.merged_manifest)
            merged_site = Path(args.merged_site)
        else:
            sys.path.insert(0, str(REPO_ROOT / "deploy" / "secdeploy"))
            import merge as merge_mod  # noqa: E402

            merged_manifest = merge_mod.merge(
                base=Path(args.secdeploy_dir) / "suite.toml",
                fragment=REPO_ROOT / "deploy" / "secdeploy" / "suite.altavista.toml",
                site=REPO_ROOT / "deploy" / "secdeploy" / "secsite.altavista-eval.toml",
                out=merge_dir,
            )
            merged_site = merge_dir / "secsite.merged.toml"
        secdeploy_result = collect_secdeploy_evidence(
            Path(args.secdeploy_dir), merged_manifest, merged_site, out_dir / "secdeploy_out",
        )
        (out_dir / "secdeploy_evidence_raw.json").write_text(
            json.dumps(secdeploy_result, indent=2, sort_keys=True) + "\n", encoding="utf-8",
        )
        secdeploy_evidence = {
            "evidence": secdeploy_result["evidence"],
            "audit_verify": secdeploy_result["audit_verify_cli"],
        }

    bundle = evidence.assemble_bundle(
        repo_root=REPO_ROOT, kit_dir=kit_dir, ledger_verify_live=live["ledger_verify_live"],
        secdeploy_evidence=secdeploy_evidence,
    )
    bundle_path = evidence.write_bundle(bundle, out_dir)
    print(f"wrote {bundle_path}", file=sys.stderr)
    print(f"bundle_sha256: {bundle['bundle_sha256']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
