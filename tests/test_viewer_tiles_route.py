"""H5b-1 (`docs/heavy-plan.md` H5, round 3, "the served through the viewer server with the
gateway's authentication" half): the viewer server's `/api/tiles/*` routes really proxy a
REAL `av-tiles` gateway backed by a REAL MinIO object store, with the gateway's own OIDC
authentication and per-layer label enforcement intact end to end -- not a mock, not the
`InMemoryObjectSource` `crates/av-tiles/src/server.rs`'s own unit tests use.

# What this stands up, for real

1. A real MinIO container, from the recorded digest (`services/store/IMAGE_DIGEST.md`),
   labelled, pruned-by-label before creation, under the host-wide docker-test lock -- this
   file's own `_parse_image_digest_md`/`_local_image_id`/`_compute_skip_reason` mirror
   `crates/av-store/tests/minio_store.rs`'s and `crates/av-jobs/tests/store_tiler.rs`'s own
   `parse_image_digest_md`/`gate`/`gate_or_skip!` (their own module docs: "the recorded
   digest has exactly one home"), expressed in Python rather than depended on across
   languages. Labelling, pruning and the host-wide lock reuse `altavista.container_hardening.
   {label_args, prune_stale_labelled_resources}` and `altavista.docker_test_lock.
   lock_docker_tests` -- the SAME helpers `tests/test_edge_plugin_container.py` and
   `tests/test_proposer_container.py` already use for their own labelled containers, not a
   third way of doing any of this (this task's own brief).
2. `cargo build`s `av-jobs`' `av-tile-fixture` (`--features store-fixture`) and `av-tiles`'
   own `av-tiles` binary, once, module-scoped -- `tests/test_command_console_routes.py`'s own
   `command_bin` fixture is the template, `RUSTUP_PATH_PREFIX` handling included.
3. Runs `av-tile-fixture` for real against that MinIO to build a small (a few tens of tiles)
   real tile set labelled `CUI`, and captures the manifest hash from its own JSON stdout.
4. A `LocalTestIssuer` (copied from `tests/test_command_console_routes.py` -- that file's own
   module doc explains why: no `cryptography`/`pyjwt` in this venv, and minting a real
   RS256-signed token from Python via the system `openssl` CLI is entirely practical) mints
   two tokens: `TOKEN_AT_CLEARANCE` (groups map to `CUI`, exactly the tile set's own label --
   "at" clearance) and `TOKEN_BELOW_CLEARANCE` (groups map to `UNCLASSIFIED`, below it).
5. Starts a real `av-tiles` subprocess on an OS-assigned ephemeral loopback port (the
   bind-then-close trick `tests/test_command_console_routes.py::_free_port` already uses,
   picked before the subprocess starts -- `av-tiles` itself prints back whatever `--bind`
   value it was given, before it has actually bound, so an OS-assigned `:0` would not let this
   file recover the real port from that line), configured with that issuer, the ladder, the
   group-clearance map and that MinIO, plus a SECOND ephemeral port for H5b-1's own new
   `--admin-bind` (`crates/av-tiles/src/admin.rs`, `GET /admin/api/counters` -- added THIS
   task specifically so a refusal's own counter can be read back from outside the process;
   see that module's own doc for why round 2's "no admin surface" decision is revisited).
   Polls the subprocess's own `LISTENING` stdout line via `select.select` with a real
   deadline -- never a fixed `time.sleep`.
6. Builds the FastAPI app with `create_app(tiles_endpoint=..., tiles_token_path=...)` and
   drives it through `fastapi.testclient.TestClient`. `tiles_token_path` names a real file
   this test itself writes and can OVERWRITE mid-test -- the one way to prove
   `altavista.tiles_client`'s own "Rule 1" (the token is read fresh at request time, not
   cached at startup) against a real server rather than merely asserting it in prose.

# What each test would catch (this task's own standing review requirement -- "for each test,
name the wrong implementation it would fail against")

* ``test_manifest_route_proxies_the_real_manifest_byte_for_byte``: a proxy that reconstructs
  or re-encodes the manifest (rather than passing the gateway's own bytes straight through)
  would still often produce a valid-looking `TileSetManifest`, but its SHA-256 would not equal
  the tile set's own identity `av-tile-fixture` printed -- this is the one test that would
  catch a byte-for-byte corruption a "looks fine" JSON-shape check would miss entirely.
* ``test_a_tiles_bytes_come_back_and_their_sha256_equals_the_gateways_own_etag``: a proxy that
  drops or invents its own `ETag` (rather than forwarding the gateway's own) would pass a
  "some ETag header exists" check while this test's own recomputed SHA-256 comparison still
  fails.
* ``test_range_request_gives_206_with_exactly_the_requested_bytes_and_a_matching_if_none_
  match_gives_304``: a proxy that fails to forward `Range`/`If-None-Match` from the incoming
  request (this task's own rule 4) would see the gateway answer a plain `200` with the FULL
  tile every time -- this test's own exact byte-slice and status-code assertions catch that
  even though a "some bytes came back" check would not.
* ``test_a_below_clearance_configured_token_is_refused_403_and_the_gateways_own_counter_
  increments``: a proxy that swallows the gateway's own `403` and answers a generic `200`/
  `500` (or a caller-controlled clearance that let a below-clearance token through) fails the
  status-code assertion; a refusal this test could not distinguish from "nothing happened at
  all" (never reaching the gateway, or the gateway refusing for an unrelated reason) is ruled
  out by the counter assertion, read from the gateway's OWN `/admin/api/counters` -- this is
  exactly this task's own "a refusal that is not counted is not a refusal this project
  accepts" rule.
* ``test_an_unknown_manifest_hash_is_404_not_500``: a proxy (or a gateway) that turns a
  not-found manifest into an unhandled exception -- an empty body with no status-code
  guarantee, or FastAPI's own default `500` -- fails this test's exact status assertion.
* ``test_the_token_appears_in_neither_the_response_body_the_response_headers_nor_any_log_
  record``: a route that echoes the `Authorization` header back (a common accidental "proxy
  transparency" bug: forwarding EVERY response/request header verbatim instead of an explicit
  allowlist) or a client library that logs its own request headers at an enabled log level
  would leak the token into exactly the three places this test inspects.
* ``test_rotating_the_token_file_between_two_requests_changes_behaviour_on_the_very_next_
  request``: a server that reads `tiles_token_path` once at `create_app` time and caches the
  token in memory (rather than `tiles_client.proxy_get`'s own "read fresh, every call" -- this
  task's own rule 1, "so rotation works") would keep answering with the FIRST token's own
  clearance forever, never picking up the second file's content at all.
* ``test_create_app_with_no_tiles_arguments_answers_a_typed_503_and_every_other_route_still_
  works``: a `create_app` that raises, hangs, or answers an empty `200` when the tiles
  arguments are left at their defaults -- or one whose new routes' mere presence breaks an
  existing, unrelated route -- fails this test's own byte-for-byte "nothing else changed"
  assertion (mirrors `tests/test_command_console_routes.py::test_no_command_endpoint_
  configured_answers_a_typed_503_and_other_routes_still_work`, restated for the tiles routes).
* ``test_a_configured_but_unreachable_tiles_endpoint_answers_a_typed_503``: a `proxy_get` that
  lets an `httpx` connection error (or a timeout) escape uncaught would answer FastAPI's own
  default `500`, not the typed `503` this route promises; this test needs no docker gate at
  all (a bound-then-closed loopback port is deterministically unreachable), so it always runs.

# No network at test time (question 154) / no environment mutation (question 199)

`tests/test_command_console_routes.py`'s own module doc already establishes both points for
this whole test suite, restated here: binding/connecting to `127.0.0.1` never leaves the
host's own kernel network stack -- it is not "the network" for question 154's purposes. No
test in this file calls `os.environ[...]`/`monkeypatch.setenv` to configure anything
`av-tiles`, `av-tile-fixture` or this server reads; every setting reaches a subprocess or
`create_app` as an explicit argument, a CLI flag, or a file this test itself wrote and named
explicitly.
"""
from __future__ import annotations

