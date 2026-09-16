"""Question 202's own charter, closed end to end: `services/av-ingest/deploy/
nginx-av-ingest-grpc.conf.template` rendered and run through a REAL nginx, fronting a REAL
`crates/av-ingest/src/bin/av-ingest-server.rs` subprocess, driven by a REAL mTLS gRPC
client (`crates/av-ingest-client/src/bin/av-ingest-mtls-client.rs`) -- never an in-process
`tonic::transport::Server` the way `crates/av-ingest/tests/wire_identity.rs` proves the
identity path (that file is exactly right for proving the logic without a front; this file
is the front's own proof, which by definition needs a front).

# The CA: real seccert + lego, not the cheaper OpenSSL-CLI test CA

Question 200(b) makes seccert the test CA. `scripts/edge_local_ca.py::provision` (round
1's own recipe, already proven end to end by `tests/test_edge_identity_seccert.py`) is
reused here for exactly the leaves this file needs: `edge.localhost` (the "good" leaf),
`foreign.localhost` (issued by a second, independent seccert instance -- the "wrong CA"
leaf) and `short.localhost` (a genuinely one-minute-lifetime leaf, for the "lapsed" case).
This proved entirely practical -- no fallback to `tests/test_grpc_tls.py::_build_test_ca`
was needed for the CLIENT-identity CA. The nginx front's own TLS SERVER certificate is a
separate, throwaway, self-signed EC P-384 leaf built with the Homebrew OpenSSL CLI (SAN
`IP:127.0.0.1`, since there is no DNS here): seccert's own leaves only ever carry a DNSName
SAN for the domain they were issued for (`scripts/edge_local_ca.py`'s own `_SeccertServer.
__enter__` comment explains why: no IP SAN, ever), and resolving `edge.localhost` et al.
to `127.0.0.1` for THIS test's own client would add a DNS dependency this file's mTLS proof
does not need -- the server's own TLS identity is orthogonal to the client-certificate
identity question 202 is actually about. This is a deliberate scope split, not a shortcut:
the CA that matters for this file's own charter (the CLIENT'S identity, verified in
process by `av_edge::identity::verify_identity`) is the real seccert CA throughout.

Skips **visibly** (question 194, `-rs` prints every reason) when nginx, seccert, or lego is
not present on this machine -- never a silent pass.

# The `ssl_verify_client` decision: `optional_no_ca`, not `on` -- and NOT plain `optional`

See the template's own header comment for the full argument; the short version: `services/
gmat-service/deploy`'s own template uses `ssl_verify_client on`, under which nginx itself is
the enforcement point and a wrong-CA/expired/missing client certificate never reaches the
backend at all -- exactly wrong for THIS front, whose entire point (question 202) is that a
wrong-CA or lapsed identity lands in `av-ingest`'s own `IdentityCounters`, not only in
nginx's error log. An EARLIER DRAFT of this template used plain `ssl_verify_client
optional`, and this test file itself caught, empirically, that this does not do what its
name suggests: `SSL_VERIFY_PEER` (what both `optional` and `on` set) only tolerates a
MISSING certificate -- once ANY certificate is presented, a verification failure
(untrusted CA, expired) still fails the TLS handshake itself, exactly like `on`, just for a
different input; `test_leaf_from_a_different_ca_is_refused_...` failed with nginx's own
plain HTTP 400 the first time this file ran, against `optional`. `ssl_verify_client
optional_no_ca` is what actually gives the intended shape: nginx still terminates TLS and
still forwards whatever certificate was actually presented (trusted or not, expired or not,
or none at all) via `$ssl_client_escaped_cert`; `av-ingest`'s own `Announce` handler is the
one and only enforcement point, via `av_edge::identity::verify_identity` against its OWN
configured trust anchors (`av-ingest-server`'s own `--trust-anchor`, entirely independent of
what this template's `ssl_client_certificate` names). Every test below proves one leg of
exactly this: `test_valid_seccert_leaf_...` (accepted, counted),
`test_leaf_from_a_different_ca_...` (refused, `issuer_not_trusted` counted IN PROCESS),
`test_lapsed_leaf_...` (refused, `expired` counted IN PROCESS, using the injected clock,
never sleeping), and `test_no_client_certificate_...` (refused, and it is `av-ingest`, not
nginx, that refused it -- the connection itself succeeds).

# What nginx actually sends on the header, observed

`crates/av-ingest/src/service.rs::announce` now prints the first 120 characters of every
forwarded-certificate header it receives to its own stderr (a permanent, always-on
diagnostic line -- see that file's own comment for why this is not gated on any
config/environment flag). `test_valid_seccert_leaf_is_accepted_through_nginx_and_batches_submit`
captures that line from the real subprocess's stderr and prints it again here, as the
actual evidence question 148 asks for -- not a description of what nginx's documentation
says, an observation of what it actually sent. What was found: nginx's real
`$ssl_client_escaped_cert` percent-encodes exactly as `crates/av-ingest/src/
forwarded_cert.rs`'s module doc already documented (letters/digits/`-._~` pass through
unescaped; a PEM's newlines become `%0A`; the literal space in `"BEGIN CERTIFICATE"`
becomes `%20`) -- so **no defect was found and no fix to `forwarded_cert.rs` was needed**;
that module's doc has been updated (see its own diff) to say "observed", quoting this file's
own capture, where it used to say "not observed".

# `grpc_set_header` vs `proxy_set_header`, measured rather than assumed

`test_proxy_set_header_is_not_honoured_for_a_grpc_pass_location` renders a second, minimal
variant of the SAME template with its one `grpc_set_header` line swapped for
`proxy_set_header` (otherwise identical), starts a real nginx with it, and drives a request
with a valid client certificate through it: `nginx -t` accepts the config either way (an
unused directive from a different module is not a syntax error), but the backend receives
NO forwarded-certificate header at all under `proxy_set_header` -- confirming
`ngx_http_grpc_module`'s own `grpc_set_header` is the directive a `grpc_pass` location
actually honours, `ngx_http_proxy_module`'s `proxy_set_header` is not, and this was
measured, not assumed.
"""
from __future__ import annotations

