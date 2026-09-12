#!/usr/bin/env python3
"""Bring up the real local CA loop for `docs/edge-plan.md` milestone E2: seccert (RFC
8555 ACME CA) + lego (a standard ACME client), issuing genuine seccert leaves the way
`tests/test_edge_identity_seccert.py` drives against `av-edge-identity` end to end.

This is the manager's own probed recipe (`docs/edge-plan.md`'s "What your manager already
proved on this host"), turned into an importable module rather than re-derived by hand in
the test file:

- seccert (`/Users/probe/code/secdeploy/work/seccert`) is configured **entirely by
  `SECCERT_*` environment variables passed to that subprocess** via `subprocess.Popen`'s
  own `env=` argument -- this module never mutates `os.environ` of the calling (pytest)
  process itself (question 199).
- lego 5.4.1 refuses a plain-http ACME server, so seccert runs with
  `SECCERT_TLS_MODE=native` and self-issues its own serving certificate from the
  Intermediate on first boot.
- lego is pointed at that serving certificate's trust chain (Root + Intermediate,
  concatenated) via `LEGO_CA_CERTIFICATES`, passed the same way (`env=` on the lego
  subprocess) -- never `--tls-skip-verify`.
- lego 5.x puts every flag after the `run` subcommand.
- seccert validates http-01 by connecting to the domain on `SECCERT_HTTP01_PORT`, not
  whatever port lego was told to listen on -- the two must be the same number, so two
  leaves from the *same* seccert instance are issued sequentially on that one port, never
  concurrently. A second, independent seccert instance (its own data directory and ports)
  has no such constraint with the first.
- `--not-after <RFC3339>` on `lego run` is honoured by seccert, which is how
  [`issue_leaf`] produces a genuinely short-lived leaf (`short.localhost`) without ever
  sleeping to observe it expire.

Every port this module binds is chosen with [`free_port`] (an ephemeral port that is free
*at the moment of the check* -- the same small, unavoidable, well-understood race every
other test fixture in this repository accepts, e.g. `tests/test_grpc_tls.py::_free_port`),
never hardcoded.
"""
from __future__ import annotations

import os
import socket
import ssl
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parents[1]
SECCERT_DIR = Path("/Users/probe/code/secdeploy/work/seccert")
LEGO_BIN = "/opt/homebrew/bin/lego"

READY_TIMEOUT_S = 30.0
LEGO_TIMEOUT_S = 60.0
SHORT_LEAF_LIFETIME = timedelta(minutes=1)


# --------------------------------------------------------------------------- availability