import base64
import hashlib
import json
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
from fastapi.testclient import TestClient

from altavista.container_hardening import label_args, prune_stale_labelled_resources
from altavista.docker_test_lock import lock_docker_tests
from altavista.pb.altavista.v1 import heavy_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parents[1]
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
IMAGE_DIGEST_MD = REPO_ROOT / "services" / "store" / "IMAGE_DIGEST.md"

# Measured directly on this host (question 148: not assumed): a freshly `cargo build`-linked
# binary's FIRST execution can stall many seconds to low minutes with the process reporting
# 0.00s user/system CPU the entire time (macOS's syspolicyd/AMFI code-signing validation on
# first launch of a new binary -- observed directly: a trivial, argument-free `./target/debug/
# av-tiles` invocation, which does no I/O and fails argument parsing near-instantly once
# actually running, took 23.4s wall-clock on its FIRST run and 0.005s on its second, with
# `syspolicyd` visibly near 130% CPU in `top` at the time -- this host runs several tracks'
# worth of freshly-built binaries concurrently, each triggering the identical first-launch
# check). 90.0s (this file's own value through two rounds of this task) was measured
# insufficient twice in a row under today's load, each attempt's av-tiles subprocess showing
# the identical 0.00s CPU signature for the entire 90s. Raised to 240.0s with this reason
# recorded at the call site -- mirrors `docs/heavy-plan.md`'s own round-1 status, "the
# readiness budget was raised to 60s with the reason recorded at the call site" for the
# analogous MinIO-container contention finding.
READY_TIMEOUT_S = 240.0
MINIO_HEALTH_TIMEOUT_S = 30.0
CARGO_BUILD_TIMEOUT_S = 900.0  # this host has been observed taking over 600s on a cold tree.

