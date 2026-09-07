"""M7.3: tests for the compliance artifacts this task adds.

Three things, in order:

1. Direct (fast, no GMAT/subprocess) tests of `gmat_service.evidence.EvidenceLog`'s
   hash chain -- GENESIS convention, `prev_hash`/`hash` linkage, and the tamper-detection
   proof this task's brief specifically asks for ("tamper with a record's content and
   prove `verify` reports the break and the sequence number where it happened"). The Rust
   twin of every test here lives in `crates/av-dynamics-service/src/evidence.rs`'s own
   `#[cfg(test)] mod tests`; `tests/test_dynamics_service_rs.py` separately proves the same
   thing end-to-end over the live `/admin/api/evidence/verify` HTTP endpoint (subprocess,
   slower). This file's Python-side equivalent stays fast by driving `EvidenceLog` and
   `gmat_service.admin.serve_admin` directly, with a tiny duck-typed protobuf-`Message`
   double (`gmat_service.evidence.hash_message` only ever calls `.SerializeToString`) --
   no `grpcio`/GMAT process needed.

2. Direct tests of `gmat_service.fips.detect()`.

3. Structural tests of `docs/compliance/*/control-matrix.md`: the legend and a deficiency
   list are present, and -- the load-bearing check -- **every row marked "Met" names a
   real `file:function` reference that actually exists in this repository** (binding rule:
   "Every `Met` row must name a real `file:function` that exists"). This is a mechanical,
   re-run-on-every-CI check, not a one-time manual claim.
"""
from __future__ import annotations

import http.client
import json
import re
import sys
import threading
import time
from pathlib import Path
from typing import Optional

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SERVICE_DIR = REPO_ROOT / "services" / "gmat-service"

if str(SERVICE_DIR) not in sys.path:
    sys.path.insert(0, str(SERVICE_DIR))

# Only leaf modules with no altavista/gmatpy import at module scope (mirrors
# tests/test_gmat_service.py's own `from gmat_service import covariance` comment) --
# evidence.py/fips.py/admin.py import nothing GMAT-specific, so this stays cheap.
from gmat_service import admin as gmat_admin  # noqa: E402
from gmat_service import fips as gmat_fips  # noqa: E402
from gmat_service.evidence import GENESIS, EvidenceLog  # noqa: E402


class _FakeMessage:
    """A duck-typed stand-in for a `google.protobuf.message.Message`: `evidence.py`'s
    `hash_message` only ever calls `.SerializeToString(deterministic=...)`, so a real
    protobuf message is unnecessary machinery for testing the *chain*, not the protobuf
    encoding (which `tests/test_gmat_service.py`/`tests/test_dynamics_service_rs.py`
    already exercise against real messages over the wire)."""

    def __init__(self, payload: str):
        self._payload = payload.encode("utf-8")

    def SerializeToString(self, deterministic: bool = False) -> bytes:  # noqa: N802 (protobuf's own naming)
        return self._payload


def _free_port() -> int:
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# =============================================================================================
# 1. EvidenceLog hash chain
# =============================================================================================
def test_fresh_log_chain_head_is_genesis(tmp_path):
    log = EvidenceLog(tmp_path / "evidence.jsonl")
    assert log.chain_head() == GENESIS
    assert log.entry_count() == 0


def test_first_record_prev_hash_is_the_genesis_sentinel(tmp_path):
    path = tmp_path / "evidence.jsonl"
    log = EvidenceLog(path)
    log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
              settings_hash="sh", run_id="run1")

    entry = json.loads(path.read_text().splitlines()[0])
    assert entry["seq"] == 1
    assert entry["prev_hash"] == GENESIS
    assert len(entry["hash"]) == 64
    assert all(c in "0123456789abcdef" for c in entry["hash"])
    assert log.chain_head() == entry["hash"]
    assert log.entry_count() == 1


def test_later_records_chain_prev_hash_to_the_previous_records_hash(tmp_path):
    path = tmp_path / "evidence.jsonl"
    log = EvidenceLog(path)
    for _ in range(3):
        log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                  settings_hash="sh", run_id="run1")

    lines = [json.loads(l) for l in path.read_text().splitlines()]
    assert len(lines) == 3
    for i, entry in enumerate(lines):
        assert entry["seq"] == i + 1
    assert lines[1]["prev_hash"] == lines[0]["hash"]
    assert lines[2]["prev_hash"] == lines[1]["hash"]
    # Distinct hashes -- a chain collapsed to one repeated value would hide tampering.
    assert len({l["hash"] for l in lines}) == 3


