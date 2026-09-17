"""H5b-2 (docs/heavy-plan.md H5, round 3): the shared docker-gated "stand up a real
`av-tiles` gateway, backed by a real MinIO, serving a real tile set built by a real
`av-tile-fixture` run" stack-up, extracted out of `tests/test_viewer_tiles_route.py`
(H5b-1) so `tests/test_viewer_layers_stream.py` (H5b-2 deliverable 4) can stand up the
IDENTICAL real stack rather than a second, independently-drifting copy of it --
question 218's "no second, independently-maintained copy of a rule" reasoning,
applied here to test infrastructure rather than to application logic.

Every fixture and helper below is copied verbatim (same behaviour, same reasoning) from
`tests/test_viewer_tiles_route.py`'s own H5b-1 commit; that file's own module docstring
already explains WHY each piece exists (the digest gate, the host-wide docker-test
lock, the labelled MinIO container, the `LocalTestIssuer`, the readiness poll) and is
not repeated a second time here -- see it for the full reasoning. This file is the ONE
home for that reasoning and that code now; `tests/test_viewer_tiles_route.py` imports
every one of these names rather than redefining any of them, and this file's own
9-passed baseline is unchanged (see that file's own module docstring for the
byte-for-byte "nothing else changed" requirement this extraction had to preserve).

# How a second test module reuses this

Pytest fixtures imported by name into another test module become usable fixtures in
THAT module too (a standard, documented pytest pattern -- "sharing fixtures across
multiple files" -- not a bespoke mechanism this file invents): a module does

    from heavy_stack import rust_bins, minio, tile_set, issuer, av_tiles_service, ...

and every one of those becomes available to its own test functions by name, exactly as
if it had defined them itself. `key_prefix`/`tile_set_label`/`minio_bucket` are
themselves fixtures (not plain module constants) specifically so a second test module
COULD override them with its own fixture of the same name if it ever needed a
different key prefix or clearance label -- in practice neither existing caller does,
because each `minio`/`av_tiles_service` fixture instance is a brand-new container/
process every time (`run_id` below always carries a fresh `uuid.uuid4()`), so nothing
protects two SEPARATE test modules' fixture instances from using the identical default
values already reachable by that indirection.

# No network at test time beyond loopback (question 154) / no environment mutation
(question 199)

Restated here from `tests/test_command_console_routes.py`'s own module doc (which
first established both points for this whole test suite): binding/connecting to
`127.0.0.1` never leaves the host's own kernel network stack -- it is not "the
network" for question 154's purposes. Nothing in this module calls
`os.environ[...]`/`monkeypatch.setenv` to configure anything `av-tiles`,
`av-tile-fixture`, or a caller's own server reads; every setting reaches a subprocess
as an explicit argument, a CLI flag, or a file a test itself wrote and named
explicitly.
"""
from __future__ import annotations

import base64
import json
import os
import re
import select
import socket
import subprocess
import time
import uuid
from pathlib import Path
from types import SimpleNamespace
from typing import Optional, Tuple

import httpx
import pytest

from altavista.container_hardening import label_args, prune_stale_labelled_resources
from altavista.docker_test_lock import lock_docker_tests

REPO_ROOT = Path(__file__).resolve().parent.parent
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
IMAGE_DIGEST_MD = REPO_ROOT / "services" / "store" / "IMAGE_DIGEST.md"

# Measured directly on this host (question 148: not assumed) -- see
# tests/test_viewer_tiles_route.py's own H5b-1 commit for the full first-launch
# syspolicyd/AMFI code-signing measurement this value is based on.
READY_TIMEOUT_S = 240.0
MINIO_HEALTH_TIMEOUT_S = 30.0
CARGO_BUILD_TIMEOUT_S = 900.0  # this host has been observed taking over 600s on a cold tree.

