"""M5.3: TLS for gmat-service's gRPC, the suite's way (ADR-004).

`grpcio` (what `services/gmat-service` is built on) bundles BoringSSL, so gmat-service
itself stays **plaintext on loopback** (unchanged from M2.2 -- see that service's
README). TLS, including **mandatory client-certificate verification (mTLS)**, is
terminated in front of it by an nginx `grpc_pass` reverse proxy configured the way
`secproxy` configures everything else in the SecRouter suite: nginx linking the host's
OpenSSL, a server certificate, and `ssl_verify_client on` against a seccert-style CA chain
(`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`).

This test proves the whole path end to end, locally:

1. builds a local, throwaway **two-tier CA** (root + intermediate, **ECDSA P-384**, via
   the Homebrew OpenSSL 3.6.3 CLI only -- no Python crypto library touches key material
   anywhere in this file);
2. issues a server certificate (SAN `IP:127.0.0.1`, since there is no DNS here) and a
   client certificate from that CA;
3. starts gmat-service in its normal plaintext-on-loopback mode;
4. renders the deploy template above with those certs/ports and starts nginx in front of
   it;
5. drives `crates/av-grpc`'s `describe_client` Rust binary -- a `tonic` client wired onto
   an **OpenSSL-backed** connector (`hyper-openssl`), deliberately avoiding tonic's own
   `tls`/`tls-native-roots`/`tls-webpki-roots` features because all three pull in
   `ring` (forbidden by ADR-004) -- through the proxy **with** the client certificate
   (expected: `Describe` succeeds) and **without** one (expected: refused -- see the
   note on the actual refusal mechanism below, it is not what you'd first guess).

Measured refusal mechanism (worth stating precisely rather than assuming): with
`ssl_verify_client on`, nginx does **not** abort the TLS handshake itself when the client
presents no certificate -- OpenSSL completes it (`SSL_VERIFY_PEER` without
`SSL_VERIFY_FAIL_IF_NO_PEER_CERT`), and nginx then rejects the *HTTP request* over that
connection with a plain `400 Bad Request` before it ever reaches `grpc_pass`/gmat-service
(`nginx error.log`: "client sent no required SSL certificate while reading client request
headers"). The Rust `tonic` client sees this as an RPC-level failure (h2 chokes trying to
parse the HTML error body as a gRPC frame), which is exactly as hard a refusal from the
caller's point of view -- no client identity, no RPC -- just enforced one layer up from
where a first guess ("the handshake itself fails") would put it.

Skips (not xfails) with a clear reason if nginx isn't on this machine -- `nginx IS present
in the target environment, so in practice this always runs; the skip path exists only for
a machine that genuinely lacks it, not as a way to dodge a real failure here.

**M6.2 addition**: the back half of this file (`test_describe_succeeds_through_proxy_with_client_cert_against_rust_backend`
onward) proves the identical mTLS-through-nginx path in front of `crates/av-dynamics-service`'s
own Rust `DynamicsService` server (ADR-002 depth 2, `"gmat-ffi"`) instead of `gmat-service` --
same template, same test-CA machinery, same `describe_client` binary, a different
plaintext backend and a different `depth`/`settings_hash` in the response. This is the
proof named in `docs/adr/003-substrate-and-deployment.md`'s 2026-09-02 amendment that the
Rust-hosted service (not just the Python one) answers an `av-grpc` client through the same
service-owned nginx front.
"""
from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import textwrap
import time
from pathlib import Path
from types import SimpleNamespace

import grpc
import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SERVICE_DIR = REPO_ROOT / "services" / "gmat-service"
DEPLOY_DIR = SERVICE_DIR / "deploy"
TEMPLATE_PATH = DEPLOY_DIR / "nginx-gmat-grpc.conf.template"

NGINX_BIN = "/opt/homebrew/bin/nginx"
OPENSSL_BIN = "/opt/homebrew/opt/openssl@3/bin/openssl"  # Homebrew OpenSSL 3.6.3, NOT
# /usr/bin/openssl (LibreSSL 3.3.6 on this machine -- documented to behave differently
# for ECDSA P-384; see this task's own environment notes).