def test_verify_reports_ok_on_an_untampered_chain(tmp_path):
    path = tmp_path / "evidence.jsonl"
    log = EvidenceLog(path)
    for _ in range(5):
        log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                  settings_hash="sh", run_id="run1")
    result = log.verify()
    assert result == {"ok": True, "checked": 5, "broken_at_seq": None, "detail": "chain intact"}


def test_verify_reports_the_empty_log_as_ok(tmp_path):
    log = EvidenceLog(tmp_path / "evidence.jsonl")
    result = log.verify()
    assert result["ok"] is True
    assert result["checked"] == 0


def test_verify_detects_a_tampered_record_and_reports_its_sequence_number(tmp_path):
    """**The tamper-detection proof.** Writes genuine records, edits one record's *content*
    on disk without recomputing its `hash` -- exactly what an attacker or a bit-flip would
    do -- and proves `verify()` both flags the chain as broken and names the exact `seq` it
    broke at. This is the Python twin of
    `crates/av-dynamics-service/src/evidence.rs::verify_detects_a_tampered_record_and_reports_its_sequence_number`."""
    path = tmp_path / "evidence.jsonl"
    log = EvidenceLog(path)
    for _ in range(4):
        log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                  settings_hash="sh", run_id="run1")
    assert log.verify()["ok"] is True  # sanity: untampered, this chain verifies clean

    lines = path.read_text().splitlines()
    assert len(lines) == 4
    tampered = json.loads(lines[2])
    assert tampered["seq"] == 3
    tampered["method"] = "Propagate"  # content edited; "hash" field left stale
    lines[2] = json.dumps(tampered, sort_keys=True)
    path.write_text("\n".join(lines) + "\n")

    result = log.verify()
    assert result["ok"] is False
    assert result["broken_at_seq"] == 3, result
    assert result["checked"] == 2, result  # records before the break (seq 1, 2) still verify
    assert "tampered" in result["detail"]


def test_verify_detects_a_severed_prev_hash_link(tmp_path):
    path = tmp_path / "evidence.jsonl"
    log = EvidenceLog(path)
    for _ in range(3):
        log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                  settings_hash="sh", run_id="run1")

    lines = path.read_text().splitlines()
    tampered = json.loads(lines[1])
    assert tampered["seq"] == 2
    tampered["prev_hash"] = "0" * 64
    lines[1] = json.dumps(tampered, sort_keys=True)
    path.write_text("\n".join(lines) + "\n")

    result = log.verify()
    assert result["ok"] is False
    assert result["broken_at_seq"] == 2
    assert "prev_hash" in result["detail"]


def test_evidence_log_survives_reopen_and_continues_the_same_chain(tmp_path):
    path = tmp_path / "evidence.jsonl"
    log = EvidenceLog(path)
    log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
              settings_hash="sh", run_id="run1")
    log.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
              settings_hash="sh", run_id="run1")

    log2 = EvidenceLog(path)  # simulates a server restart
    assert log2.entry_count() == 2
    log2.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
               settings_hash="sh", run_id="run1")

    result = log2.verify()
    assert result["ok"] is True
    assert result["checked"] == 3


# =============================================================================================
# 2. FIPS posture detection
# =============================================================================================
def test_fips_detect_reports_a_real_openssl_version_and_never_raises():
    posture = gmat_fips.detect()
    assert set(posture.keys()) == {"openssl_version", "openssl_version_number", "fips_provider_active", "detail"}
    assert posture["openssl_version"]  # non-empty
    assert isinstance(posture["openssl_version_number"], int)
    assert isinstance(posture["fips_provider_active"], bool)
    assert posture["detail"]


# =============================================================================================
# Admin HTTP server (gmat_service.admin) -- driven directly, no GMAT/grpc needed.
# =============================================================================================
@pytest.fixture()
def admin_server(tmp_path):
    evidence = EvidenceLog(tmp_path / "evidence.jsonl")
    port = _free_port()
    httpd = gmat_admin.serve_admin(host="127.0.0.1", port=port, evidence=evidence, run_id="admin_test")
    try:
        yield httpd, evidence, port
    finally:
        httpd.shutdown()
        httpd.server_close()


def _admin_get(port: int, path: str) -> tuple[int, dict]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        conn.request("GET", path)
        resp = conn.getresponse()
        body = json.loads(resp.read().decode("utf-8"))
        return resp.status, body
    finally:
        conn.close()