import contextlib
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
from datetime import timedelta
from email.utils import parsedate_to_datetime
from pathlib import Path
from types import SimpleNamespace

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))
import edge_local_ca as ca  # noqa: E402  (path insert must precede this import)

from altavista.test_env import resolve_cfs_mirror_dir, resolve_gmat_root  # noqa: E402

DEPLOY_DIR = REPO_ROOT / "services" / "av-ingest" / "deploy"
TEMPLATE_PATH = DEPLOY_DIR / "nginx-av-ingest-grpc.conf.template"
FORWARDED_CERT_RS = REPO_ROOT / "crates" / "av-ingest" / "src" / "forwarded_cert.rs"

NGINX_BIN = "/opt/homebrew/bin/nginx"
OPENSSL_BIN = "/opt/homebrew/opt/openssl@3/bin/openssl"  # Homebrew OpenSSL, NOT the
# system LibreSSL -- see tests/test_grpc_tls.py's own identical note on ECDSA P-384.

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"

# Resolved ONCE at import time (question 217(d)): see `tests/test_edge_identity_seccert.py`'s
# own identical note and `altavista/test_env.py`'s own doc -- env var first, then this
# worktree's own machine-local `GMAT R2026a`/`third_party/mirrors` entries, else `None` with a
# named reason rather than a `/Users/probe` literal. Never assigned back into `os.environ`
# (question 199).
GMAT_ROOT, _GMAT_ROOT_SKIP_REASON = resolve_gmat_root()
CFS_MIRROR_DIR, _CFS_MIRROR_DIR_SKIP_REASON = resolve_cfs_mirror_dir()
OPENSSL_DIR = "/opt/homebrew/opt/openssl@3"

NGINX_READY_TIMEOUT_S = 10.0
SERVER_READY_TIMEOUT_S = 15.0
TAI_MINUS_UNIX_NS = 37_000_000_000  # see tests/test_edge_identity_seccert.py's own note


def _cargo_env() -> dict:
    """Never assigned back into `os.environ` itself (question 199). Only overrides
    GMAT_ROOT/CFS_MIRROR_DIR when they actually resolved (`av_ingest_binaries` below skips
    visibly before ever calling this when they did not) -- never overwrites an inherited
    value with a `/Users/probe` literal that might not exist on this machine."""
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    if GMAT_ROOT is not None:
        env["GMAT_ROOT"] = GMAT_ROOT
    if CFS_MIRROR_DIR is not None:
        env["CFS_MIRROR_DIR"] = CFS_MIRROR_DIR
    env["OPENSSL_DIR"] = OPENSSL_DIR
    return env


def unix_to_tai_ns(unix_seconds: float) -> int:
    return int(round(unix_seconds * 1_000_000_000)) + TAI_MINUS_UNIX_NS


def now_tai_ns() -> int:
    import datetime as _dt

    return unix_to_tai_ns(_dt.datetime.now(_dt.timezone.utc).timestamp())


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_port(port: int, timeout_s: float) -> None:
    deadline = time.monotonic() + timeout_s
    last_err = None
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return
        except OSError as e:
            last_err = e
            time.sleep(0.05)
    raise TimeoutError(f"127.0.0.1:{port} never accepted a connection: {last_err}")