READY_TIMEOUT_S = 90.0
NGINX_READY_TIMEOUT_S = 10.0
MODEL_ID = "gmat.earth.jgm2_8x8.sun_moon"

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
OPENSSL_DIR = "/opt/homebrew/opt/openssl@3"  # openssl-sys links THIS libssl/libcrypto,
# never a vendored/bundled copy -- see services/gmat-service/README.md's FIPS accounting.


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    env["OPENSSL_DIR"] = OPENSSL_DIR
    return env


def _free_port() -> int:
    """An ephemeral localhost port, free at the moment of the check (same trick, same
    small unavoidable race, as tests/test_gmat_service.py's `_free_port`)."""
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
            time.sleep(0.1)
    raise TimeoutError(f"127.0.0.1:{port} never accepted a connection: {last_err}")


def _run(args: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess:
    proc = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if proc.returncode != 0:
        pytest.fail(
            f"command failed (rc={proc.returncode}): {' '.join(args)}\n"
            f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return proc


# --------------------------------------------------------------------------- nginx presence
def _require_nginx() -> None:
    if not Path(NGINX_BIN).is_file():
        pytest.skip(f"nginx not found at {NGINX_BIN} -- skipping the mTLS proxy test "
                    "(gmat-service's own plaintext-on-loopback behavior is unaffected "
                    "and covered by tests/test_gmat_service.py).")


# --------------------------------------------------------------------------- the test CA
def _openssl(args: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return _run([OPENSSL_BIN, *args], cwd=cwd)


def _ecdsa_p384_key(cwd: Path, out: str) -> None:
    _openssl(["ecparam", "-name", "secp384r1", "-genkey", "-noout", "-out", out], cwd)


def _build_test_ca(cwd: Path) -> SimpleNamespace:
    """A local, throwaway two-tier ECDSA P-384 CA (root + intermediate), plus a server
    leaf (SAN IP:127.0.0.1 -- there is no DNS in this environment) and a client leaf, all
    issued with the Homebrew OpenSSL 3.6.3 CLI. Mirrors ADR-004's own algorithm choice for
    the edge's signing key (P-384) and its two-tier structure (root as the enclave trust
    anchor, short-lived leaves from an intermediate)."""
    # --- root -----------------------------------------------------------------
    _ecdsa_p384_key(cwd, "root.key")
    _openssl([
        "req", "-x509", "-new", "-key", "root.key", "-sha384", "-days", "3650",
        "-subj", "/CN=AltaVista M5.3 Test Root CA",
        "-addext", "basicConstraints=critical,CA:TRUE,pathlen:1",
        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        "-addext", "subjectKeyIdentifier=hash",
        "-out", "root.crt",
    ], cwd)

    # --- intermediate -----------------------------------------------------------
    _ecdsa_p384_key(cwd, "intermediate.key")
    _openssl([
        "req", "-new", "-key", "intermediate.key",
        "-subj", "/CN=AltaVista M5.3 Test Intermediate CA",
        "-out", "intermediate.csr",
    ], cwd)
    (cwd / "intermediate.ext").write_text(textwrap.dedent("""\
        basicConstraints = critical,CA:TRUE,pathlen:0
        keyUsage = critical,keyCertSign,cRLSign
        subjectKeyIdentifier = hash
        authorityKeyIdentifier = keyid:always,issuer
        """))
    _openssl([
        "x509", "-req", "-in", "intermediate.csr",
        "-CA", "root.crt", "-CAkey", "root.key", "-CAcreateserial",
        "-days", "1825", "-sha384", "-extfile", "intermediate.ext",
        "-out", "intermediate.crt",
    ], cwd)

    # --- server leaf --------------------------------------------------------------
    _ecdsa_p384_key(cwd, "server.key")
    _openssl(["req", "-new", "-key", "server.key", "-subj", "/CN=127.0.0.1",
              "-out", "server.csr"], cwd)
    (cwd / "server.ext").write_text(textwrap.dedent("""\
        basicConstraints = critical,CA:FALSE
        keyUsage = critical,digitalSignature
        extendedKeyUsage = serverAuth
        subjectAltName = IP:127.0.0.1,DNS:localhost
        subjectKeyIdentifier = hash
        authorityKeyIdentifier = keyid:always,issuer
        """))
    _openssl([
        "x509", "-req", "-in", "server.csr",
        "-CA", "intermediate.crt", "-CAkey", "intermediate.key", "-CAcreateserial",
        "-days", "825", "-sha384", "-extfile", "server.ext", "-out", "server.crt",
    ], cwd)
    (cwd / "server_fullchain.pem").write_text(
        (cwd / "server.crt").read_text() + (cwd / "intermediate.crt").read_text())

    # --- client leaf --------------------------------------------------------------
    _ecdsa_p384_key(cwd, "client.key")
    _openssl(["req", "-new", "-key", "client.key", "-subj", "/CN=av-grpc-test-client",
              "-out", "client.csr"], cwd)
    (cwd / "client.ext").write_text(textwrap.dedent("""\
        basicConstraints = critical,CA:FALSE
        keyUsage = critical,digitalSignature
        extendedKeyUsage = clientAuth
        subjectKeyIdentifier = hash
        authorityKeyIdentifier = keyid:always,issuer
        """))
    _openssl([
        "x509", "-req", "-in", "client.csr",
        "-CA", "intermediate.crt", "-CAkey", "intermediate.key", "-CAcreateserial",
        "-days", "825", "-sha384", "-extfile", "client.ext", "-out", "client.crt",
    ], cwd)
    (cwd / "client_fullchain.pem").write_text(
        (cwd / "client.crt").read_text() + (cwd / "intermediate.crt").read_text())

    # nginx's client-cert trust anchor: the issuing chain above the leaf.
    (cwd / "client_ca_bundle.pem").write_text(
        (cwd / "intermediate.crt").read_text() + (cwd / "root.crt").read_text())

    # Sanity-check the chains this test is about to rely on (fails loudly here, with
    # OpenSSL's own diagnosis, rather than as a confusing TLS handshake error later).
    _openssl(["verify", "-CAfile", "root.crt", "-untrusted", "intermediate.crt",
              "server.crt"], cwd)
    _openssl(["verify", "-CAfile", "root.crt", "-untrusted", "intermediate.crt",
              "client.crt"], cwd)

    return SimpleNamespace(
        root_crt=cwd / "root.crt",
        server_fullchain=cwd / "server_fullchain.pem",
        server_key=cwd / "server.key",
        client_ca_bundle=cwd / "client_ca_bundle.pem",
        client_fullchain=cwd / "client_fullchain.pem",
        client_key=cwd / "client.key",
    )


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def describe_client_bin():
    """Builds crates/av-grpc's `describe_client` once for the module. Asserts the build
    actually succeeds -- a build failure here is a real failure of this task, not
    something to paper over by skipping."""
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-grpc", "--bin", "describe_client"],
        cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True,
        timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-grpc failed:\n--- stdout ---\n{proc.stdout}\n"
                     f"--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "describe_client"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def gmat_service_backend(tmp_path_factory):
    """gmat-service, unchanged from M2.2: plaintext gRPC on loopback only. This is the
    same subprocess pattern tests/test_gmat_service.py uses (its own fixture, not shared,
    since that test file belongs to a different worker's tree)."""
    port = _free_port()
    evidence_path = tmp_path_factory.mktemp("gmat_service_tls") / "evidence.jsonl"
    proc = subprocess.Popen(
        [sys.executable, "-m", "gmat_service", "--port", str(port),
         "--evidence-path", str(evidence_path), "--run-id", "test_grpc_tls"],
        cwd=str(SERVICE_DIR), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    channel = grpc.insecure_channel(f"127.0.0.1:{port}")
    try:
        try:
            grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
        except Exception as e:
            channel.close()
            output = ""
            try:
                if proc.stdout is not None:
                    output = proc.stdout.read()
            except Exception:
                pass
            pytest.fail(
                f"gmat-service subprocess did not become ready within "
                f"{READY_TIMEOUT_S}s: {e}\n--- subprocess output ---\n{output}")
        channel.close()
        yield port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@pytest.fixture(scope="module")
def nginx_mtls_proxy(tmp_path_factory, gmat_service_backend):
    """Renders the deploy template with a fresh test CA and starts nginx in front of
    gmat-service. Yields (listen_port, ca) -- `ca` is the SimpleNamespace of cert paths
    from `_build_test_ca`."""
    _require_nginx()

    work = tmp_path_factory.mktemp("nginx_mtls")
    ca = _build_test_ca(work)

    listen_port = _free_port()
    rendered = TEMPLATE_PATH.read_text()
    substitutions = {
        "__PID_FILE__": str(work / "nginx.pid"),
        "__ERROR_LOG__": str(work / "error.log"),
        "__ACCESS_LOG__": str(work / "access.log"),
        "__CLIENT_BODY_TEMP__": str(work / "client_body_temp"),
        "__LISTEN_PORT__": str(listen_port),
        "__SERVER_NAME__": "127.0.0.1",
        "__SSL_CERTIFICATE__": str(ca.server_fullchain),
        "__SSL_CERTIFICATE_KEY__": str(ca.server_key),
        "__SSL_CLIENT_CERTIFICATE__": str(ca.client_ca_bundle),
        "__BACKEND_PORT__": str(gmat_service_backend),
    }
    for token, value in substitutions.items():
        assert token in rendered, f"template is missing expected placeholder {token}"
        rendered = rendered.replace(token, value)
    remaining = [line for line in rendered.splitlines()
                 if "__" in line and not line.strip().startswith("#")]
    assert not remaining, f"unsubstituted placeholder(s) left in rendered config: {remaining}"

    (work / "client_body_temp").mkdir(parents=True, exist_ok=True)
    conf_path = work / "nginx-gmat-grpc.conf"
    conf_path.write_text(rendered)

    # `nginx -t` first: a config mistake should fail here with nginx's own diagnosis, not
    # as a mysterious connection refusal from the Rust client later.
    _run([NGINX_BIN, "-c", str(conf_path), "-t"])

    proc = subprocess.Popen(
        [NGINX_BIN, "-c", str(conf_path), "-g", "daemon off;"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        try:
            _wait_for_port(listen_port, NGINX_READY_TIMEOUT_S)
        except TimeoutError as e:
            proc.terminate()
            output = ""
            try:
                if proc.stdout is not None:
                    output = proc.stdout.read()
            except Exception:
                pass
            log = ""
            try:
                log = (work / "error.log").read_text()
            except Exception:
                pass
            pytest.fail(f"nginx did not start listening on 127.0.0.1:{listen_port}: {e}\n"
                        f"--- nginx stdout/stderr ---\n{output}\n"
                        f"--- nginx error.log ---\n{log}")
        yield SimpleNamespace(port=listen_port, ca=ca, error_log=work / "error.log")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def _run_describe_client(binary: Path, *, endpoint: str, ca_file: Path,
                          client_cert: Path | None = None,
                          client_key: Path | None = None) -> subprocess.CompletedProcess:
    args = [str(binary), "--endpoint", endpoint, "--ca", str(ca_file)]
    if client_cert is not None:
        args += ["--client-cert", str(client_cert)]
    if client_key is not None:
        args += ["--client-key", str(client_key)]
    return subprocess.run(args, capture_output=True, text=True, timeout=30)


# --------------------------------------------------------------------------- the proof
def test_describe_succeeds_through_proxy_with_client_cert(nginx_mtls_proxy, describe_client_bin):
    """The positive case: a `tonic` client, TLS via `hyper-openssl` (OpenSSL, not `ring`),
    presenting the mTLS client certificate, reaches `Describe` through the nginx front."""
    proc = _run_describe_client(
        describe_client_bin,
        endpoint=f"https://127.0.0.1:{nginx_mtls_proxy.port}",
        ca_file=nginx_mtls_proxy.ca.root_crt,
        client_cert=nginx_mtls_proxy.ca.client_fullchain,
        client_key=nginx_mtls_proxy.ca.client_key,
    )
    assert proc.returncode == 0, (
        f"describe_client (with client cert) should succeed through the mTLS proxy but "
        f"exited {proc.returncode}\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    print("\n--- describe_client stdout (with client cert) ---\n" + proc.stdout)
    assert f"OK id={MODEL_ID}" in proc.stdout, proc.stdout
    assert "depth=gmat-api" in proc.stdout, proc.stdout


def test_refused_without_client_cert(nginx_mtls_proxy, describe_client_bin):
    """The negative case, equally load-bearing: the SAME endpoint, same trusted root, but
    no client certificate at all -- `ssl_verify_client on` must refuse the request. As
    measured (see this module's docstring), the TLS handshake itself completes and nginx
    rejects the HTTP request over it with 400 before gmat-service is ever reached; either
    way the RPC never succeeds and no client identity was accepted."""
    proc = _run_describe_client(
        describe_client_bin,
        endpoint=f"https://127.0.0.1:{nginx_mtls_proxy.port}",
        ca_file=nginx_mtls_proxy.ca.root_crt,
        # deliberately no --client-cert / --client-key
    )
    assert proc.returncode != 0, (
        "describe_client with NO client certificate must be refused by "
        f"ssl_verify_client on, but it exited 0\n--- stdout ---\n{proc.stdout}")
    # Evidence for the report: the exact client-side refusal, plus nginx's own view of it.
    error_log_tail = ""
    try:
        error_log_tail = nginx_mtls_proxy.error_log.read_text()[-4000:]
    except Exception:
        pass
    print("\n--- describe_client stderr (no client cert) ---\n" + proc.stderr)
    print("\n--- nginx error.log (tail) ---\n" + error_log_tail)
    assert proc.stderr.strip(), "expected a non-empty error on stderr for the refused connection"


# =========================================================================================
# M6.2: the identical proof, against crates/av-dynamics-service's Rust DynamicsService
# server (ADR-002 depth 2, "gmat-ffi") instead of gmat-service. Same template
# (services/gmat-service/deploy/nginx-gmat-grpc.conf.template -- ADR-003's amendment names
# no separate template for this; the front is per-plaintext-gRPC-backend, not per-language),
# same test-CA machinery, same describe_client binary; a fresh CA and listen port so this
# proxy instance never shares state with `nginx_mtls_proxy` above. Deliberately NOT
# refactored to share fixtures with the gmat-service half of this file: both halves are
# independently readable, and neither risks the other if one is edited later.
# =========================================================================================

AV_DYNAMICS_SERVICE_DIR = REPO_ROOT / "crates" / "av-dynamics-service"


@pytest.fixture(scope="module")
def av_dynamics_service_bin():
    """Builds crates/av-dynamics-service's `av-dynamics-service` server binary once for the
    module. A build failure here is a real failure of this task, not something to skip."""
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
def av_dynamics_service_backend(av_dynamics_service_bin, tmp_path_factory):
    """The Rust DynamicsService server, plaintext gRPC on loopback only -- exactly the same
    posture as `gmat_service_backend` above, a different process/language behind it."""
    port = _free_port()
    evidence_path = tmp_path_factory.mktemp("av_dynamics_service_tls") / "evidence.jsonl"
    proc = subprocess.Popen(
        [str(av_dynamics_service_bin), "--port", str(port), "--evidence-path", str(evidence_path),
         "--run-id", "test_grpc_tls_rust"],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    channel = grpc.insecure_channel(f"127.0.0.1:{port}")
    try:
        try:
            grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
        except Exception as e:
            channel.close()
            output = ""
            try:
                if proc.stdout is not None:
                    output = proc.stdout.read()
            except Exception:
                pass
            pytest.fail(
                f"av-dynamics-service subprocess did not become ready within "
                f"{READY_TIMEOUT_S}s: {e}\n--- subprocess output ---\n{output}")
        channel.close()
        yield port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@pytest.fixture(scope="module")
def nginx_mtls_proxy_rust(tmp_path_factory, av_dynamics_service_backend):
    """Same rendering/start sequence as `nginx_mtls_proxy`, pointed at the Rust backend
    instead, with its own fresh test CA and listen port."""
    _require_nginx()

    work = tmp_path_factory.mktemp("nginx_mtls_rust")
    ca = _build_test_ca(work)

    listen_port = _free_port()
    rendered = TEMPLATE_PATH.read_text()
    substitutions = {
        "__PID_FILE__": str(work / "nginx.pid"),
        "__ERROR_LOG__": str(work / "error.log"),
        "__ACCESS_LOG__": str(work / "access.log"),
        "__CLIENT_BODY_TEMP__": str(work / "client_body_temp"),
        "__LISTEN_PORT__": str(listen_port),
        "__SERVER_NAME__": "127.0.0.1",
        "__SSL_CERTIFICATE__": str(ca.server_fullchain),
        "__SSL_CERTIFICATE_KEY__": str(ca.server_key),
        "__SSL_CLIENT_CERTIFICATE__": str(ca.client_ca_bundle),
        "__BACKEND_PORT__": str(av_dynamics_service_backend),
    }
    for token, value in substitutions.items():
        assert token in rendered, f"template is missing expected placeholder {token}"
        rendered = rendered.replace(token, value)
    remaining = [line for line in rendered.splitlines()
                 if "__" in line and not line.strip().startswith("#")]
    assert not remaining, f"unsubstituted placeholder(s) left in rendered config: {remaining}"

    (work / "client_body_temp").mkdir(parents=True, exist_ok=True)
    conf_path = work / "nginx-gmat-grpc.conf"
    conf_path.write_text(rendered)

    _run([NGINX_BIN, "-c", str(conf_path), "-t"])

    proc = subprocess.Popen(
        [NGINX_BIN, "-c", str(conf_path), "-g", "daemon off;"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        try:
            _wait_for_port(listen_port, NGINX_READY_TIMEOUT_S)
        except TimeoutError as e:
            proc.terminate()
            output = ""
            try:
                if proc.stdout is not None:
                    output = proc.stdout.read()
            except Exception:
                pass
            log = ""
            try:
                log = (work / "error.log").read_text()
            except Exception:
                pass
            pytest.fail(f"nginx did not start listening on 127.0.0.1:{listen_port}: {e}\n"
                        f"--- nginx stdout/stderr ---\n{output}\n"
                        f"--- nginx error.log ---\n{log}")
        yield SimpleNamespace(port=listen_port, ca=ca, error_log=work / "error.log")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def test_describe_succeeds_through_proxy_with_client_cert_against_rust_backend(
        nginx_mtls_proxy_rust, describe_client_bin):
    """The M6.2 positive case: the same `av-grpc` `describe_client` (OpenSSL via
    `hyper-openssl`, never `ring`) proving mTLS through the same nginx front, this time
    reaching `crates/av-dynamics-service`'s Rust server instead of gmat-service. `depth`
    is `"gmat-ffi"` here (ADR-002 depth 2), not gmat-service's `"gmat-api"` -- see
    `crates/av-dynamics-service/README.md`."""
    proc = _run_describe_client(
        describe_client_bin,
        endpoint=f"https://127.0.0.1:{nginx_mtls_proxy_rust.port}",
        ca_file=nginx_mtls_proxy_rust.ca.root_crt,
        client_cert=nginx_mtls_proxy_rust.ca.client_fullchain,
        client_key=nginx_mtls_proxy_rust.ca.client_key,
    )
    assert proc.returncode == 0, (
        f"describe_client (with client cert) should succeed through the mTLS proxy "
        f"fronting av-dynamics-service but exited {proc.returncode}\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    print("\n--- describe_client stdout (Rust backend, with client cert) ---\n" + proc.stdout)
    assert f"OK id={MODEL_ID}" in proc.stdout, proc.stdout
    assert "depth=gmat-ffi" in proc.stdout, proc.stdout


def test_refused_without_client_cert_against_rust_backend(nginx_mtls_proxy_rust, describe_client_bin):
    """The M6.2 negative case: same refusal (`ssl_verify_client on`, no client cert) proven
    against the Rust backend's own proxy instance."""
    proc = _run_describe_client(
        describe_client_bin,
        endpoint=f"https://127.0.0.1:{nginx_mtls_proxy_rust.port}",
        ca_file=nginx_mtls_proxy_rust.ca.root_crt,
        # deliberately no --client-cert / --client-key
    )
    assert proc.returncode != 0, (
        "describe_client with NO client certificate must be refused by "
        f"ssl_verify_client on (Rust backend), but it exited 0\n--- stdout ---\n{proc.stdout}")
    error_log_tail = ""
    try:
        error_log_tail = nginx_mtls_proxy_rust.error_log.read_text()[-4000:]
    except Exception:
        pass
    print("\n--- describe_client stderr (Rust backend, no client cert) ---\n" + proc.stderr)
    print("\n--- nginx error.log (tail) ---\n" + error_log_tail)
    assert proc.stderr.strip(), "expected a non-empty error on stderr for the refused connection"