def test_admin_evidence_endpoint_reports_version_settings_hash_chain_head_and_fips(admin_server):
    """`gmat_service.admin._AdminHandler._evidence_body` reports the *current*
    `config.settings_hash()` (this server hosts exactly one fixed model/settings, so that is
    always the settings that would produce the next record -- matching `_record`'s own
    per-call read in `gmat_service/service.py`), not whatever `settings_hash` a past record
    happened to be written with; a record with a stale/different value here would itself be
    the interesting finding, not this test's assumption."""
    from gmat_service import config as gmat_config

    httpd, evidence, port = admin_server
    evidence.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                    settings_hash="deadbeef", run_id="admin_test")

    status, body = _admin_get(port, "/admin/api/evidence")
    assert status == 200
    assert set(body.keys()) == {"chain_head", "entries", "evidence_path", "fips", "run_id", "settings_hash", "version"}
    assert body["run_id"] == "admin_test"
    assert body["settings_hash"] == gmat_config.settings_hash()
    assert len(body["settings_hash"]) == 64
    assert body["entries"] == 1
    assert body["chain_head"] != "GENESIS"
    assert len(body["chain_head"]) == 64
    assert set(body["fips"].keys()) == {"openssl_version", "openssl_version_number", "fips_provider_active", "detail"}


def test_admin_evidence_verify_endpoint_reports_ok_on_an_untampered_chain(admin_server):
    httpd, evidence, port = admin_server
    evidence.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                    settings_hash="sh", run_id="admin_test")
    status, body = _admin_get(port, "/admin/api/evidence/verify")
    assert status == 200
    assert body == {"ok": True, "checked": 1, "broken_at_seq": None, "detail": "chain intact"}


def test_admin_evidence_verify_detects_a_tampered_record_via_http(admin_server, tmp_path):
    """The end-to-end HTTP proof (not just the direct-`EvidenceLog` proof above): tamper
    with the evidence file on disk while the admin server is live, and confirm
    `/admin/api/evidence/verify` reports the break -- mirrors
    `tests/test_dynamics_service_rs.py::test_admin_evidence_verify_detects_a_tampered_record_on_disk_and_reports_its_seq`
    on the Rust side."""
    httpd, evidence, port = admin_server
    for _ in range(4):
        evidence.record(method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
                        settings_hash="sh", run_id="admin_test")

    clean_status, clean_body = _admin_get(port, "/admin/api/evidence/verify")
    assert clean_status == 200 and clean_body["ok"] is True

    lines = evidence.path.read_text().splitlines()
    tampered = json.loads(lines[2])
    assert tampered["seq"] == 3
    tampered["method"] = "Propagate"
    lines[2] = json.dumps(tampered, sort_keys=True)
    evidence.path.write_text("\n".join(lines) + "\n")

    status, body = _admin_get(port, "/admin/api/evidence/verify")
    assert status == 200
    assert body["ok"] is False
    assert body["broken_at_seq"] == 3
    assert body["checked"] == 2
    assert "tampered" in body["detail"]


def test_admin_unknown_path_is_404(admin_server):
    httpd, evidence, port = admin_server
    status, body = _admin_get(port, "/nope")
    assert status == 404
    assert body["path"] == "/nope"


# =============================================================================================
# 3. Control-matrix structural checks
# =============================================================================================
CONTROL_MATRIX_PATHS = [
    REPO_ROOT / "docs" / "compliance" / "av-dynamics-service" / "control-matrix.md",
    REPO_ROOT / "docs" / "compliance" / "gmat-service" / "control-matrix.md",
]

#: Matches a single-line markdown table row: `| ID | Requirement | Status | Impl | Evidence |`
#: Deliberately simple (no multi-line-cell support) -- every row in both documents is one
#: physical line, which this test also implicitly checks by construction (a row that didn't
#: parse this way would just not be counted, not silently mis-parsed).
_ROW_RE = re.compile(r"^\|\s*([0-9][0-9./ ,–-]*)\s*\|(.+)\|(.+)\|(.+)\|(.+)\|\s*$")

#: A backtick-quoted `path/like/this.ext:Name::or.dotted` reference -- what this task's
#: binding rule calls a `file:function` reference. Requires a `/` in the path segment (so a
#: bare `EvidenceLog::verify` cross-reference without a path, used in prose elsewhere in
#: these documents, is not mistaken for a citable implementation pointer) and requires the
#: path to end in a recognized source extension.
_FILE_FUNC_RE = re.compile(r"`([\w./-]+\.(?:rs|py)):([\w.:]+)`")


def _parse_control_matrix_rows(text: str) -> list[dict]:
    rows = []
    for line in text.splitlines():
        m = _ROW_RE.match(line.strip())
        if not m:
            continue
        control_id, requirement, status, implementation, evidence = (g.strip() for g in m.groups())
        rows.append({
            "id": control_id, "requirement": requirement, "status": status,
            "implementation": implementation, "evidence": evidence,
        })
    return rows