def _run(args: list[str], **kwargs) -> subprocess.CompletedProcess:
    kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    kwargs.setdefault("timeout", 60)
    proc = subprocess.run(args, **kwargs)
    if proc.returncode != 0:
        pytest.fail(f"command failed (rc={proc.returncode}): {' '.join(args)}\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return proc


# --------------------------------------------------------------------------- availability

def _require_nginx() -> None:
    if not Path(NGINX_BIN).is_file():
        pytest.skip(f"nginx not found at {NGINX_BIN} -- skipping the av-ingest mTLS front test")


def _require_seccert_and_lego() -> None:
    reason = ca.check_seccert_available()
    if reason:
        pytest.skip(f"seccert unavailable, skipping the real seccert+lego av-ingest mTLS front test: {reason}")
    reason = ca.check_lego_available()
    if reason:
        pytest.skip(f"lego unavailable, skipping the real seccert+lego av-ingest mTLS front test: {reason}")


# --------------------------------------------------------------------------- the front's own TLS server identity

def _openssl(args: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return _run([OPENSSL_BIN, *args], cwd=cwd)


def _build_front_server_cert(cwd: Path) -> SimpleNamespace:
    """A throwaway, self-signed EC P-384 leaf (SAN `IP:127.0.0.1`) for the nginx front's
    OWN TLS server identity -- deliberately NOT a seccert leaf (see this module's own
    docstring for why: seccert's leaves carry no IP SAN, and the server's own TLS identity
    is orthogonal to the client-certificate identity this file is actually about). Used
    directly as its own trust anchor by this file's mTLS client (a self-signed cert is a
    valid, if minimal, CA file for that purpose)."""
    cwd.mkdir(parents=True, exist_ok=True)
    key = cwd / "front_server.key"
    crt = cwd / "front_server.crt"
    _openssl(["ecparam", "-name", "secp384r1", "-genkey", "-noout", "-out", str(key)], cwd)
    _openssl(
        [
            "req", "-x509", "-new", "-key", str(key), "-sha384", "-days", "2",
            "-subj", "/CN=127.0.0.1",
            "-addext", "subjectAltName=IP:127.0.0.1",
            "-addext", "keyUsage=critical,digitalSignature",
            "-addext", "extendedKeyUsage=serverAuth",
            "-out", str(crt),
        ],
        cwd,
    )
    return SimpleNamespace(key=key, crt=crt)


def _fullchain(dest: Path, leaf: Path, issuer: Path) -> Path:
    dest.write_text(leaf.read_text() + issuer.read_text())
    return dest


# --------------------------------------------------------------------------- rust binaries

@pytest.fixture(scope="module")
def av_ingest_binaries():
    """Builds `av-ingest-server` (crates/av-ingest) and `av-ingest-mtls-client`
    (crates/av-ingest-client) once for the module. Skips visibly (question 194) if
    `GMAT_ROOT`/`CFS_MIRROR_DIR` could not be resolved -- see `tests/
    test_edge_identity_seccert.py::av_edge_binaries`'s own identical note. A build failure
    once both directories ARE present is a real failure of this task, never something to
    skip."""
    if _GMAT_ROOT_SKIP_REASON is not None:
        pytest.skip(_GMAT_ROOT_SKIP_REASON)
    if _CFS_MIRROR_DIR_SKIP_REASON is not None:
        pytest.skip(_CFS_MIRROR_DIR_SKIP_REASON)
    env = _cargo_env()
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-ingest", "--bin", "av-ingest-server", "-p", "av-ingest-client", "--bin", "av-ingest-mtls-client"],
        cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=900,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build (av-ingest-server / av-ingest-mtls-client) failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    server_bin = REPO_ROOT / "target" / "debug" / "av-ingest-server"
    client_bin = REPO_ROOT / "target" / "debug" / "av-ingest-mtls-client"
    assert server_bin.is_file(), server_bin
    assert client_bin.is_file(), client_bin
    return SimpleNamespace(server=server_bin, client=client_bin)


# --------------------------------------------------------------------------- seccert provisioning

@pytest.fixture(scope="module")
def provisioned(tmp_path_factory):
    _require_seccert_and_lego()
    output_dir = tmp_path_factory.mktemp("edge_ingest_mtls_ca")
    return ca.provision(output_dir)


@pytest.fixture(scope="module")
def combined_intermediates(provisioned, tmp_path_factory) -> Path:
    """seccert1's Intermediate (issues `edge.localhost`/`ingest.localhost`/
    `short.localhost`) AND seccert2's Intermediate (issues `foreign.localhost`),
    concatenated -- `av_edge::identity::TrustAnchors`'s own module doc / `av_edge::
    identity::parse_chain_pem` accepts a chain PEM with more than one certificate; this
    lets ONE `av-ingest-server` instance build a full (if, for the foreign leaf,
    ultimately UNTRUSTED-at-the-root) chain for either leaf, so the wrong-CA case is
    refused as `ISSUER_NOT_TRUSTED` (a real chain that fails at the root) rather than
    failing earlier for the unrelated reason of "no matching intermediate at all" --
    mirroring `crates/av-ingest/tests/wire_identity.rs`'s own reasoning for deliberately
    configuring a matching chain hint for whichever leaf a given test presents."""
    work = tmp_path_factory.mktemp("combined_intermediates")
    dest = work / "combined_intermediates.pem"
    dest.write_text(provisioned.edge.issuer_pem.read_text() + provisioned.foreign.issuer_pem.read_text())
    return dest


# --------------------------------------------------------------------------- av-ingest-server subprocess

def _read_two_listen_lines(proc: subprocess.Popen, timeout_s: float) -> tuple[str, str]:
    """Reads exactly `av-ingest-server`'s own two `GRPC_LISTENING`/`ADMIN_LISTENING`
    stdout lines, off a background thread joined with a bounded timeout -- a bounded
    readiness wait, not a sleep-and-hope (question 199's sibling rule): the thread blocks
    on `readline()`, which returns the moment the subprocess actually writes and flushes,
    never on a fixed delay."""
    lines: list[str] = []

    def reader() -> None:
        for _ in range(2):
            line = proc.stdout.readline()
            if not line:
                return
            lines.append(line.rstrip("\n"))

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    t.join(timeout_s)
    if len(lines) < 2:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        stderr = proc.stderr.read() if proc.stderr else ""
        pytest.fail(f"av-ingest-server did not print its two listening lines within {timeout_s}s (got {lines})\n--- stderr ---\n{stderr}")
    grpc_line, admin_line = lines
    assert grpc_line.startswith("GRPC_LISTENING "), grpc_line
    assert admin_line.startswith("ADMIN_LISTENING "), admin_line
    return grpc_line.split(" ", 1)[1], admin_line.split(" ", 1)[1]


@contextlib.contextmanager
def _running_server(binary: Path, work: Path, *, root_pem: Path, chain_pem: Path, clock_args: list[str]):
    log_dir = work / "ingest_log"
    log_dir.mkdir(parents=True, exist_ok=True)
    args = [
        str(binary),
        "--grpc-bind", "127.0.0.1:0",
        "--admin-bind", "127.0.0.1:0",
        "--log-dir", str(log_dir),
        "--trust-anchor", str(root_pem),
        "--intermediate-chain", str(chain_pem),
        "--clearance-ladder", "UNCLASSIFIED,CUI",
        "--max-batch-age-ns", "5000000000",
        *clock_args,
    ]
    proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        grpc_addr, admin_addr = _read_two_listen_lines(proc, SERVER_READY_TIMEOUT_S)
        backend_port = int(grpc_addr.rsplit(":", 1)[1])
        admin_port = int(admin_addr.rsplit(":", 1)[1])
        yield SimpleNamespace(proc=proc, backend_port=backend_port, admin_port=admin_port)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


# --------------------------------------------------------------------------- nginx front

def _render_template(*, header_directive: str, work: Path, listen_port: int, server_cert: SimpleNamespace, client_ca_bundle: Path, backend_port: int) -> Path:
    rendered = TEMPLATE_PATH.read_text()
    substitutions = {
        "__PID_FILE__": str(work / "nginx.pid"),
        "__ERROR_LOG__": str(work / "error.log"),
        "__ACCESS_LOG__": str(work / "access.log"),
        "__CLIENT_BODY_TEMP__": str(work / "client_body_temp"),
        "__LISTEN_PORT__": str(listen_port),
        "__SERVER_NAME__": "127.0.0.1",
        "__SSL_CERTIFICATE__": str(server_cert.crt),
        "__SSL_CERTIFICATE_KEY__": str(server_cert.key),
        "__SSL_CLIENT_CERTIFICATE__": str(client_ca_bundle),
        "__BACKEND_PORT__": str(backend_port),
    }
    for token, value in substitutions.items():
        assert token in rendered, f"template is missing expected placeholder {token}"
        rendered = rendered.replace(token, value)
    remaining = [line for line in rendered.splitlines() if "__" in line and not line.strip().startswith("#")]
    assert not remaining, f"unsubstituted placeholder(s) left in rendered config: {remaining}"

    if header_directive != "grpc_set_header":
        # test_proxy_set_header_is_not_honoured_for_a_grpc_pass_location's own variant:
        # swap ONLY the forwarded-cert directive keyword, nothing else -- proving the rest
        # of the config is unaffected.
        assert "grpc_set_header x-ssl-client-escaped-cert" in rendered
        rendered = rendered.replace("grpc_set_header x-ssl-client-escaped-cert", f"{header_directive} x-ssl-client-escaped-cert")

    (work / "client_body_temp").mkdir(parents=True, exist_ok=True)
    conf_path = work / "nginx-av-ingest-grpc.conf"
    conf_path.write_text(rendered)
    return conf_path


@contextlib.contextmanager
def _running_nginx(conf_path: Path, listen_port: int, work: Path):
    _run([NGINX_BIN, "-c", str(conf_path), "-t"])
    proc = subprocess.Popen([NGINX_BIN, "-c", str(conf_path), "-g", "daemon off;"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        try:
            _wait_for_port(listen_port, NGINX_READY_TIMEOUT_S)
        except TimeoutError as e:
            proc.terminate()
            output = proc.stdout.read() if proc.stdout else ""
            log = (work / "error.log").read_text() if (work / "error.log").is_file() else ""
            pytest.fail(f"nginx did not start listening on 127.0.0.1:{listen_port}: {e}\n--- nginx stdout/stderr ---\n{output}\n--- error.log ---\n{log}")
        yield SimpleNamespace(port=listen_port, error_log=work / "error.log")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@contextlib.contextmanager
def _backend_and_front(work: Path, *, av_ingest_server_bin: Path, root_pem: Path, chain_pem: Path, clock_args: list[str], server_cert: SimpleNamespace, client_ca_bundle: Path, header_directive: str = "grpc_set_header"):
    """Starts a fresh `av-ingest-server` and a fresh nginx front for it, in this order (so
    nginx's own `grpc_pass` backend port is already known and listening before nginx ever
    starts), and tears both down -- nginx first, then the server -- even on failure."""
    with _running_server(av_ingest_server_bin, work / "server", root_pem=root_pem, chain_pem=chain_pem, clock_args=clock_args) as backend:
        listen_port = _free_port()
        conf_path = _render_template(header_directive=header_directive, work=work / "nginx", listen_port=listen_port, server_cert=server_cert, client_ca_bundle=client_ca_bundle, backend_port=backend.backend_port)
        with _running_nginx(conf_path, listen_port, work / "nginx") as front:
            yield SimpleNamespace(backend=backend, front=front)


# --------------------------------------------------------------------------- the mTLS client

def _run_mtls_client(binary: Path, *, endpoint: str, server_ca: Path, client_cert: Path | None = None, client_key: Path | None = None, producer_id: str, shard_key: str = "shard-a", clearance: str = "CUI", label_marking: str = "CUI", submit_with_key: Path | None = None, batch_tai_ns: int | None = None) -> dict:
    args = [str(binary), "--endpoint", endpoint, "--server-ca", str(server_ca), "--producer-id", producer_id, "--clearance", clearance, "--label-marking", label_marking, "--shard-key", shard_key]
    if client_cert is not None:
        args += ["--client-cert", str(client_cert), "--client-key", str(client_key)]
    if submit_with_key is not None:
        args += ["--submit-with-key", str(submit_with_key), "--batch-tai-ns", str(batch_tai_ns)]
    proc = subprocess.run(args, capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        pytest.fail(f"av-ingest-mtls-client failed (rc={proc.returncode})\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    assert proc.stdout.strip(), f"av-ingest-mtls-client produced no stdout\n--- stderr ---\n{proc.stderr}"
    return json.loads(proc.stdout.strip().splitlines()[-1])


def _get_evidence(admin_port: int) -> dict:
    import urllib.request

    with urllib.request.urlopen(f"http://127.0.0.1:{admin_port}/admin/api/evidence", timeout=5.0) as resp:
        return json.loads(resp.read().decode("utf-8"))


# =========================================================================================
# the proof
# =========================================================================================

def test_template_names_the_exact_forwarded_cert_header_literal():
    """Static, no-nginx-needed check that this template's `grpc_set_header` line names
    `av_ingest::forwarded_cert::FORWARDED_CLIENT_CERT_HEADER`'s ACTUAL literal (read off
    that file directly, never retyped from memory -- question 202's own instruction)."""
    rs_text = FORWARDED_CERT_RS.read_text()
    m = re.search(r'FORWARDED_CLIENT_CERT_HEADER:\s*&str\s*=\s*"([^"]+)"', rs_text)
    assert m, f"could not find FORWARDED_CLIENT_CERT_HEADER's literal in {FORWARDED_CERT_RS}"
    header_literal = m.group(1)

    template_text = TEMPLATE_PATH.read_text()
    assert f"grpc_set_header {header_literal} $ssl_client_escaped_cert;" in template_text, (
        f"template does not contain the expected 'grpc_set_header {header_literal} "
        f"$ssl_client_escaped_cert;' line naming the exact literal read from {FORWARDED_CERT_RS}"
    )


def test_valid_seccert_leaf_is_accepted_through_nginx_and_batches_submit(tmp_path, av_ingest_binaries, provisioned, combined_intermediates):
    """The positive case: a genuine seccert+lego-issued leaf (`edge.localhost`) reaches
    `Announce` AND `Submit` through the real nginx front, and the batch it signs is
    accepted -- proving the certificate the ingest received (after nginx's own escaping,
    a header hop, and this crate's own percent-decoding) still carries the SAME public key
    the leaf's own private key matches. Also captures and prints the raw forwarded-header
    evidence question 148 asks for."""
    _require_nginx()
    server_cert = _build_front_server_cert(tmp_path / "front_cert")
    client_fullchain = _fullchain(tmp_path / "edge_fullchain.pem", provisioned.edge.cert_pem, provisioned.edge.issuer_pem)

    with _backend_and_front(
        tmp_path,
        av_ingest_server_bin=av_ingest_binaries.server,
        root_pem=provisioned.seccert1.root_pem,
        chain_pem=combined_intermediates,
        clock_args=["--real-clock"],
        server_cert=server_cert,
        client_ca_bundle=provisioned.seccert1.trust_bundle_pem,
    ) as running:
        result = _run_mtls_client(
            av_ingest_binaries.client,
            endpoint=f"https://127.0.0.1:{running.front.port}",
            server_ca=server_cert.crt,
            client_cert=client_fullchain,
            client_key=provisioned.edge.key_pem,
            producer_id="edge-plugin-good",
            submit_with_key=provisioned.edge.key_pem,
            batch_tai_ns=now_tai_ns(),
        )
        assert result["announce"]["accepted"] is True, result
        assert len(result["submit_verdicts"]) == 1, result
        assert result["submit_verdicts"][0]["accepted"] is True, result
        assert result["submit_verdicts"][0]["rejection"] == "BATCH_REJECTION_UNSPECIFIED", result

        evidence = _get_evidence(running.backend.admin_port)
        assert evidence["identity"]["accepted"] == 1, evidence
        assert evidence["identity"]["issuer_not_trusted"] == 0, evidence
        assert evidence["identity"]["expired"] == 0, evidence
        assert evidence["accepted_total"] == 1, evidence

        # --- question 148's evidence: what nginx actually put on the wire, observed ---
        running.backend.proc.terminate()
        try:
            running.backend.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            running.backend.proc.kill()
            running.backend.proc.wait(timeout=10)
        stderr = running.backend.proc.stderr.read() if running.backend.proc.stderr else ""
        header_lines = [line for line in stderr.splitlines() if "received x-ssl-client-escaped-cert header" in line]
        assert header_lines, f"expected av-ingest-server's own stderr to log the received header at least once\n--- full stderr ---\n{stderr}"
        print("\n--- av-ingest-server stderr: observed forwarded-certificate header (question 148 evidence) ---")
        print(header_lines[0])
        assert "BEGIN" in header_lines[0] and "%0A" in header_lines[0], (
            f"expected nginx's real $ssl_client_escaped_cert to percent-encode the PEM's newlines as %0A "
            f"(letters/digits/-._~ pass through unescaped, per crates/av-ingest/src/forwarded_cert.rs's own "
            f"module doc) -- observed: {header_lines[0]!r}"
        )


def test_leaf_from_a_different_ca_is_refused_and_issuer_not_trusted_increments_in_process(tmp_path, av_ingest_binaries, provisioned, combined_intermediates):
    """The wrong-CA case: `foreign.localhost` (issued by an entirely independent second
    seccert instance/Root) is forwarded by nginx exactly like a trusted leaf would be
    (`ssl_verify_client optional_no_ca` -- see this module's own docstring), and it is
    `av-ingest`'s OWN `Announce` handler, not nginx, that refuses it -- and its refusal
    lands in `IdentityCounters.issuer_not_trusted`, read back via `GET /admin/api/
    evidence`, not only in nginx's error log."""
    _require_nginx()
    server_cert = _build_front_server_cert(tmp_path / "front_cert")
    foreign_fullchain = _fullchain(tmp_path / "foreign_fullchain.pem", provisioned.foreign.cert_pem, provisioned.foreign.issuer_pem)

    with _backend_and_front(
        tmp_path,
        av_ingest_server_bin=av_ingest_binaries.server,
        root_pem=provisioned.seccert1.root_pem,  # the TRUSTED root -- deliberately NOT seccert2's
        chain_pem=combined_intermediates,
        clock_args=["--real-clock"],
        server_cert=server_cert,
        client_ca_bundle=provisioned.seccert1.trust_bundle_pem,
    ) as running:
        result = _run_mtls_client(
            av_ingest_binaries.client,
            endpoint=f"https://127.0.0.1:{running.front.port}",
            server_ca=server_cert.crt,
            client_cert=foreign_fullchain,
            client_key=provisioned.foreign.key_pem,
            producer_id="edge-plugin-wrong-ca",
        )
        # The RPC itself must succeed (the TLS handshake completed, the request reached
        # av-ingest, and it answered) -- the refusal is IN the ManifestAck, not a
        # transport failure. This is the load-bearing proof that nginx did NOT reject the
        # connection (ssl_verify_client optional_no_ca, not on, and not plain optional).
        assert result["announce"]["accepted"] is False, result
        assert result["announce"]["refusal"] == "MANIFEST_REFUSAL_IDENTITY_REFUSED", result

        evidence = _get_evidence(running.backend.admin_port)
        assert evidence["identity"]["issuer_not_trusted"] == 1, evidence
        assert evidence["identity"]["accepted"] == 0, evidence
        assert evidence["identity"]["expired"] == 0, evidence


def test_lapsed_leaf_is_refused_and_expired_increments_using_the_injected_clock(tmp_path, av_ingest_binaries, provisioned, combined_intermediates):
    """The lapsed case: `short.localhost` (a genuine, one-minute-lifetime seccert+lego
    leaf) is presented well past its real `notAfter` -- proven by starting
    `av-ingest-server` with `--clock-tai-ns` FIXED at a value 30s after that leaf's own,
    actually-issued `notAfter` (read directly off the certificate with `openssl -enddate`),
    never by sleeping and never by waiting for a short-lived certificate to actually
    expire."""
    _require_nginx()
    server_cert = _build_front_server_cert(tmp_path / "front_cert")
    short_fullchain = _fullchain(tmp_path / "short_fullchain.pem", provisioned.short.cert_pem, provisioned.short.issuer_pem)

    enddate = _openssl(["x509", "-in", str(provisioned.short.cert_pem), "-noout", "-enddate"], tmp_path).stdout.strip()
    assert enddate.startswith("notAfter=")
    not_after = parsedate_to_datetime(enddate[len("notAfter="):])
    lapsed_clock_tai_ns = unix_to_tai_ns((not_after + timedelta(seconds=30)).timestamp())

    with _backend_and_front(
        tmp_path,
        av_ingest_server_bin=av_ingest_binaries.server,
        root_pem=provisioned.seccert1.root_pem,
        chain_pem=combined_intermediates,
        clock_args=["--clock-tai-ns", str(lapsed_clock_tai_ns)],
        server_cert=server_cert,
        client_ca_bundle=provisioned.seccert1.trust_bundle_pem,
    ) as running:
        result = _run_mtls_client(
            av_ingest_binaries.client,
            endpoint=f"https://127.0.0.1:{running.front.port}",
            server_ca=server_cert.crt,
            client_cert=short_fullchain,
            client_key=provisioned.short.key_pem,
            producer_id="edge-plugin-lapsed",
        )
        assert result["announce"]["accepted"] is False, result
        assert result["announce"]["refusal"] == "MANIFEST_REFUSAL_IDENTITY_REFUSED", result

        evidence = _get_evidence(running.backend.admin_port)
        assert evidence["identity"]["expired"] == 1, evidence
        assert evidence["identity"]["accepted"] == 0, evidence
        assert evidence["identity"]["issuer_not_trusted"] == 0, evidence


def test_no_client_certificate_is_refused_by_the_ingest_not_nginx(tmp_path, av_ingest_binaries, provisioned, combined_intermediates):
    """No client certificate at all. Under `ssl_verify_client optional_no_ca`, the TLS handshake
    itself must still complete (proven by the RPC succeeding as a well-formed, answered
    gRPC call rather than a transport error) -- nginx is NOT the layer that refuses this;
    `av-ingest`'s own `Announce` handler is, because no forwarded-certificate header (or an
    empty one -- see below) ever gives it anything to verify."""
    _require_nginx()
    server_cert = _build_front_server_cert(tmp_path / "front_cert")

    with _backend_and_front(
        tmp_path,
        av_ingest_server_bin=av_ingest_binaries.server,
        root_pem=provisioned.seccert1.root_pem,
        chain_pem=combined_intermediates,
        clock_args=["--real-clock"],
        server_cert=server_cert,
        client_ca_bundle=provisioned.seccert1.trust_bundle_pem,
    ) as running:
        result = _run_mtls_client(
            av_ingest_binaries.client,
            endpoint=f"https://127.0.0.1:{running.front.port}",
            server_ca=server_cert.crt,
            # deliberately no --client-cert / --client-key
            producer_id="edge-plugin-no-cert",
        )
        assert result["announce"]["accepted"] is False, result
        assert result["announce"]["refusal"] == "MANIFEST_REFUSAL_IDENTITY_REFUSED", result

        evidence = _get_evidence(running.backend.admin_port)
        # Two honest possibilities, both refused IN PROCESS (never by nginx): nginx's own
        # `$ssl_client_escaped_cert` is empty when no cert was presented, and
        # `grpc_set_header` (documented, like `proxy_set_header`, to drop an empty-valued
        # header rather than forward it) either omits the header entirely -- in which case
        # NO identity counter moves at all, since crate::service::announce's own
        # "no header present" branch returns before ever calling verify_identity -- or (if
        # that documented behaviour differs in practice) forwards an EMPTY header, which
        # decodes to zero bytes and is refused as MalformedPem. Printed, not guessed.
        if evidence["identity"]["malformed_pem"] == 1:
            print("\nobserved: nginx forwarded an EMPTY x-ssl-client-escaped-cert header; av-ingest refused it as MALFORMED_PEM (identity.malformed_pem == 1)")
            assert evidence["identity"]["accepted"] == 0
        else:
            print("\nobserved: nginx did NOT forward the x-ssl-client-escaped-cert header at all when no client certificate was presented (identity counters are all still zero)")
            assert evidence["identity"] == {"accepted": 0, "malformed_pem": 0, "not_p384": 0, "issuer_not_trusted": 0, "expired": 0, "not_yet_valid": 0, "openssl_error": 0}, evidence


def test_proxy_set_header_is_not_honoured_for_a_grpc_pass_location_only_grpc_set_header_is(tmp_path, av_ingest_binaries, provisioned, combined_intermediates):
    """Measures, rather than assumes, which directive a `grpc_pass` location actually
    honours (the template's own header comment's claim). Renders a variant of the SAME
    template with its one `grpc_set_header` line swapped for `proxy_set_header`
    (`ngx_http_proxy_module`'s own directive, for `proxy_pass`, not `grpc_pass`) and
    drives a request with a VALID client certificate through it: `nginx -t` accepts this
    variant with no complaint (an unused directive from a different module is not a
    config error), but the header never reaches the backend at all."""
    _require_nginx()
    server_cert = _build_front_server_cert(tmp_path / "front_cert")
    client_fullchain = _fullchain(tmp_path / "edge_fullchain.pem", provisioned.edge.cert_pem, provisioned.edge.issuer_pem)

    with _backend_and_front(
        tmp_path,
        av_ingest_server_bin=av_ingest_binaries.server,
        root_pem=provisioned.seccert1.root_pem,
        chain_pem=combined_intermediates,
        clock_args=["--real-clock"],
        server_cert=server_cert,
        client_ca_bundle=provisioned.seccert1.trust_bundle_pem,
        header_directive="proxy_set_header",
    ) as running:
        result = _run_mtls_client(
            av_ingest_binaries.client,
            endpoint=f"https://127.0.0.1:{running.front.port}",
            server_ca=server_cert.crt,
            client_cert=client_fullchain,
            client_key=provisioned.edge.key_pem,
            producer_id="edge-plugin-proxy-set-header-variant",
        )
        # A VALID, trusted leaf -- if the header had actually arrived, this would be
        # accepted (see test_valid_seccert_leaf_...). Refused instead: proxy_set_header
        # did not forward it under grpc_pass.
        assert result["announce"]["accepted"] is False, result
        assert result["announce"]["refusal"] == "MANIFEST_REFUSAL_IDENTITY_REFUSED", result
        evidence = _get_evidence(running.backend.admin_port)
        # Same "no header at all" vs "empty header" split as the no-cert case, printed for
        # the record -- either way, this leaf's own trust/validity was never even reached.
        print(f"\nproxy_set_header variant: identity counters after presenting a VALID leaf = {evidence['identity']}")
        assert evidence["identity"]["accepted"] == 0, evidence
        assert evidence["identity"]["issuer_not_trusted"] == 0, evidence
        assert evidence["identity"]["expired"] == 0, evidence