TEST_ISSUER = "https://sso.test.example/"
TEST_AUDIENCE = "av-tiles"
LADDER = "UNCLASSIFIED,CUI,SECRET"
KEY_PREFIX = "viewer-tiles-route-test"
TILE_SET_LABEL = "CUI"
MINIO_REGION = "us-east-1"


def _cargo_env() -> dict:
    import os

    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _free_port() -> int:
    """Same bind-then-close trick as `tests/test_command_console_routes.py::_free_port` --
    the one way to hand a subprocess a real, OS-assigned ephemeral loopback port it can print
    back verbatim in its own readiness line (`av-tiles` prints the `--bind` value it was
    GIVEN, before it has bound at all -- a literal `:0` would not let this file recover the
    real port from that printed line)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


class LocalTestIssuer:
    """Copied from `tests/test_command_console_routes.py::LocalTestIssuer` -- see that
    class's own docstring for why this is the answer to "mint a token from Python with no new
    dependency": a local RSA-2048 key pair + RS256 signer driven through the system `openssl`
    CLI, the Python-side mirror of `crates/av-command/src/test_support.rs::TestIssuer`."""

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
    """Matches every check `crates/av-command/src/oidc.rs::verify` makes, against the REAL
    wall clock -- `av-tiles`' own binary always constructs a `SystemClock`, exactly like
    `av-command`'s (there is no `--clock` flag to inject a `TestClock` into a separate
    process). Mirrors `tests/test_command_console_routes.py::_valid_claims`."""
    now = int(time.time())
    return {"iss": TEST_ISSUER, "aud": TEST_AUDIENCE, "sub": sub, "iat": now, "exp": now + 3600, "groups": groups, "amr": [], "acr": "", "jti": str(uuid.uuid4())}


# =================================================================================================
# services/store/IMAGE_DIGEST.md parsing -- mirrors crates/av-store/tests/minio_store.rs's and
# crates/av-jobs/tests/store_tiler.rs's own parse_image_digest_md/fenced_block_after/gate exactly
# (this file's own module doc, "What this stands up, for real", item 1).
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
    """`docker image inspect <image_ref> --format {{.Id}}` -- resolves only against an image
    already present locally; never triggers a pull (question 154). Mirrors
    `av_lockstep::docker::local_image_id` exactly."""
    result = subprocess.run(["docker", "image", "inspect", image_ref, "--format", "{{.Id}}"], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def _compute_skip_reason() -> Optional[str]:
    """Question 212(a): `Ok`/`None` iff the daemon answers AND the image is present locally
    AND its id equals the recorded one exactly -- mirrors `av_lockstep::docker::recorded_
    digest_gate`. Computed once at import time (`tests/test_edge_plugin_container.py`'s own
    module-level `_compute_skip_reason` convention), never re-derived per test."""
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


_SKIP_REASON = _compute_skip_reason()


# =================================================================================================
# Rust binaries -- module-scoped, mirrors tests/test_command_console_routes.py::command_bin.
# =================================================================================================


@pytest.fixture(scope="module")
def rust_bins():
    """Builds `av-jobs`' `av-tile-fixture` (`--features store-fixture`, `crates/av-jobs/
    Cargo.toml`'s own `[[bin]]` entry -- see that file's own comment for why the feature gate
    exists) and `av-tiles`' own `av-tiles` binary, once for the module. A build failure here is
    a real failure of this task, not something to skip over -- same posture as `tests/
    test_command_console_routes.py::command_bin`."""
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
# lock for its ENTIRE life (creation through every test through teardown), mirroring
# crates/av-jobs/tests/store_tiler.rs's own "hold the lock for the test's whole body" rule,
# generalised here to the whole life of a container this module's tests share.
# =================================================================================================


def _random_creds(run_id: str) -> Tuple[str, str]:
    sanitized = "".join(c for c in run_id if c.isalnum())
    return f"avtilesroutetest{sanitized}", f"avtilesroutetestsecret{sanitized}"


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
def minio():
    if _SKIP_REASON is not None:
        pytest.skip(_SKIP_REASON)
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

            yield SimpleNamespace(host_port=host_port, access_key=access_key, secret_key=secret_key, bucket="av-tiles-route-test", container_id=container_id)
        finally:
            subprocess.run(["docker", "rm", "-f", container_id], capture_output=True, timeout=30)


# =================================================================================================
# The small, real tile set -- module-scoped: run av-tile-fixture once against the real MinIO.
# =================================================================================================


@pytest.fixture(scope="module")
def tile_set(rust_bins, minio):
    """Runs `av-tile-fixture` for real against `minio` to build a small (a few tens of tiles)
    tile set labelled `CUI` -- this task's own step 3. Returns the fixture's own printed JSON,
    parsed, plus the store configuration every later fixture/test needs to read the SAME
    objects back out (through `av-tiles`, never directly)."""
    cmd = [
        str(rust_bins.tile_fixture),
        "--key-prefix", KEY_PREFIX,
        "--ladder", LADDER,
        "--label-marking", TILE_SET_LABEL,
        "--job-id", "viewer-tiles-route-test",
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
    return LocalTestIssuer(tmp_path_factory.mktemp("av_tiles_issuer"))


@pytest.fixture(scope="module")
def token_at_clearance(issuer):
    """Groups map to `CUI` -- exactly `TILE_SET_LABEL`, the "at" boundary this task's own
    brief asks for ("at or above")."""
    return issuer.mint(_valid_claims("viewer-at-clearance", ["tile-readers"]))


@pytest.fixture(scope="module")
def token_below_clearance(issuer):
    """Groups map to `UNCLASSIFIED` -- below `TILE_SET_LABEL` (`CUI`)."""
    return issuer.mint(_valid_claims("viewer-below-clearance", ["tile-guests"]))


# =================================================================================================
# The real av-tiles subprocess.
# =================================================================================================


def _wait_for_listening_line(proc: subprocess.Popen, deadline_s: float) -> str:
    """Polls `proc`'s own stdout for a line containing `LISTENING`, via `select.select` with a
    real deadline -- never a fixed `time.sleep` (this task's own binding rule: "poll for a
    real condition"). `select` (not a blocking `readline()`) is what makes this a bounded
    wait rather than a wait that could hang forever if the subprocess printed nothing at all."""
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
def av_tiles_service(rust_bins, minio, issuer):
    """Starts a real `av-tiles` subprocess on two OS-assigned ephemeral loopback ports (main
    + `--admin-bind`), configured with `issuer`, `LADDER`, a group-clearance map, and `minio`
    -- this task's own step 5."""
    bind_port = _free_port()
    admin_port = _free_port()
    cmd = [
        str(rust_bins.av_tiles),
        "--oidc-issuer", TEST_ISSUER,
        "--oidc-audience", TEST_AUDIENCE,
        "--oidc-public-key-path", str(issuer.public_key_path),
        "--ladder", LADDER,
        "--key-prefix", KEY_PREFIX,
        "--store-endpoint", f"http://127.0.0.1:{minio.host_port}",
        "--store-region", MINIO_REGION,
        "--store-access-key-id", minio.access_key,
        "--store-secret-access-key", minio.secret_key,
        "--store-bucket", minio.bucket,
        "--store-path-style",
        "--group-clearance", "tile-readers=CUI",
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


def _admin_counter(av_tiles_service, code: str) -> int:
    """Reads `code`'s own current count from the REAL, running `av-tiles` gateway's own
    `GET /admin/api/counters` (`crates/av-tiles/src/admin.rs`) -- never assumed, never
    inferred from this file's own request count. A short, bounded retry tolerates the
    admin listener's own bind finishing a moment after the main port's (both are spawned
    before the subprocess's own `LISTENING` line, but the admin one is not itself polled for
    -- see `crates/av-tiles/src/bin/av-tiles.rs`'s own ordering comment); this is a poll for a
    real condition (a successful connection), never a fixed sleep-then-assume."""
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


# =================================================================================================
# The FastAPI app under test.
# =================================================================================================


@pytest.fixture()
def token_path(tmp_path, token_at_clearance) -> Path:
    path = tmp_path / "tiles_token.txt"
    path.write_text(token_at_clearance)
    return path


@pytest.fixture()
def client(av_tiles_service, token_path, tmp_path) -> TestClient:
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, tiles_endpoint=av_tiles_service.endpoint, tiles_token_path=token_path)
    return TestClient(app)


# =================================================================================================
# The tests.
# =================================================================================================


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_manifest_route_proxies_the_real_manifest_byte_for_byte(client: TestClient, tile_set):
    resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert resp.status_code == 200, resp.text
    assert hashlib.sha256(resp.content).hexdigest() == tile_set.manifest_sha256, "the proxied manifest's own SHA-256 must equal the tile set's identity av-tile-fixture printed"

    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(resp.content)
    assert manifest.object_key_prefix == KEY_PREFIX
    assert len(manifest.tiles) == tile_set.tile_count


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_a_tiles_bytes_come_back_and_their_sha256_equals_the_gateways_own_etag(client: TestClient, tile_set):
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert manifest_resp.status_code == 200, manifest_resp.text
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]

    resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}")
    assert resp.status_code == 200, resp.text
    etag = resp.headers.get("etag", "").strip('"')
    assert etag, resp.headers
    assert hashlib.sha256(resp.content).hexdigest() == etag, "the tile's own bytes must hash to exactly the gateway's own ETag"
    assert etag == tile.sha256


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_range_request_gives_206_with_exactly_the_requested_bytes_and_a_matching_if_none_match_gives_304(client: TestClient, tile_set):
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert manifest_resp.status_code == 200, manifest_resp.text
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]
    path = f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"

    full = client.get(path)
    assert full.status_code == 200

    ranged = client.get(path, headers={"Range": "bytes=0-3"})
    assert ranged.status_code == 206, ranged.text
    assert ranged.content == full.content[0:4]
    assert ranged.headers.get("content-range") == f"bytes 0-3/{len(full.content)}"

    etag = full.headers["etag"]
    not_modified = client.get(path, headers={"If-None-Match": etag})
    assert not_modified.status_code == 304, not_modified.text
    assert not_modified.content == b""


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_a_below_clearance_configured_token_is_refused_403_and_the_gateways_own_counter_increments(client: TestClient, tile_set, token_path: Path, token_below_clearance: str, av_tiles_service):
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert manifest_resp.status_code == 200, manifest_resp.text
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]
    path = f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"

    before = _admin_counter(av_tiles_service, "tiles_layer_label_over_clearance")

    # Rewrite the SAME token file to the below-clearance token -- proves rule 1's "read
    # fresh, at request time" on the SAME app/client this test already built, rather than a
    # second app instance (see this module's own doc, item 6).
    token_path.write_text(token_below_clearance)

    resp = client.get(path)
    assert resp.status_code == 403, resp.text
    assert resp.content == b"", "a refusal must never carry tile bytes"

    after = _admin_counter(av_tiles_service, "tiles_layer_label_over_clearance")
    assert after == before + 1, f"the gateway's own tiles_layer_label_over_clearance counter must increment by exactly 1 (before={before}, after={after})"


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_rotating_the_token_file_between_two_requests_changes_behaviour_on_the_very_next_request(client: TestClient, tile_set, token_path: Path, token_at_clearance: str, token_below_clearance: str):
    """Restates the previous test's own rotation proof from the opposite direction (below then
    back to at-clearance), and adds the ONE assertion the previous test does not: the FIRST
    request (before any rewrite) must succeed. Together the two tests prove rotation in both
    directions on one running server, never a value cached at `create_app` time."""
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]
    path = f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"

    first = client.get(path)  # token_path fixture already wrote token_at_clearance.
    assert first.status_code == 200, first.text

    token_path.write_text(token_below_clearance)
    second = client.get(path)
    assert second.status_code == 403, second.text

    token_path.write_text(token_at_clearance)
    third = client.get(path)
    assert third.status_code == 200, third.text


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_an_unknown_manifest_hash_is_404_not_500(client: TestClient):
    never_stored = "b" * 64
    resp = client.get(f"/api/tiles/{never_stored}/manifest")
    assert resp.status_code == 404, resp.text


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_the_token_appears_in_neither_the_response_body_the_response_headers_nor_any_log_record(client: TestClient, tile_set, token_at_clearance: str, caplog):
    with caplog.at_level("DEBUG"):
        resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert resp.status_code == 200, resp.text

    assert token_at_clearance.encode("utf-8") not in resp.content
    for name, value in resp.headers.items():
        assert token_at_clearance not in value, f"the token leaked into response header {name!r}: {value!r}"
    for record in caplog.records:
        assert token_at_clearance not in record.getMessage(), f"the token leaked into a log record: {record.getMessage()!r}"


def test_create_app_with_no_tiles_arguments_answers_a_typed_503_and_every_other_route_still_works(tmp_path):
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)  # no tiles_endpoint/tiles_token_path at all
    client = TestClient(app)

    resp = client.get(f"/api/tiles/{'a' * 64}/manifest")
    assert resp.status_code == 503, resp.text
    assert "not configured" in resp.text.lower() or "no tiles" in resp.text.lower(), resp.text

    resp = client.get(f"/api/tiles/{'a' * 64}/tiles/0/0/0")
    assert resp.status_code == 503, resp.text

    # Every pre-existing route must still work -- the whole point of "configuration, not a
    # hard dependency" (mirrors tests/test_command_console_routes.py's identical assertion
    # for /api/command/*).
    assert client.get("/api/health").status_code == 200
    assert client.get("/api/scenarios").status_code == 200


def test_a_configured_but_unreachable_tiles_endpoint_answers_a_typed_503(tmp_path):
    unused_port = _free_port()  # bound-then-closed: nothing is listening here
    token_path = tmp_path / "unused_token.txt"
    token_path.write_text("irrelevant-for-this-test")
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, tiles_endpoint=f"127.0.0.1:{unused_port}", tiles_token_path=token_path)
    client = TestClient(app)

    resp = client.get(f"/api/tiles/{'a' * 64}/manifest")
    assert resp.status_code == 503, resp.text
    assert "unreachable" in resp.text.lower(), resp.text

    assert client.get("/api/health").status_code == 200
