"""`docs/edge-plan.md` milestone E2: the real end-to-end proof, against a genuine local
seccert (RFC 8555 ACME CA) + lego (a standard ACME client) loop -- `scripts/
edge_local_ca.py` does the provisioning; this file drives `crates/av-edge`'s two small
CLI helpers (`av-edge-identity`, `av-edge-make-signed-batch`) against what that
provisioning actually produced, and asserts the facts with `openssl` directly.

Skips **visibly** (question 194, `-rs` in `pyproject.toml` prints the reason) when
seccert's venv or the `lego` binary is not present, or when `cargo` cannot build the two
Rust binaries this file needs. Never mutates `os.environ` of this pytest process --
`scripts/edge_local_ca.py`'s subprocesses are all started with an explicit `env=`
argument, and so is `cargo build` here (question 199).

TAI/Unix conversion: `av_cdm::time::Tai` documents that its UTC conversion extrapolates
the leap-second table's last entry (`data/time/leap_seconds.json`: 37s, effective
2017-01-01) forward forever; every date in these tests is 2023 or later, so
`tai_ns == unix_ns + 37_000_000_000` exactly, and this file uses that fact directly rather
than re-deriving the leap-second table in Python.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))
import edge_local_ca as ca  # noqa: E402  (path insert must precede this import)

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
GMAT_ROOT = "/Users/probe/code/AltaVista/GMAT R2026a"
CFS_MIRROR_DIR = "/Users/probe/code/AltaVista/third_party/mirrors"

OPENSSL_BIN = "openssl"  # LibreSSL 3.3.6 on this host -- fine for the plain `-text`/
# `verify`/`-dates`/`-noout -subject` assertions this file makes (no Python `cryptography`
# import anywhere in this file, per this task's own instruction).

TAI_MINUS_UNIX_NS = 37_000_000_000  # see module docstring

# Read ONCE at import time (never assigned back into `os.environ` -- reading an inherited
# variable is not the mutation question 199 forbids) so this file's own skip path can be
# demonstrated honestly: launch pytest itself with
# `ALTAVISTA_EDGE_TEST_SECCERT_DIR=/some/nonexistent/path` in the *subprocess* env (the
# normal way every other environment variable in this task reaches a subprocess, e.g.
# `_cargo_env`'s PATH/GMAT_ROOT), and `_require_seccert_and_lego` below threads it into
# `check_seccert_available` as a genuine function argument -- never by editing this test
# to always skip.
_SECCERT_DIR_OVERRIDE = os.environ.get("ALTAVISTA_EDGE_TEST_SECCERT_DIR")
SECCERT_DIR_FOR_TEST = Path(_SECCERT_DIR_OVERRIDE) if _SECCERT_DIR_OVERRIDE else ca.SECCERT_DIR


def _cargo_env() -> dict:
    """A fresh copy of the process environment, with the additions every `cargo`/`pytest`
    invocation in this task needs (never assigned back into `os.environ` itself --
    question 199)."""
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    env["GMAT_ROOT"] = GMAT_ROOT
    env["CFS_MIRROR_DIR"] = CFS_MIRROR_DIR
    return env


def unix_to_tai_ns(unix_seconds: float) -> int:
    return int(round(unix_seconds * 1_000_000_000)) + TAI_MINUS_UNIX_NS


def now_tai_ns() -> int:
    return unix_to_tai_ns(datetime.now(timezone.utc).timestamp())


def _run(args: list, **kwargs) -> subprocess.CompletedProcess:
    kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    kwargs.setdefault("timeout", 60)
    return subprocess.run(args, **kwargs)


def _openssl(args: list) -> subprocess.CompletedProcess:
    proc = _run([OPENSSL_BIN, *args])
    if proc.returncode != 0:
        pytest.fail(f"openssl {' '.join(args)} failed (rc={proc.returncode})\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return proc


# --------------------------------------------------------------------------- availability

def _require_seccert_and_lego() -> None:
    reason = ca.check_seccert_available(seccert_dir=SECCERT_DIR_FOR_TEST)
    if reason:
        pytest.skip(f"seccert unavailable, skipping the real seccert+lego E2 identity test: {reason}")
    reason = ca.check_lego_available()
    if reason:
        pytest.skip(f"lego unavailable, skipping the real seccert+lego E2 identity test: {reason}")


# --------------------------------------------------------------------------- fixtures

@pytest.fixture(scope="module")
def av_edge_binaries():
    """Builds `av-edge-identity` and `av-edge-make-signed-batch` once for the module.
    Skips visibly if `cargo` itself is not on `PATH` (question 194: docker/toolchain-gated
    tests skip visibly or run for real, never pass silently); a build failure once cargo
    IS present is a real failure of this task, not something to paper over by skipping."""
    env = _cargo_env()
    cargo = None
    for candidate in (f"{RUSTUP_PATH_PREFIX}/cargo", "cargo"):
        found = subprocess.run(["which", candidate], capture_output=True, text=True, env=env).stdout.strip()
        if found:
            cargo = candidate
            break
    if cargo is None:
        pytest.skip("cargo not found on PATH (checked with rustup's bin dir prepended) -- skipping the real seccert+lego E2 identity test")

    proc = subprocess.run(
        [cargo, "build", "-p", "av-edge", "--bin", "av-edge-identity", "--bin", "av-edge-make-signed-batch"],
        cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=600)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-edge (the two CLI helpers) failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")

    identity_bin = REPO_ROOT / "target" / "debug" / "av-edge-identity"
    make_batch_bin = REPO_ROOT / "target" / "debug" / "av-edge-make-signed-batch"
    assert identity_bin.is_file(), f"expected {identity_bin} after a successful cargo build"
    assert make_batch_bin.is_file(), f"expected {make_batch_bin} after a successful cargo build"
    return identity_bin, make_batch_bin


@pytest.fixture(scope="module")
def provisioned(tmp_path_factory):
    """Runs the real seccert+lego provisioning once for the module (four leaves, two
    independent seccert instances) -- `scripts/edge_local_ca.provision`. Both seccert
    processes are guaranteed shut down (by `provision` itself, on every exit path) before
    this fixture's `with`-less body even returns; nothing here needs its own cleanup."""
    _require_seccert_and_lego()
    output_dir = tmp_path_factory.mktemp("edge_local_ca")
    return ca.provision(output_dir)


