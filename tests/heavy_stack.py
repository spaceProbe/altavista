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
import contextlib
import json
import os
import re
import select
import shutil
import socket
import subprocess
import time
import uuid
from pathlib import Path
from types import SimpleNamespace
from typing import Optional, Tuple

import httpx
import pytest

from altavista.container_hardening import HARDENING_RUN_FLAGS, label_args, prune_stale_labelled_resources
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
    and `minio`.

    Round 5, item B: `GET /admin/api/counters` now authenticates (question 229's open
    ruling -- see `crates/av-tiles/src/admin.rs`'s own module doc). `--admin-role
    admin-readers` grants the ONE group this fixture mints an admin token for
    (`admin_token`, below) the `admin_counters` surface -- a group name deliberately
    distinct from `tile-readers`/`tile-guests` (the main port's own clearance groups),
    so an admin token and a tile-clearance token are never accidentally the same
    credential."""
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
        "--admin-role", "admin-readers",
    ]
    proc = subprocess.Popen(cmd, cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    try:
        _wait_for_listening_line(proc, READY_TIMEOUT_S)
        admin_token = issuer.mint(_valid_claims("heavy-stack-admin", ["admin-readers"]))
        yield SimpleNamespace(endpoint=f"127.0.0.1:{bind_port}", admin_endpoint=f"127.0.0.1:{admin_port}", admin_token=admin_token, proc=proc)
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
    for a real condition (a successful connection), never a fixed sleep-then-assume.

    Round 5, item B: this route now authenticates -- `av_tiles_service.admin_token`
    (minted for the `admin-readers` group the fixture's own `--admin-role` flag
    grants) is sent as `Authorization: Bearer <token>`, exactly like every other
    caller of this crate's authenticated surfaces in this module."""
    deadline = time.monotonic() + 10.0
    last_exc = None
    headers = {"Authorization": f"Bearer {av_tiles_service.admin_token}"}
    while time.monotonic() < deadline:
        try:
            resp = httpx.get(f"http://{av_tiles_service.admin_endpoint}/admin/api/counters", headers=headers, timeout=2.0)
            resp.raise_for_status()
            return resp.json()["counters"].get(code, 0)
        except httpx.HTTPError as e:
            last_exc = e
            time.sleep(0.1)
    raise AssertionError(f"could not read counters from {av_tiles_service.admin_endpoint}: {last_exc}")


# =================================================================================================
# Round 3, task P4 (H4's remaining round-2 open item 6): a CONTAINERISED variant of
# `av_tiles_service` above -- the real, unmodified `av-tiles:local` image (`services/tiles/
# Dockerfile` + `services/tiles/build-image.sh` + `services/tiles/IMAGE_DIGEST.md`, the same
# scripted, digest-recorded shape `services/edge-plugin/` already established), run as a
# container rather than a host subprocess, reusing `rust_bins`/`minio`/`key_prefix`/
# `tile_set_label`/`tile_set` exactly as `av_tiles_service` does -- only the gateway process
# itself moves into a container. Extracted here (not into `tests/test_tiles_container.py`
# alone) for the identical reason `av_tiles_service` itself lives here: a second consumer of
# "the real stack, gateway included" should never have to duplicate this.
#
# Gated separately from `SKIP_REASON` above (which only ever concerns the MinIO image):
# `TILES_SKIP_REASON` additionally requires `av-tiles:local` to be present AND to match
# `services/tiles/IMAGE_DIGEST.md`'s own recorded id (question 212(a): a test trusts an image
# only after comparing it to its recorded digest -- unlike `tests/test_edge_plugin_container.
# py`'s own `_compute_skip_reason`, which only checks presence by tag, this repeats the
# stronger digest-compare discipline `_compute_skip_reason` above already applies to MinIO,
# since a ':local' tag is exactly the kind of mutable reference a stale, un-rebuilt image can
# hide behind).
# =================================================================================================

TILES_IMAGE_TAG = "av-tiles:local"
TILES_IMAGE_DIGEST_MD = REPO_ROOT / "services" / "tiles" / "IMAGE_DIGEST.md"
TILES_BUILD_SCRIPT = "services/tiles/build-image.sh"
TILES_CONTAINER_SCRATCH_ROOT = REPO_ROOT / ".av-test-tmp" / "tiles_container"


def _parse_tiles_image_digest_md() -> str:
    """The RECORDED image id from `services/tiles/IMAGE_DIGEST.md`'s own "docker image
    inspect" fenced block -- generated by `services/tiles/build-image.sh`, never hand-edited.
    Question 212(a)'s "the recorded digest has exactly one home": this is the one function that
    reads it."""
    text = TILES_IMAGE_DIGEST_MD.read_text()
    recorded_id = _fenced_block_after(text, "docker image inspect")
    if not recorded_id:
        raise RuntimeError(f"could not find the 'docker image inspect' fenced code block in {TILES_IMAGE_DIGEST_MD}")
    return recorded_id


def _compute_tiles_skip_reason() -> Optional[str]:
    daemon_reason = _docker_daemon_unavailable_reason()
    if daemon_reason is not None:
        return f"Docker not available: {daemon_reason}"
    if not TILES_IMAGE_DIGEST_MD.exists():
        return (
            f"{TILES_IMAGE_DIGEST_MD} does not exist -- {TILES_IMAGE_TAG} has never been built on "
            f"this host (question 194: this test only inspects/runs an already-built image, it "
            f"never builds one). Run `{TILES_BUILD_SCRIPT}` on a host with Docker and network "
            f"access, then re-run this test."
        )
    try:
        recorded_id = _parse_tiles_image_digest_md()
    except RuntimeError as e:
        return str(e)
    actual_id = _local_image_id(TILES_IMAGE_TAG)
    if actual_id is None:
        return (
            f"image {TILES_IMAGE_TAG!r} is not present locally -- this test never builds one "
            f"(question 194: a test that finds no image and returns is a defect, so this is a "
            f"visible skip, not a silent pass). Run `{TILES_BUILD_SCRIPT}`, then re-run this test."
        )
    if actual_id != recorded_id:
        return (
            f"image {TILES_IMAGE_TAG!r} is present locally but its id {actual_id!r} does not match "
            f"the digest {recorded_id!r} recorded in {TILES_IMAGE_DIGEST_MD} (question 212(a): a "
            f"test trusts an image only after comparing it to its recorded digest). Rebuild with "
            f"`{TILES_BUILD_SCRIPT}` and re-run."
        )
    return None


TILES_SKIP_REASON = _compute_tiles_skip_reason()


def _wait_for_container_listening_line(container_id: str, deadline_s: float) -> None:
    """Polls `docker logs` for `av-tiles`' own `"LISTENING"` startup line -- mirrors
    `_wait_for_listening_line` above, adapted from a piped subprocess stdout (which a detached
    `docker run -d` does not give this process) to `docker logs`, exactly the same adaptation
    `tests/test_edge_plugin_container.py::_wait_for_container_stdout_lines` already made for the
    identical reason. Bounded polling, never a fixed sleep (question 199's neighbour rule: "poll
    a real condition, never sleep on a clock")."""
    deadline = time.monotonic() + deadline_s
    while True:
        logs = subprocess.run(["docker", "logs", container_id], capture_output=True, text=True).stdout
        if "LISTENING" in logs:
            return
        if time.monotonic() > deadline:
            pytest.fail(f"container {container_id} never printed its own LISTENING line within {deadline_s}s; docker logs:\n{logs}")
        time.sleep(0.1)


def _container_host_port(container_id: str, container_port: int) -> int:
    result = subprocess.run(["docker", "port", container_id, f"{container_port}/tcp"], capture_output=True, text=True, timeout=10)
    if result.returncode != 0:
        pytest.fail(f"`docker port {container_id} {container_port}/tcp` failed: {result.stderr}")
    return int(result.stdout.strip().rsplit(":", 1)[-1])


@contextlib.contextmanager
def tiles_gateway_container(rust_bins, minio, key_prefix, tile_set_label):
    """Starts the REAL, unmodified `av-tiles:local` image as a container, on a dedicated,
    labelled, user-defined bridge network shared with `minio` (connected to it by the network
    alias `"minio"` -- Docker's own embedded per-network DNS, the identical mechanism
    `tests/test_proposer_container.py` already relies on for its own Container P -> Container G
    name resolution), with its two ports (`--bind`/`--admin-bind`) published to the host so this
    context manager's callers can reach it exactly like `av_tiles_service` above.

    Deliberately a plain `@contextlib.contextmanager`, not a `@pytest.fixture` (unlike every
    other name in this module): `tests/test_tiles_container.py`'s own "the container is gone
    afterward" assertion needs to run AFTER teardown but WITHIN the same test function's own
    body (so a leaked container is that one test's own failure, not a fact only visible from a
    session-teardown hook) -- exactly `tests/test_edge_plugin_container.py::ResourceGuard`'s own
    shape (`guard.cleanup()` then `guard.assert_nothing_left()`, both inside one test), applied
    here as a context manager instead of a class since there is only ever one container to
    track. `rust_bins`/`minio`/`key_prefix`/`tile_set_label` are still the SAME pytest fixtures
    `av_tiles_service` depends on -- callers request them normally and pass the resolved values
    in.

    Three things this fixture gets right that a naive "just run the image" attempt would not
    (each measured, not assumed, while building this fixture):

    1. **`--bind 0.0.0.0:... --admin-bind 0.0.0.0:...`, never the image's own loopback
       defaults.** `services/tiles/Dockerfile`'s own "Network posture at RUNTIME" header section
       explains why: a process bound only to a container's own `127.0.0.1` is unreachable
       through Docker's published ports at all (published traffic arrives over `eth0`, never
       `lo`) -- the naive run (no `--bind` override) was tried first and its published port
       answered nothing, confirming this is a real trap, not a theoretical one.
    2. **The OIDC public key lives under `TILES_CONTAINER_SCRATCH_ROOT`
       (`<repo>/.av-test-tmp/tiles_container/`), never `tmp_path_factory`.** Unlike
       `av_tiles_service` above (a host subprocess, which can read any path directly), this
       fixture bind-mounts the key file INTO a container -- and Colima mounts only `$HOME` into
       its VM (`tests/test_edge_plugin_container.py`'s own module doc, "Why every bind-mount
       source lives under .av-test-tmp/"). A `tmp_path_factory` path is not under `$HOME` and
       would silently bind-mount as an empty directory. This fixture therefore mints its OWN
       `LocalTestIssuer` here rather than reusing the module's `issuer` fixture.
    3. **No writable volume is declared or mounted**, matching `services/tiles/Dockerfile`'s own
       "Container hardening" section: `av-tiles`' own production code (grepped) writes nothing
       to disk beyond the one startup read of the OIDC public key, so `--read-only` applies to
       the whole container root with no exception.
    """
    if TILES_SKIP_REASON is not None:
        pytest.skip(TILES_SKIP_REASON)

    run_id = f"tiles-container-{uuid.uuid4().hex[:12]}"
    scratch = TILES_CONTAINER_SCRATCH_ROOT / run_id
    scratch.mkdir(parents=True, exist_ok=True)

    with lock_docker_tests():
        # No prune here: `prune_stale_labelled_resources` removes EVERY resource carrying the
        # test label, and the `minio` fixture this context manager depends on already pruned
        # before creating its container, which carries that label. A second prune at this
        # point killed the fixture's own live MinIO (`docker events`: kill, die 137, destroy,
        # one second after creation) and every `docker network connect` after it failed with
        # "No such container" -- the third distinct reason this test had never been seen
        # green (lead, 2026-09-21, question 232). Prune once, before the first creation.
        issuer = LocalTestIssuer(scratch)
        network_name = f"av-tiles-test-net-{run_id}"

        net_result = subprocess.run(["docker", "network", "create", *label_args(run_id), network_name], capture_output=True, text=True, timeout=30)
        if net_result.returncode != 0:
            pytest.fail(f"`docker network create` failed: {net_result.stderr}")

        try:
            connect_result = subprocess.run(
                ["docker", "network", "connect", "--alias", "minio", network_name, minio.container_id],
                capture_output=True, text=True, timeout=30,
            )
            if connect_result.returncode != 0:
                pytest.fail(f"`docker network connect` (minio -> {network_name}) failed: {connect_result.stderr}")

            container_name = f"av-tiles-test-{run_id}"
            run_cmd = [
                "docker", "run", "-d", "--name", container_name, "--network", network_name,
                *label_args(run_id),
                *HARDENING_RUN_FLAGS,
                "-v", f"{issuer.public_key_path}:/keys/oidc_pub.pem:ro",
                "-p", "127.0.0.1::50073",
                "-p", "127.0.0.1::50173",
                TILES_IMAGE_TAG,
                "--oidc-issuer", TEST_ISSUER,
                "--oidc-audience", TEST_AUDIENCE,
                "--oidc-public-key-path", "/keys/oidc_pub.pem",
                "--ladder", LADDER,
                "--key-prefix", key_prefix,
                "--store-endpoint", "http://minio:9000",
                "--store-region", MINIO_REGION,
                "--store-access-key-id", minio.access_key,
                "--store-secret-access-key", minio.secret_key,
                "--store-bucket", minio.bucket,
                "--store-path-style",
                "--group-clearance", f"tile-readers={tile_set_label}",
                "--group-clearance", "tile-guests=UNCLASSIFIED",
                "--bind", "0.0.0.0:50073",
                "--admin-bind", "0.0.0.0:50173",
            ]
            run_result = subprocess.run(run_cmd, capture_output=True, text=True, timeout=30)
            if run_result.returncode != 0:
                pytest.fail(f"`docker run` (av-tiles container) failed: {run_result.stderr}")
            container_id = run_result.stdout.strip()

            try:
                _wait_for_container_listening_line(container_id, READY_TIMEOUT_S)

                # Question 212(a), restated at the point of actual use (mirrors `minio`'s own
                # identical re-check above): the RUNNING container's own image id must still
                # equal the recorded digest, not just the tag that was resolved at gate time.
                actual_id = _local_image_id(TILES_IMAGE_TAG)
                recorded_id = _parse_tiles_image_digest_md()
                assert actual_id == recorded_id, f"the RUNNING container's own image id {actual_id!r} must equal the recorded digest {recorded_id!r} (question 212(a)) -- got a mismatch after the module-level gate already passed"

                bind_port = _container_host_port(container_id, 50073)
                admin_port = _container_host_port(container_id, 50173)
                yield SimpleNamespace(
                    endpoint=f"127.0.0.1:{bind_port}",
                    admin_endpoint=f"127.0.0.1:{admin_port}",
                    container_id=container_id,
                    container_name=container_name,
                    issuer=issuer,
                )
            finally:
                subprocess.run(["docker", "rm", "-f", container_id], capture_output=True, timeout=30)
        finally:
            subprocess.run(["docker", "network", "rm", network_name], capture_output=True, timeout=30)
            shutil.rmtree(scratch, ignore_errors=True)