TEST_ISSUER = "https://sso.test.example/"
TEST_AUDIENCE = "av-tiles"
LADDER = "UNCLASSIFIED,CUI,SECRET"
MINIO_REGION = "us-east-1"


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _free_port() -> int:
    """Bind-then-close trick, same as `tests/test_command_console_routes.py::_free_port`
    -- the one way to hand a subprocess a real, OS-assigned ephemeral loopback port it
    can print back verbatim in its own readiness line."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


class LocalTestIssuer:
    """Copied from `tests/test_command_console_routes.py::LocalTestIssuer` -- see that
    class's own docstring for why this is the answer to "mint a token from Python with
    no new dependency": a local RSA-2048 key pair + RS256 signer driven through the
    system `openssl` CLI, the Python-side mirror of
    `crates/av-command/src/test_support.rs::TestIssuer`."""

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


def _valid_claims(sub: str, groups: list) -> dict:
    """Matches every check `crates/av-command/src/oidc.rs::verify` makes, against the
    REAL wall clock -- `av-tiles`' own binary always constructs a `SystemClock`, exactly
    like `av-command`'s (there is no `--clock` flag to inject a `TestClock` into a
    separate process)."""
    now = int(time.time())
    return {"iss": TEST_ISSUER, "aud": TEST_AUDIENCE, "sub": sub, "iat": now, "exp": now + 3600, "groups": groups, "amr": [], "acr": "", "jti": str(uuid.uuid4())}


# =================================================================================================
# services/store/IMAGE_DIGEST.md parsing -- mirrors crates/av-store/tests/minio_store.rs's and
# crates/av-jobs/tests/store_tiler.rs's own parse_image_digest_md/fenced_block_after/gate exactly.
# =================================================================================================


def _fenced_block_after(text: str, marker: str) -> Optional[str]:
    lines = text.splitlines()
    marker_idx = next((i for i, line in enumerate(lines) if marker in line), None)
    if marker_idx is None:
        return None
    try:
        fence_start = next(i for i in range(marker_idx, len(lines)) if lines[i].strip() == "```")
        fence_end = next(i for i in range(fence_start + 1, len(lines)) if lines[i].strip() == "```")
    except StopIteration:
        return None
    content = "\n".join(lines[fence_start + 1 : fence_end]).strip()
    return content or None


def _parse_image_digest_md() -> Tuple[str, str]:
    text = IMAGE_DIGEST_MD.read_text()
    image_ref = _fenced_block_after(text, "Registry reference")
    recorded_id = _fenced_block_after(text, "docker image inspect")
    if not image_ref or not recorded_id:
        raise RuntimeError(f"could not find the 'Registry reference'/'docker image inspect' fenced code blocks in {IMAGE_DIGEST_MD}")
    return image_ref, recorded_id


def _docker_daemon_unavailable_reason() -> Optional[str]:
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"docker is not installed or could not be launched: {e}"
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip().splitlines()
        return f"`docker info` exited {result.returncode}: {detail[-1] if detail else '(no stderr)'}"
    return None


def _local_image_id(image_ref: str) -> Optional[str]:
    """`docker image inspect <image_ref> --format {{.Id}}` -- resolves only against an
    image already present locally; never triggers a pull (question 154). Mirrors
    `av_lockstep::docker::local_image_id` exactly."""
    result = subprocess.run(["docker", "image", "inspect", image_ref, "--format", "{{.Id}}"], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def _compute_skip_reason() -> Optional[str]:
    """Question 212(a): `Ok`/`None` iff the daemon answers AND the image is present
    locally AND its id equals the recorded one exactly -- mirrors
    `av_lockstep::docker::recorded_digest_gate`. Computed once at import time
    (`tests/test_edge_plugin_container.py`'s own module-level `_compute_skip_reason`
    convention), never re-derived per test."""
    daemon_reason = _docker_daemon_unavailable_reason()
    if daemon_reason is not None:
        return f"Docker not available: {daemon_reason}"
    try:
        image_ref, recorded_id = _parse_image_digest_md()
    except RuntimeError as e:
        return str(e)
    actual_id = _local_image_id(image_ref)
    if actual_id is None:
        return (
            f"image {image_ref!r} is not present locally (question 154: this test never pulls -- "
            f"see {IMAGE_DIGEST_MD} for the one-time `docker pull` command that puts it there)."
        )
    if actual_id != recorded_id:
        return (
            f"image {image_ref!r} is present locally but its id {actual_id!r} does not match the "
            f"digest {recorded_id!r} recorded in {IMAGE_DIGEST_MD} (question 212(a): a test trusts "
            f"an image only after comparing it to its recorded digest)."
        )
    return None


SKIP_REASON = _compute_skip_reason()


# =================================================================================================
# Rust binaries -- module-scoped, mirrors tests/test_command_console_routes.py::command_bin.
# =================================================================================================


@pytest.fixture(scope="module")
def rust_bins():
    """Builds `av-jobs`' `av-tile-fixture` (`--features store-fixture`) and `av-tiles`'
    own `av-tiles` binary, once for the module. A build failure here is a real failure
    of this task, not something to skip over -- same posture as
    `tests/test_command_console_routes.py::command_bin`."""
    env = _cargo_env()
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-jobs", "--bin", "av-tile-fixture", "--features", "store-fixture"],
        cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=CARGO_BUILD_TIMEOUT_S,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-jobs --bin av-tile-fixture --features store-fixture failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-tiles", "--bin", "av-tiles"],
        cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=CARGO_BUILD_TIMEOUT_S,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-tiles --bin av-tiles failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    tile_fixture_bin = REPO_ROOT / "target" / "debug" / "av-tile-fixture"
    av_tiles_bin = REPO_ROOT / "target" / "debug" / "av-tiles"
    assert tile_fixture_bin.is_file(), f"expected {tile_fixture_bin} after a successful cargo build"
    assert av_tiles_bin.is_file(), f"expected {av_tiles_bin} after a successful cargo build"
    return SimpleNamespace(tile_fixture=tile_fixture_bin, av_tiles=av_tiles_bin)


# =================================================================================================
# The real, labelled MinIO container -- module-scoped, held under the host-wide docker-test
# lock for its ENTIRE life (creation through every test through teardown).
# =================================================================================================


def _random_creds(run_id: str) -> Tuple[str, str]:
    sanitized = "".join(c for c in run_id if c.isalnum())
    return f"avheavystacktest{sanitized}", f"avheavystacktestsecret{sanitized}"


def _wait_for_minio_health(host_port: int, container_id: str) -> None:
    deadline = time.monotonic() + MINIO_HEALTH_TIMEOUT_S
    last_err = "never attempted"
    while time.monotonic() < deadline:
        try:
            resp = httpx.get(f"http://127.0.0.1:{host_port}/minio/health/live", timeout=2.0)
            if resp.status_code == 200:
                return
            last_err = f"status {resp.status_code}"
        except httpx.HTTPError as e:
            last_err = str(e)
        time.sleep(0.15)
    logs = subprocess.run(["docker", "logs", container_id], capture_output=True, text=True).stdout
    pytest.fail(f"MinIO at 127.0.0.1:{host_port} (container {container_id}) did not answer GET /minio/health/live with 200 within {MINIO_HEALTH_TIMEOUT_S}s; last error: {last_err}; docker logs:\n{logs}")


@pytest.fixture(scope="module")
def minio_bucket() -> str:
    """The bucket name `minio`/`tile_set` use -- its own fixture (not a bare module
    constant) so a caller could override it, though nothing forces one to: each
    `minio` fixture instance is a brand-new container every time (see this module's
    own doc, "How a second test module reuses this")."""
    return "av-tiles-heavy-stack-test"


@pytest.fixture(scope="module")
def minio(minio_bucket):
    if SKIP_REASON is not None:
        pytest.skip(SKIP_REASON)
    image_ref, recorded_id = _parse_image_digest_md()
    run_id = f"{__name__}-{uuid.uuid4()}"

    with lock_docker_tests():
        prune_stale_labelled_resources()

        access_key, secret_key = _random_creds(run_id)
        cmd = [
            "docker", "run", "-d", *label_args(run_id),
            "-e", f"MINIO_ROOT_USER={access_key}",
            "-e", f"MINIO_ROOT_PASSWORD={secret_key}",
            "-p", "127.0.0.1::9000",
            image_ref,
            "server", "/data",
        ]
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        if result.returncode != 0:
            pytest.fail(f"`docker run` (MinIO) failed: {result.stderr}")
        container_id = result.stdout.strip()

        try:
            port_result = subprocess.run(["docker", "port", container_id, "9000/tcp"], capture_output=True, text=True, timeout=10)
            if port_result.returncode != 0:
                pytest.fail(f"`docker port {container_id} 9000/tcp` failed: {port_result.stderr}")
            host_port = int(port_result.stdout.strip().rsplit(":", 1)[-1])

            _wait_for_minio_health(host_port, container_id)

            actual_id = _local_image_id(image_ref)
            assert actual_id == recorded_id, f"the RUNNING container's own image id {actual_id!r} must equal the recorded digest {recorded_id!r} (question 212(a)) -- got a mismatch after the module-level gate already passed"

            yield SimpleNamespace(host_port=host_port, access_key=access_key, secret_key=secret_key, bucket=minio_bucket, container_id=container_id)
        finally:
            subprocess.run(["docker", "rm", "-f", container_id], capture_output=True, timeout=30)


# =================================================================================================
# The small, real tile set -- module-scoped: run av-tile-fixture once against the real MinIO.
# =================================================================================================


@pytest.fixture(scope="module")
def key_prefix() -> str:
    """The `--key-prefix`/`object_key_prefix` value `tile_set`/`av_tiles_service` use --
    its own fixture (see this module's own doc, "How a second test module reuses
    this")."""
    return "heavy-stack-test"


@pytest.fixture(scope="module")
def tile_set_label() -> str:
    """The `--label-marking` a tile set is built with, and the "at clearance" group's
    own mapped clearance -- its own fixture for the same reason as `key_prefix`."""
    return "CUI"


@pytest.fixture(scope="module")
def tile_set(rust_bins, minio, key_prefix, tile_set_label):
    """Runs `av-tile-fixture` for real against `minio` to build a small (a few tens of
    tiles) tile set labelled `tile_set_label`. Returns the fixture's own printed JSON,
    parsed, plus the store configuration every later fixture/test needs to read the
    SAME objects back out (through `av-tiles`, never directly)."""
    cmd = [
        str(rust_bins.tile_fixture),
        "--key-prefix", key_prefix,
        "--ladder", LADDER,
        "--label-marking", tile_set_label,
        "--job-id", f"{key_prefix}-job",
        "--min-level", "0",
        "--max-level", "2",
        "--tile-size", "16",
        "--synthetic-source", "32x16",
        "--store-endpoint", f"http://127.0.0.1:{minio.host_port}",
        "--store-region", MINIO_REGION,
        "--store-access-key-id", minio.access_key,
        "--store-secret-access-key", minio.secret_key,
        "--store-bucket", minio.bucket,
        "--store-path-style",
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        pytest.fail(f"av-tile-fixture failed (returncode={proc.returncode}):\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    stdout_lines = [line for line in proc.stdout.splitlines() if line.strip()]
    assert stdout_lines, f"av-tile-fixture printed nothing on stdout; stderr:\n{proc.stderr}"
    result = json.loads(stdout_lines[-1])
    assert re.fullmatch(r"[0-9a-f]{64}", result["manifest_sha256"]), result
    assert result["tile_count"] >= 10, f"expected a few tens of tiles, got {result['tile_count']}: {result}"
    return SimpleNamespace(**result)


# =================================================================================================
# The RS256 issuer and its two tokens.
# =================================================================================================


@pytest.fixture(scope="module")
def issuer(tmp_path_factory):
    return LocalTestIssuer(tmp_path_factory.mktemp("heavy_stack_issuer"))


@pytest.fixture(scope="module")
def token_at_clearance(issuer, tile_set_label):
    """Groups map to `tile_set_label` -- exactly the tile set's own label ("at"
    clearance)."""
    return issuer.mint(_valid_claims("heavy-stack-at-clearance", ["tile-readers"]))


@pytest.fixture(scope="module")
def token_below_clearance(issuer):
    """Groups map to `UNCLASSIFIED` -- below `tile_set_label` (as long as
    `tile_set_label` is anything above `UNCLASSIFIED` on `LADDER`, true for both
    callers' own default of `CUI`)."""
    return issuer.mint(_valid_claims("heavy-stack-below-clearance", ["tile-guests"]))


# =================================================================================================
# The real av-tiles subprocess.
# =================================================================================================


def _wait_for_listening_line(proc: subprocess.Popen, deadline_s: float) -> str:
    """Polls `proc`'s own stdout for a line containing `LISTENING`, via `select.select`
    with a real deadline -- never a fixed `time.sleep` (this task's own binding rule:
    "poll for a real condition"). `select` (not a blocking `readline()`) is what makes
    this a bounded wait rather than a wait that could hang forever if the subprocess
    printed nothing at all."""
    deadline = time.monotonic() + deadline_s
    collected: list = []
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            pytest.fail(f"av-tiles did not print its own LISTENING line within {deadline_s}s (returncode={proc.poll()}):\n{''.join(collected)}")
        ready, _, _ = select.select([proc.stdout], [], [], min(remaining, 1.0))
        if not ready:
            continue
        line = proc.stdout.readline()
        if not line:
            if proc.poll() is not None:
                pytest.fail(f"av-tiles exited before printing its own LISTENING line (returncode={proc.poll()}):\n{''.join(collected)}")
            continue
        collected.append(line)
        if "LISTENING" in line:
            return "".join(collected)


@pytest.fixture(scope="module")
def av_tiles_service(rust_bins, minio, issuer, key_prefix, tile_set_label):
    """Starts a real `av-tiles` subprocess on two OS-assigned ephemeral loopback ports
    (main + `--admin-bind`), configured with `issuer`, `LADDER`, a group-clearance map,
    and `minio`."""
    bind_port = _free_port()
    admin_port = _free_port()
    cmd = [
        str(rust_bins.av_tiles),
        "--oidc-issuer", TEST_ISSUER,
        "--oidc-audience", TEST_AUDIENCE,
        "--oidc-public-key-path", str(issuer.public_key_path),
        "--ladder", LADDER,
        "--key-prefix", key_prefix,
        "--store-endpoint", f"http://127.0.0.1:{minio.host_port}",
        "--store-region", MINIO_REGION,
        "--store-access-key-id", minio.access_key,
        "--store-secret-access-key", minio.secret_key,
        "--store-bucket", minio.bucket,
        "--store-path-style",
        "--group-clearance", f"tile-readers={tile_set_label}",
        "--group-clearance", "tile-guests=UNCLASSIFIED",
        "--bind", f"127.0.0.1:{bind_port}",
        "--admin-bind", f"127.0.0.1:{admin_port}",
    ]
    proc = subprocess.Popen(cmd, cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    try:
        _wait_for_listening_line(proc, READY_TIMEOUT_S)
        yield SimpleNamespace(endpoint=f"127.0.0.1:{bind_port}", admin_endpoint=f"127.0.0.1:{admin_port}", proc=proc)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def admin_counter(av_tiles_service, code: str) -> int:
    """Reads `code`'s own current count from the REAL, running `av-tiles` gateway's own
    `GET /admin/api/counters` (`crates/av-tiles/src/admin.rs`) -- never assumed, never
    inferred from a caller's own request count. A short, bounded retry tolerates the
    admin listener's own bind finishing a moment after the main port's; this is a poll
    for a real condition (a successful connection), never a fixed sleep-then-assume."""
    deadline = time.monotonic() + 10.0
    last_exc = None
    while time.monotonic() < deadline:
        try:
            resp = httpx.get(f"http://{av_tiles_service.admin_endpoint}/admin/api/counters", timeout=2.0)
            resp.raise_for_status()
            return resp.json()["counters"].get(code, 0)
        except httpx.HTTPError as e:
            last_exc = e
            time.sleep(0.1)
    raise AssertionError(f"could not read counters from {av_tiles_service.admin_endpoint}: {last_exc}")