def check_seccert_available(seccert_dir: Path = SECCERT_DIR) -> Optional[str]:
    """`None` if seccert's own venv looks usable; otherwise a reason string naming exactly
    what is missing and where it was looked for (question 194: a visible, specific skip
    reason, never a silent pass)."""
    python_bin = seccert_dir / ".venv" / "bin" / "python"
    if not python_bin.is_file():
        return f"seccert venv python not found at {python_bin} (expected a venv created once under {seccert_dir})"
    proc = subprocess.run([str(python_bin), "-c", "import seccert"], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        return f"{python_bin} cannot import the seccert package (rc={proc.returncode}): {detail}"
    return None


def check_lego_available(lego_bin: str = LEGO_BIN) -> Optional[str]:
    """`None` if `lego_bin` looks usable; otherwise a reason string naming exactly what is
    missing and where it was looked for."""
    if not Path(lego_bin).is_file():
        return f"lego binary not found at {lego_bin}"
    proc = subprocess.run([lego_bin, "--version"], capture_output=True, text=True, timeout=10)
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        return f"{lego_bin} --version failed (rc={proc.returncode}): {detail}"
    return None


# --------------------------------------------------------------------------- small helpers

def free_port() -> int:
    """An ephemeral localhost port, free at the moment of the check."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _https_directory_responds(port: int) -> bool:
    """Whether `https://127.0.0.1:<port>/acme/directory` answers at all. Deliberately
    unverified TLS here (seccert has not necessarily finished self-issuing its own
    serving certificate the instant its process starts accepting connections) -- this is
    a liveness poll only; the real trust check (`openssl verify` against the recorded
    Root+Intermediate) happens later, on the caller's own schedule, against the files
    this module writes."""
    ctx = ssl._create_unverified_context()
    try:
        with urllib.request.urlopen(f"https://127.0.0.1:{port}/acme/directory", timeout=1.0, context=ctx) as resp:
            return resp.status == 200
    except Exception:
        return False


def _wait_or_die(*, proc: subprocess.Popen, check, timeout_s: float, log_path: Path, what: str) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            log = log_path.read_text() if log_path.is_file() else "(no log file)"
            raise RuntimeError(f"{what} exited early (rc={proc.returncode}) before becoming ready.\n--- log ---\n{log}")
        if check():
            return
        time.sleep(0.2)
    log = log_path.read_text() if log_path.is_file() else "(no log file)"
    raise TimeoutError(f"{what} did not become ready within {timeout_s}s.\n--- log ---\n{log}")


# --------------------------------------------------------------------------- seccert

@dataclass
class SeccertInstance:
    """One running seccert process and the trust material it produced. `root_pem` and
    `intermediate_pem` are the actual files seccert generated on first boot
    (`<data_dir>/ca/{root,intermediate}.crt`); `trust_bundle_pem` is the two
    concatenated, exactly what `LEGO_CA_CERTIFICATES` needs."""

    data_dir: Path
    port: int
    http01_port: int
    external_url: str
    root_pem: Path
    intermediate_pem: Path
    trust_bundle_pem: Path
    log_path: Path


class _SeccertServer:
    """Context manager: starts one seccert instance, yields a [`SeccertInstance`], and
    shuts it down on every exit path -- normal return, an exception from inside the
    `with` block, or a failure to become ready in the first place (in which case the
    process, if it was started, is still terminated before the exception propagates)."""

    def __init__(self, data_dir: Path, port: int, http01_port: int, *, seccert_dir: Path = SECCERT_DIR, ca_key_type: str = "ecdsa-p384") -> None:
        self.data_dir = data_dir
        self.port = port
        self.http01_port = http01_port
        self.seccert_dir = seccert_dir
        self.ca_key_type = ca_key_type
        self._proc: Optional[subprocess.Popen] = None
        self._log_file = None

    def __enter__(self) -> SeccertInstance:
        self.data_dir.mkdir(parents=True, exist_ok=True)
        # "localhost", NOT "127.0.0.1": seccert's native-TLS self-issued serving
        # certificate (`ca.py::issue_leaf`) only ever emits a DNSName SAN for
        # `urlparse(external_url).hostname` (never an IPAddress SAN, even when that
        # hostname happens to look like an IP literal) -- so a client connecting to the
        # literal IP "127.0.0.1" hits Go's (lego's) TLS stack refusing the handshake with
        # "x509: cannot validate certificate for 127.0.0.1 because it doesn't contain any
        # IP SANs" (measured directly: `lego run` against `external_url=https://
        # 127.0.0.1:<port>` fails with exactly that error). Connecting by the hostname
        # "localhost" instead makes this an ordinary DNS-SAN check, which the
        # DNSName("localhost") SAN satisfies -- and "localhost" resolves to 127.0.0.1 on
        # this host exactly like "edge.localhost"/"ingest.localhost" do, so `SECCERT_HOST`
        # can stay the literal bind address "127.0.0.1" while only the URL clients use
        # changes.
        external_url = f"https://localhost:{self.port}"
        log_path = self.data_dir / "seccert.log"
        self._log_file = open(log_path, "w")

        python_bin = self.seccert_dir / ".venv" / "bin" / "python"
        env = dict(os.environ)
        env.update(
            {
                "SECCERT_DATA_DIR": str(self.data_dir),
                "SECCERT_HOST": "127.0.0.1",
                "SECCERT_PORT": str(self.port),
                "SECCERT_EXTERNAL_URL": external_url,
                "SECCERT_HTTP01_PORT": str(self.http01_port),
                "SECCERT_TLS_MODE": "native",
                "SECCERT_CA_KEY_TYPE": self.ca_key_type,
            }
        )
        self._proc = subprocess.Popen(
            [str(python_bin), "-m", "seccert"],
            cwd=str(self.seccert_dir),
            env=env,
            stdout=self._log_file,
            stderr=subprocess.STDOUT,
        )
        try:
            _wait_or_die(proc=self._proc, check=lambda: _https_directory_responds(self.port), timeout_s=READY_TIMEOUT_S, log_path=log_path, what=f"seccert on 127.0.0.1:{self.port} (data_dir={self.data_dir})")

            ca_dir = self.data_dir / "ca"
            root_pem = ca_dir / "root.crt"
            intermediate_pem = ca_dir / "intermediate.crt"
            deadline = time.monotonic() + READY_TIMEOUT_S
            while not (root_pem.is_file() and intermediate_pem.is_file()) and time.monotonic() < deadline:
                time.sleep(0.1)
            if not (root_pem.is_file() and intermediate_pem.is_file()):
                raise RuntimeError(f"seccert became ready but never wrote {root_pem} / {intermediate_pem}")

            trust_bundle_pem = self.data_dir / "trust_bundle.pem"
            trust_bundle_pem.write_text(root_pem.read_text() + intermediate_pem.read_text())

            return SeccertInstance(
                data_dir=self.data_dir,
                port=self.port,
                http01_port=self.http01_port,
                external_url=external_url,
                root_pem=root_pem,
                intermediate_pem=intermediate_pem,
                trust_bundle_pem=trust_bundle_pem,
                log_path=log_path,
            )
        except Exception:
            self._shutdown()
            raise

    def __exit__(self, exc_type, exc, tb) -> None:
        self._shutdown()

    def _shutdown(self) -> None:
        if self._proc is not None and self._proc.poll() is None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait(timeout=10)
        if self._log_file is not None:
            try:
                self._log_file.close()
            except Exception:
                pass


def seccert_server(data_dir: Path, port: int, http01_port: int, *, seccert_dir: Path = SECCERT_DIR, ca_key_type: str = "ecdsa-p384") -> _SeccertServer:
    """`with seccert_server(data_dir, port, http01_port) as instance:` -- see
    [`_SeccertServer`]."""
    return _SeccertServer(data_dir, port, http01_port, seccert_dir=seccert_dir, ca_key_type=ca_key_type)


# --------------------------------------------------------------------------- lego

@dataclass
class LegoResult:
    domain: str
    cert_pem: Path
    issuer_pem: Path
    key_pem: Path
    stdout: str
    stderr: str


def issue_leaf(*, lego_bin: str, seccert: SeccertInstance, domain: str, work_dir: Path, not_after: Optional[datetime] = None, email: str = "edge-test@example.invalid") -> LegoResult:
    """Issues one leaf for `domain` against `seccert`'s ACME directory. `work_dir` becomes
    lego's own `--path` (it writes `certificates/`, `accounts/` underneath). Must not be
    called concurrently with another `issue_leaf` against the SAME `seccert` instance --
    both would try to validate http-01 on `seccert.http01_port` (the manager's own
    measured constraint: seccert validates against `SECCERT_HTTP01_PORT`, not whatever
    port lego itself was told to listen on, so the two must match and therefore must not
    overlap in time). Sequential calls against the same instance, or concurrent calls
    against two *different* `SeccertInstance`s, are both fine.
    """
    work_dir.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env["LEGO_CA_CERTIFICATES"] = str(seccert.trust_bundle_pem)

    args = [
        lego_bin,
        "run",
        "--server",
        f"{seccert.external_url}/acme/directory",
        "--accept-tos",
        "--email",
        email,
        "--path",
        str(work_dir),
        "--key-type",
        "EC384",
        "--http",
        "--http.address",
        f":{seccert.http01_port}",
        "--domains",
        domain,
    ]
    if not_after is not None:
        args += ["--not-after", not_after.strftime("%Y-%m-%dT%H:%M:%SZ")]

    proc = subprocess.run(args, env=env, capture_output=True, text=True, timeout=LEGO_TIMEOUT_S)
    if proc.returncode != 0:
        raise RuntimeError(f"lego run --domains {domain} failed (rc={proc.returncode})\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")

    certs_dir = work_dir / "certificates"
    result = LegoResult(
        domain=domain,
        cert_pem=certs_dir / f"{domain}.crt",
        issuer_pem=certs_dir / f"{domain}.issuer.crt",
        key_pem=certs_dir / f"{domain}.key",
        stdout=proc.stdout,
        stderr=proc.stderr,
    )
    for path in (result.cert_pem, result.issuer_pem, result.key_pem):
        if not path.is_file():
            raise RuntimeError(f"lego reported success for {domain} but did not write the expected file {path}\n--- stdout ---\n{proc.stdout}")
    return result


# --------------------------------------------------------------------------- orchestration

@dataclass
class ProvisionResult:
    """Everything [`provision`] produced, under the caller's `output_dir`."""

    seccert1: SeccertInstance
    seccert2: SeccertInstance
    edge: LegoResult
    ingest: LegoResult
    short: LegoResult
    foreign: LegoResult
    short_not_before: datetime = field(repr=False)
    short_not_after: datetime = field(repr=False)


def provision(output_dir: Path, *, seccert_dir: Path = SECCERT_DIR, lego_bin: str = LEGO_BIN) -> ProvisionResult:
    """The full recipe: one seccert instance issuing `edge.localhost`, `ingest.localhost`
    and `short.localhost` (the last with a one-minute `--not-after`) sequentially on one
    http-01 port, then a second, independent seccert instance (its own data directory and
    ports, its own unrelated Root) issuing `foreign.localhost`. Everything lands under
    `output_dir`; both seccert processes are shut down before this function returns
    (successfully or by raising) -- see [`_SeccertServer.__exit__`]."""
    output_dir.mkdir(parents=True, exist_ok=True)

    edge_res: Optional[LegoResult] = None
    ingest_res: Optional[LegoResult] = None
    short_res: Optional[LegoResult] = None
    short_not_before: Optional[datetime] = None
    short_not_after: Optional[datetime] = None
    seccert1_snapshot: Optional[SeccertInstance] = None

    with seccert_server(output_dir / "seccert1", free_port(), free_port(), seccert_dir=seccert_dir) as seccert1:
        seccert1_snapshot = seccert1
        edge_res = issue_leaf(lego_bin=lego_bin, seccert=seccert1, domain="edge.localhost", work_dir=output_dir / "lego_edge")
        ingest_res = issue_leaf(lego_bin=lego_bin, seccert=seccert1, domain="ingest.localhost", work_dir=output_dir / "lego_ingest")

        short_not_before = datetime.now(timezone.utc)
        short_not_after = short_not_before + SHORT_LEAF_LIFETIME
        short_res = issue_leaf(lego_bin=lego_bin, seccert=seccert1, domain="short.localhost", work_dir=output_dir / "lego_short", not_after=short_not_after)

    foreign_res: Optional[LegoResult] = None
    seccert2_snapshot: Optional[SeccertInstance] = None
    with seccert_server(output_dir / "seccert2", free_port(), free_port(), seccert_dir=seccert_dir) as seccert2:
        seccert2_snapshot = seccert2
        foreign_res = issue_leaf(lego_bin=lego_bin, seccert=seccert2, domain="foreign.localhost", work_dir=output_dir / "lego_foreign")

    assert seccert1_snapshot is not None and seccert2_snapshot is not None
    assert edge_res is not None and ingest_res is not None and short_res is not None and foreign_res is not None
    assert short_not_before is not None and short_not_after is not None

    return ProvisionResult(
        seccert1=seccert1_snapshot,
        seccert2=seccert2_snapshot,
        edge=edge_res,
        ingest=ingest_res,
        short=short_res,
        foreign=foreign_res,
        short_not_before=short_not_before,
        short_not_after=short_not_after,
    )


def main(argv: list) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} <output-dir>", file=sys.stderr)
        return 2
    output_dir = Path(argv[1]).resolve()

    reason = check_seccert_available()
    if reason:
        print(f"seccert unavailable: {reason}", file=sys.stderr)
        return 1
    reason = check_lego_available()
    if reason:
        print(f"lego unavailable: {reason}", file=sys.stderr)
        return 1

    result = provision(output_dir)
    print(f"edge.localhost:    {result.edge.cert_pem}")
    print(f"ingest.localhost:  {result.ingest.cert_pem}")
    print(f"short.localhost:   {result.short.cert_pem}  (notAfter ~{result.short_not_after.isoformat()})")
    print(f"foreign.localhost: {result.foreign.cert_pem}  (issued under an unrelated Root: {result.seccert2.root_pem})")
    print(f"seccert1 Root:     {result.seccert1.root_pem}")
    print(f"seccert2 Root:     {result.seccert2.root_pem}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