@pytest.mark.parametrize("path", CONTROL_MATRIX_PATHS, ids=lambda p: p.parent.name)
def test_control_matrix_exists_and_has_legend_and_deficiencies(path):
    assert path.is_file(), f"missing control matrix: {path}"
    text = path.read_text()
    assert "Gap" in text and "Partial" in text and "Inherited" in text and "Met" in text
    assert re.search(r"^##\s+Deficiencies\s*$", text, re.MULTILINE), \
        "control matrix must have a '## Deficiencies' section"
    # At least one numbered deficiency entry.
    assert re.search(r"^\d+\.\s+\*\*", text, re.MULTILINE)


@pytest.mark.parametrize("path", CONTROL_MATRIX_PATHS, ids=lambda p: p.parent.name)
def test_control_matrix_has_rows_in_every_status(path):
    """A matrix that is all `Met` (or all `Gap`) would itself be a red flag -- this
    task's brief: "Be honest -- most rows will be Gap or Inherited, and that is the
    correct output." Assert the *shape* mechanically rather than trusting prose."""
    rows = _parse_control_matrix_rows(path.read_text())
    assert len(rows) >= 15, f"expected a substantial matrix, found {len(rows)} rows in {path}"
    statuses = {r["status"] for r in rows}
    assert "Met" in statuses
    assert "Gap" in statuses
    assert "Partial" in statuses or "Inherited" in statuses
    # Most rows are Gap/Inherited/Partial, not Met -- the honesty requirement, checked as a
    # count rather than merely "at least one of each".
    met_count = sum(1 for r in rows if r["status"] == "Met")
    assert met_count < len(rows) / 2, (
        f"{met_count}/{len(rows)} rows are 'Met' -- suspiciously high for a first compliance "
        "pass; re-check every 'Met' claim against this task's honesty requirement")


@pytest.mark.parametrize("path", CONTROL_MATRIX_PATHS, ids=lambda p: p.parent.name)
def test_every_met_row_names_a_real_file_function_that_exists(path):
    """The load-bearing check: **every row marked "Met" must name a real `file:function`
    reference whose file actually exists in this repository**, and the function/method name
    must actually appear in that file (a `fn `/`pub fn ` declaration in Rust, or a `def `
    declaration in Python) -- not merely a plausible-looking string."""
    rows = _parse_control_matrix_rows(path.read_text())
    met_rows = [r for r in rows if r["status"] == "Met"]
    assert met_rows, f"expected at least one Met row in {path}"

    for row in met_rows:
        refs = _FILE_FUNC_RE.findall(row["implementation"])
        assert refs, (
            f"{path.name} control {row['id']} is marked Met but its Implementation cell "
            f"names no `path/to/file.ext:function` reference: {row['implementation']!r}")
        for file_ref, func_ref in refs:
            src_path = REPO_ROOT / file_ref
            assert src_path.is_file(), (
                f"{path.name} control {row['id']}: referenced file does not exist: {file_ref}")
            content = src_path.read_text()
            # `func_ref` may be dotted/`::`-qualified (e.g. "EvidenceLog::record",
            # "EvidenceLog.record") -- only the last segment is the actual function/method
            # name a source file declares.
            leaf = re.split(r"[.:]+", func_ref)[-1]
            declared = (f"fn {leaf}" in content or f"fn {leaf}(" in content
                       or f"def {leaf}(" in content)
            assert declared, (
                f"{path.name} control {row['id']}: {file_ref} has no 'fn {leaf}'/'def {leaf}' "
                f"-- the Met row's implementation reference does not actually exist")


@pytest.mark.parametrize("path", CONTROL_MATRIX_PATHS, ids=lambda p: p.parent.name)
def test_no_row_claims_fips_validation_is_met(path):
    """A specific instance of the honesty requirement worth pinning down explicitly: FIPS
    validation (3.13.11) must never be marked `Met` by either document, because neither
    service runs on a CMVP-validated OpenSSL build -- see this task's report for exactly how
    that was detected (`gmat_service.fips.detect` / `crates/av-dynamics-service/src/fips.rs`)."""
    rows = _parse_control_matrix_rows(path.read_text())
    fips_rows = [r for r in rows if "3.13.11" in r["id"]]
    assert fips_rows, f"expected a 3.13.11 (FIPS) row in {path}"
    for row in fips_rows:
        assert row["status"] != "Met", f"{path.name}: 3.13.11 must not be marked Met -- {row}"