def _identity_json(identity_bin: Path, *, ca_file: Path, chain_file: Path | None, leaf_file: Path, now_tai: int, verify_batch: Path | None = None) -> tuple[dict, int]:
    args = [str(identity_bin), "--ca", str(ca_file), "--leaf", str(leaf_file), "--now-tai-ns", str(now_tai)]
    if chain_file is not None:
        args += ["--chain", str(chain_file)]
    if verify_batch is not None:
        args += ["--verify-batch", str(verify_batch)]
    proc = _run(args)
    assert proc.stdout.strip(), f"av-edge-identity produced no stdout (rc={proc.returncode})\n--- stderr ---\n{proc.stderr}"
    try:
        parsed = json.loads(proc.stdout.strip().splitlines()[-1])
    except json.JSONDecodeError as e:
        pytest.fail(f"av-edge-identity did not print valid JSON: {e}\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return parsed, proc.returncode


# --------------------------------------------------------------------------- issuance itself

def test_issuance_end_to_end_against_the_local_ca(provisioned):
    """Both `edge.localhost` and `ingest.localhost` exist, are EC P-384, chain to their
    Root (`openssl verify`), and have distinct subjects."""
    for result in (provisioned.edge, provisioned.ingest):
        assert result.cert_pem.is_file(), result.cert_pem
        assert result.issuer_pem.is_file(), result.issuer_pem
        assert result.key_pem.is_file(), result.key_pem

        text = _openssl(["x509", "-in", str(result.cert_pem), "-noout", "-text"]).stdout
        assert "ASN1 OID: secp384r1" in text, text
        assert "NIST CURVE: P-384" in text, text

        _openssl(["verify", "-CAfile", str(provisioned.seccert1.root_pem), "-untrusted", str(result.issuer_pem), str(result.cert_pem)])

    edge_subject = _openssl(["x509", "-in", str(provisioned.edge.cert_pem), "-noout", "-subject"]).stdout.strip()
    ingest_subject = _openssl(["x509", "-in", str(provisioned.ingest.cert_pem), "-noout", "-subject"]).stdout.strip()
    assert edge_subject != ingest_subject, (edge_subject, ingest_subject)
    assert "edge.localhost" in edge_subject
    assert "ingest.localhost" in ingest_subject


def test_short_leaf_notafter_honours_the_requested_one_minute_lifetime(provisioned):
    """seccert honoured lego's `--not-after`: the short-lived leaf's own `notAfter` is
    within a few seconds of what was requested (`provisioned.short_not_after`), not the
    default 90-day lifetime."""
    enddate = _openssl(["x509", "-in", str(provisioned.short.cert_pem), "-noout", "-enddate"]).stdout.strip()
    assert enddate.startswith("notAfter=")
    from email.utils import parsedate_to_datetime  # stdlib only

    not_after = parsedate_to_datetime(enddate[len("notAfter="):])
    delta = abs((not_after - provisioned.short_not_after).total_seconds())
    assert delta < 30, f"requested not_after={provisioned.short_not_after}, actual notAfter={not_after} (delta={delta}s)"


# --------------------------------------------------------------------------- av-edge-identity

def test_edge_leaf_is_accepted_under_the_seccert_root(provisioned, av_edge_binaries):
    identity_bin, _ = av_edge_binaries
    parsed, rc = _identity_json(
        identity_bin,
        ca_file=provisioned.seccert1.root_pem,
        chain_file=provisioned.edge.issuer_pem,
        leaf_file=provisioned.edge.cert_pem,
        now_tai=now_tai_ns(),
    )
    assert rc == 0, parsed
    assert parsed["accepted"] is True, parsed
    assert parsed["subject_cn"] == "edge.localhost", parsed
    assert len(parsed["fingerprint_sha256"]) == 64, parsed

    # Reproducible by hand from the same PEM file, exactly as `identity.rs`'s doc comment
    # promises: `openssl x509 -fingerprint -sha256` (colons/case aside) must match.
    fp = _openssl(["x509", "-in", str(provisioned.edge.cert_pem), "-noout", "-fingerprint", "-sha256"]).stdout.strip()
    fp_hex = fp.split("=", 1)[1].replace(":", "").lower()
    assert parsed["fingerprint_sha256"] == fp_hex, (parsed["fingerprint_sha256"], fp_hex)


def test_foreign_leaf_is_refused_as_issuer_not_trusted(provisioned, av_edge_binaries):
    identity_bin, _ = av_edge_binaries
    parsed, rc = _identity_json(
        identity_bin,
        ca_file=provisioned.seccert1.root_pem,  # the trusted Root -- NOT seccert2's
        chain_file=provisioned.foreign.issuer_pem,
        leaf_file=provisioned.foreign.cert_pem,
        now_tai=now_tai_ns(),
    )
    assert rc != 0, parsed
    assert parsed["accepted"] is False, parsed
    assert parsed["rejection"] == "ISSUER_NOT_TRUSTED", parsed


def test_short_leaf_accepted_just_inside_and_refused_just_after_its_window(provisioned, av_edge_binaries):
    identity_bin, _ = av_edge_binaries
    enddate = _openssl(["x509", "-in", str(provisioned.short.cert_pem), "-noout", "-enddate"]).stdout.strip()
    from email.utils import parsedate_to_datetime

    not_after = parsedate_to_datetime(enddate[len("notAfter="):])

    inside_tai = unix_to_tai_ns((not_after - timedelta(seconds=5)).timestamp())
    parsed_inside, rc_inside = _identity_json(
        identity_bin, ca_file=provisioned.seccert1.root_pem, chain_file=provisioned.short.issuer_pem,
        leaf_file=provisioned.short.cert_pem, now_tai=inside_tai)
    assert rc_inside == 0, parsed_inside
    assert parsed_inside["accepted"] is True, parsed_inside

    after_tai = unix_to_tai_ns((not_after + timedelta(seconds=5)).timestamp())
    parsed_after, rc_after = _identity_json(
        identity_bin, ca_file=provisioned.seccert1.root_pem, chain_file=provisioned.short.issuer_pem,
        leaf_file=provisioned.short.cert_pem, now_tai=after_tai)
    assert rc_after != 0, parsed_after
    assert parsed_after["accepted"] is False, parsed_after
    assert parsed_after["rejection"] == "EXPIRED", parsed_after


# --------------------------------------------------------------------------- E1/E2 bridge

def test_batch_signed_with_the_issued_leaf_verifies_under_the_leaf_public_key(provisioned, av_edge_binaries):
    """A batch signed with the `edge.localhost` leaf's own (lego-issued) private key
    verifies, driven entirely through the CLI: `av-edge-make-signed-batch` produces the
    batch, `av-edge-identity --verify-batch` checks it against the identity extracted
    from the certificate."""
    identity_bin, make_batch_bin = av_edge_binaries

    fp = _openssl(["x509", "-in", str(provisioned.edge.cert_pem), "-noout", "-fingerprint", "-sha256"]).stdout.strip()
    fingerprint_hex = fp.split("=", 1)[1].replace(":", "").lower()

    batch_path = provisioned.edge.cert_pem.parent / "signed_batch.pb"
    proc = _run([
        str(make_batch_bin),
        "--key", str(provisioned.edge.key_pem),
        "--fingerprint-sha256", fingerprint_hex,
        "--producer-id", "sim-asset-1",
        "--sequence", "1",
        "--batch-tai-ns", str(now_tai_ns()),
        "--out", str(batch_path),
    ])
    assert proc.returncode == 0, f"av-edge-make-signed-batch failed\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    assert batch_path.is_file(), batch_path

    parsed, rc = _identity_json(
        identity_bin,
        ca_file=provisioned.seccert1.root_pem,
        chain_file=provisioned.edge.issuer_pem,
        leaf_file=provisioned.edge.cert_pem,
        now_tai=now_tai_ns(),
        verify_batch=batch_path,
    )
    assert rc == 0, parsed
    assert parsed["accepted"] is True, parsed
    assert parsed["batch_verified"] is True, parsed
    assert parsed["batch_error"] is None, parsed


# --------------------------------------------------------------------------- the skip path

def test_availability_helpers_skip_visibly_when_pointed_at_a_missing_path():
    """Requirement: prove the skip path itself prints its reason, honestly -- by pointing
    the availability check at a path that does not exist (a function argument), never by
    editing this test to always skip. This test itself never skips."""
    reason = ca.check_seccert_available(seccert_dir=Path("/nonexistent/seccert-does-not-exist"))
    assert reason is not None
    assert "/nonexistent/seccert-does-not-exist" in reason

    reason = ca.check_lego_available(lego_bin="/nonexistent/lego-does-not-exist")
    assert reason is not None
    assert "/nonexistent/lego-does-not-exist" in reason
