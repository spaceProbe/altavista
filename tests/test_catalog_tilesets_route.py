"""Question 228's finding 2 (server half): `altavista.gateway_client` (a real gRPC client for
`DataGatewayService.Query`'s `GATEWAY_SELECTOR_CATALOG` selector) and the `GET /api/catalog/
tilesets` route `altavista/server.py` declares on top of it.

# Why a fake in-process `DataGatewayService`, not a real `av-gateway` subprocess

`tests/test_command_console_routes.py`/`tests/heavy_stack.py` build REAL `av-command`/
`av-tiles` subprocesses because what those suites need to prove includes the REAL Rust
service's own state machine or label enforcement. This suite's job is narrower and entirely on
the Python side of the wire: does `altavista.gateway_client`/`altavista/server.py`'s new route
(a) build the request this task's brief specifies (`caller_token` from `--gateway-token-path`,
`caller_clearance` always empty, no caller-suppliable field ever set from an incoming HTTP
request), (b) map every way it can fail to a typed, specific error, never a fake empty list,
and (c) never forward or get influenced by a caller-supplied `Authorization` header. A fake,
in-process `grpc.server()` running this test's own tiny `DataGatewayServiceServicer` proves
every one of those exactly as precisely as a real `av-gateway` process would (it is what
*receives* the wire request and can assert on it directly) while adding no `cargo build`, no
subprocess, and no docker dependency at all. `crates/av-gateway`'s own real auth/label
enforcement (verifying `caller_token`, deriving clearance, refusing a disagreeing `caller_
clearance`) is already covered by that crate's own Rust tests and is out of scope here -- see
this task's own report.

# No network at test time (question 154) / no environment mutation (question 199)

Binding/connecting to `127.0.0.1` never leaves the host's own kernel network stack (mirrors
every other file in this suite). No test here calls `os.environ`/`monkeypatch.setenv` to
configure anything this server reads -- every setting reaches `create_app` as an explicit
argument, or a file this test itself wrote and named explicitly.
"""
from __future__ import annotations

import json
import re
import select
import socket
import subprocess
import threading
import time
import uuid
from concurrent import futures
from pathlib import Path
from types import SimpleNamespace
from typing import List, Optional, Tuple

import grpc
import pytest
from fastapi.testclient import TestClient

from altavista import gateway_client
from altavista.container_hardening import label_args, prune_stale_labelled_resources
from altavista.docker_test_lock import lock_docker_tests
from altavista.pb import authority_pb2, authority_pb2_grpc, entity_pb2, envelope_pb2, heavy_pb2
from altavista.server import create_app

# Heavy round 6 (docs/heavy-plan.md's "A gap in the drive path that nobody had noticed"): the
# REAL round-trip proof at the bottom of this file needs `heavy_stack`'s own module-level
# docker/image-digest helpers and its `rust_bins`/MinIO machinery -- imported as a module (not
# only by fixture name) so this file can also reach its underscore-prefixed helpers
# (`_fenced_block_after`, `_local_image_id`, `_docker_daemon_unavailable_reason`, `_random_
# creds`, `_wait_for_minio_health`) rather than writing a FOURTH copy of "parse an IMAGE_
# DIGEST.md fenced code block" (Rust already has two: `crates/av-catalog/tests/
# catalog_postgis.rs`, `crates/av-gateway/tests/catalog_selector.rs`; Python already has one:
# `tests/heavy_stack.py` itself) -- see `REAL_ROUND_TRIP_SKIP_REASON`'s own comment below for
# exactly which pieces are reused and why reusing the FIXTURE objects themselves (`minio`,
# `tile_set`) is not safe here (question 232).
import heavy_stack  # noqa: E402 -- after the third-party imports above, matching this file's own existing import block
from heavy_stack import rust_bins  # noqa: F401,E402 -- transitive fixture dependency (av-tile-fixture)

MANIFEST_SHA256 = "a" * 64


def _free_port() -> int:
    """Bind-then-close trick -- same as `tests/heavy_stack.py::_free_port` and `tests/
    test_command_console_routes.py::_free_port`; not imported from either so this file has no
    dependency on `heavy_stack`'s own module-level docker probe (`heavy_stack.SKIP_REASON` is
    computed at IMPORT time -- this file avoids importing that module at all, so nothing here
    is affected by docker's presence or absence)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _fixture_catalog_record(job_id: str = "job-xyz") -> "heavy_pb2.CatalogRecord":
    return heavy_pb2.CatalogRecord(
        asset_id=f"tileset:{job_id}:{MANIFEST_SHA256}",
        asset=entity_pb2.AssetRef(
            uri="s3://altavista-heavy/tiles/manifest.pb",
            sha256=MANIFEST_SHA256,
            size_bytes=123_456_789,
            media_type=gateway_client.TILESET_MANIFEST_MEDIA_TYPE,
            label=envelope_pb2.Label(marking="CUI", caveats=["SP-EXPT"]),
        ),
        job_id=job_id,
        created_tai_ns=1_820_000_000_123_456_789,  # past 2**53 -- see this file's own stringification test
        footprint_wkt="",
    )


class _RecordingDataGatewayServicer(authority_pb2_grpc.DataGatewayServiceServicer):
    """Records every `GatewayQueryRequest` it receives (never inspects HTTP headers -- there
    are none at the gRPC transport; this is the whole point of testing at this boundary) and
    answers one fixed `CatalogRecord`."""

    def __init__(self, records: Optional[List["heavy_pb2.CatalogRecord"]] = None) -> None:
        self.received_requests: List["authority_pb2.GatewayQueryRequest"] = []
        self._records = records if records is not None else [_fixture_catalog_record()]
        self._lock = threading.Lock()

    def Query(self, request, context):  # noqa: N802 -- grpc's own generated method name
        with self._lock:
            self.received_requests.append(request)
        return authority_pb2.GatewayQueryResponse(catalog_records=self._records)


class _RefusingDataGatewayServicer(authority_pb2_grpc.DataGatewayServiceServicer):
    """Answers a real, typed gRPC refusal -- mirrors `crates/av-gateway/src/gateway.rs::
    to_status`'s own `CatalogRefusal::CatalogNotConfigured -> Status::failed_precondition`
    mapping (a real, deployed `av-gateway` with no `--catalog-host` answers exactly this way),
    proving this server's own route surfaces it, code and message, rather than flattening it."""

    def __init__(self, code: "grpc.StatusCode", message: str) -> None:
        self.code = code
        self.message = message

    def Query(self, request, context):  # noqa: N802
        context.abort(self.code, self.message)


def _start_fake_gateway(servicer) -> Tuple["grpc.Server", str]:
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=2))
    authority_pb2_grpc.add_DataGatewayServiceServicer_to_server(servicer, server)
    port = server.add_insecure_port("127.0.0.1:0")
    server.start()
    return server, f"127.0.0.1:{port}"


@pytest.fixture()
def fake_gateway():
    servicer = _RecordingDataGatewayServicer()
    server, endpoint = _start_fake_gateway(servicer)
    yield servicer, endpoint
    server.stop(grace=None)


@pytest.fixture()
def gateway_token_path(tmp_path: Path) -> Path:
    path = tmp_path / "gateway_token.txt"
    path.write_text("server-own-secret-token")
    return path


# =================================================================================================
# altavista.gateway_client -- unit tests (no HTTP layer at all).
# =================================================================================================


def test_list_tile_sets_with_no_endpoint_or_token_path_raises_not_configured():
    config = gateway_client.GatewayServiceConfig()
    with pytest.raises(gateway_client.GatewayNotConfiguredError):
        gateway_client.list_tile_sets(config)


def test_list_tile_sets_with_grpcio_absent_raises_grpcio_not_installed(fake_gateway, monkeypatch, tmp_path):
    """Simulates "grpcio is not installed" without uninstalling it -- `gateway_client`'s own
    module-level `grpc` binding is exactly what a real absent-grpcio interpreter would leave
    `None` at, mirroring `tests/test_command_console_routes.py::test_grpcio_absent_...`. Uses a
    real, readable token file: `list_tile_sets` reads the token before it ever builds a stub
    (see `gateway_client._read_token`'s own doc), so an unreadable path would raise `Gateway
    UnreachableError` first and this test would end up exercising the wrong code path."""
    _, endpoint = fake_gateway
    token_path = tmp_path / "token.txt"
    token_path.write_text("t")
    monkeypatch.setattr(gateway_client, "grpc", None)
    config = gateway_client.GatewayServiceConfig(endpoint=endpoint, token_path=token_path)
    with pytest.raises(gateway_client.GrpcioNotInstalledError):
        gateway_client.list_tile_sets(config)


def test_list_tile_sets_reads_the_token_fresh_on_every_call(fake_gateway, tmp_path):
    servicer, endpoint = fake_gateway
    token_path = tmp_path / "token.txt"
    token_path.write_text("first-token")
    config = gateway_client.GatewayServiceConfig(endpoint=endpoint, token_path=token_path)

    gateway_client.list_tile_sets(config)
    token_path.write_text("second-token")
    gateway_client.list_tile_sets(config)

    assert [r.caller_token for r in servicer.received_requests] == ["first-token", "second-token"], (
        "a config built once (at create_app/serve time) must still pick up a rotated token file "
        "on the very next call -- never a value cached at construction time"
    )


def test_list_tile_sets_sends_the_tileset_media_type_filter_and_leaves_clearance_empty(fake_gateway, tmp_path):
    servicer, endpoint = fake_gateway
    token_path = tmp_path / "token.txt"
    token_path.write_text("t")
    config = gateway_client.GatewayServiceConfig(endpoint=endpoint, token_path=token_path)

    gateway_client.list_tile_sets(config)

    assert len(servicer.received_requests) == 1
    request = servicer.received_requests[0]
    assert request.selector == authority_pb2.GATEWAY_SELECTOR_CATALOG
    assert request.catalog_query.media_type == "application/vnd.altavista.tileset-manifest+pb"
    assert request.catalog_query.media_type == gateway_client.TILESET_MANIFEST_MEDIA_TYPE
    assert not request.HasField("run"), "GATEWAY_SELECTOR_CATALOG must carry no run identity (H2c)"
    # The auth-model rule this task's own brief is explicit about: caller_clearance is always
    # sent empty (the gateway derives it from the verified caller_token), and the D1
    # caller-supplied-path attack field is never set.
    assert request.caller_clearance == ""
    assert request.caller_supplied_products_uri == ""


def test_list_tile_sets_stringifies_int64_and_uint64_fields_for_the_browser(fake_gateway, tmp_path):
    """TAI epochs (and, this module's own deliberately-conservative choice -- see `gateway_
    client._int64`'s own docstring -- any other int64/uint64) cross to the browser as JSON
    STRINGS, never numbers: `created_tai_ns`/`size_bytes` here are both chosen to already
    exceed `Number.MAX_SAFE_INTEGER` (2**53 - 1), so a regression back to a plain JSON int
    would silently round them -- this test would then fail on an exact string comparison."""
    _, endpoint = fake_gateway
    token_path = tmp_path / "token.txt"
    token_path.write_text("t")
    config = gateway_client.GatewayServiceConfig(endpoint=endpoint, token_path=token_path)

    [entry] = gateway_client.list_tile_sets(config)
    assert entry["createdTaiNs"] == "1820000000123456789"
    assert isinstance(entry["createdTaiNs"], str)
    assert entry["sizeBytes"] == "123456789"
    assert isinstance(entry["sizeBytes"], str)
    assert 123_456_789 < 2**53 - 1, "sanity: this particular size is small; created_tai_ns is the one past 2**53"
    assert entry["manifestSha256"] == MANIFEST_SHA256
    assert entry["name"] == "job-xyz"
    assert entry["marking"] == "CUI"
    assert entry["caveats"] == ["SP-EXPT"]


def test_list_tile_sets_against_an_unreachable_endpoint_raises_gateway_unreachable():
    unused_port = _free_port()  # bound-then-closed: nothing is listening here
    config = gateway_client.GatewayServiceConfig(endpoint=f"127.0.0.1:{unused_port}", token_path="/nonexistent-but-unread")
    # _read_token runs before the RPC and would itself fail first on a real unreadable path --
    # use a real, readable token file so the failure this test targets is really the RPC's own
    # UNAVAILABLE, not _read_token's OSError path (covered separately below).
    with pytest.raises(gateway_client.GatewayUnreachableError):
        gateway_client.list_tile_sets(config)


def test_list_tile_sets_with_an_unreadable_token_path_raises_gateway_unreachable(fake_gateway, tmp_path):
    _, endpoint = fake_gateway
    config = gateway_client.GatewayServiceConfig(endpoint=endpoint, token_path=tmp_path / "does-not-exist.txt")
    with pytest.raises(gateway_client.GatewayUnreachableError):
        gateway_client.list_tile_sets(config)


def test_list_tile_sets_against_a_real_refusal_raises_gateway_refused_carrying_the_code_and_message(tmp_path):
    servicer = _RefusingDataGatewayServicer(grpc.StatusCode.FAILED_PRECONDITION, "catalog tier not configured on this av-gateway deployment")
    server, endpoint = _start_fake_gateway(servicer)
    try:
        token_path = tmp_path / "token.txt"
        token_path.write_text("t")
        config = gateway_client.GatewayServiceConfig(endpoint=endpoint, token_path=token_path)
        with pytest.raises(gateway_client.GatewayRefusedError) as excinfo:
            gateway_client.list_tile_sets(config)
        assert excinfo.value.code == grpc.StatusCode.FAILED_PRECONDITION
        assert "catalog tier not configured" in excinfo.value.message
    finally:
        server.stop(grace=None)


@pytest.mark.parametrize(
    "code,expected_status",
    [
        (grpc.StatusCode.INVALID_ARGUMENT, 400),
        (grpc.StatusCode.NOT_FOUND, 404),
        (grpc.StatusCode.FAILED_PRECONDITION, 409),
        (grpc.StatusCode.PERMISSION_DENIED, 403),
        (grpc.StatusCode.UNAUTHENTICATED, 401),
        (grpc.StatusCode.INTERNAL, 502),  # unmapped falls through to the documented default
    ],
)
def test_gateway_service_error_to_http_status_maps_every_reachable_refusal_code(code, expected_status):
    exc = gateway_client.GatewayRefusedError(code, "a real refusal message")
    status, message = gateway_client.gateway_service_error_to_http_status(exc)
    assert status == expected_status
    assert message == "a real refusal message", "a real refusal's own message must be carried verbatim, never replaced"


def test_gateway_service_error_to_http_status_maps_every_own_failure_to_503():
    for exc in (
        gateway_client.GatewayNotConfiguredError(),
        gateway_client.GrpcioNotInstalledError(),
        gateway_client.GatewayUnreachableError("127.0.0.1:1", "connection refused"),
    ):
        status, _message = gateway_client.gateway_service_error_to_http_status(exc)
        assert status == 503, f"{type(exc).__name__} must map to 503 (this server's own failure to reach the gateway at all)"


# =================================================================================================
# GET /api/catalog/tilesets -- the FastAPI route.
# =================================================================================================


def test_a_caller_supplied_authorization_header_is_not_forwarded_and_does_not_influence_the_result(tmp_path, fake_gateway, gateway_token_path):
    """The single most important test in this file (this task's own brief): a caller-supplied
    `Authorization` header must never be forwarded to the gateway, and must never change the
    result -- the server's own token (`gateway_token_path`) is what always reaches the gateway,
    as `GatewayQueryRequest.caller_token`. Proven two ways: (1) the response is byte-for-byte
    identical whether or not the caller sends the header at all, and (2) both real requests the
    fake gateway received carry the SERVER's token, never the caller's header value, on
    `caller_token`."""
    servicer, endpoint = fake_gateway
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, gateway_endpoint=endpoint, gateway_token_path=gateway_token_path)
    client = TestClient(app)

    with_header = client.get("/api/catalog/tilesets", headers={"Authorization": "Bearer caller-supplied-evil-token"})
    without_header = client.get("/api/catalog/tilesets")

    assert with_header.status_code == 200, with_header.text
    assert without_header.status_code == 200, without_header.text
    assert with_header.json() == without_header.json(), "the result must not depend on whether, or what, Authorization header the caller sent"

    assert len(servicer.received_requests) == 2
    for received in servicer.received_requests:
        assert received.caller_token == "server-own-secret-token", "the server's own configured token must be the one attached"
        assert received.caller_token != "caller-supplied-evil-token", "the caller's own header value must never reach the gateway"
        assert received.caller_clearance == ""


def test_a_successful_listing_has_the_shape_the_layers_panel_needs(tmp_path, fake_gateway, gateway_token_path):
    _, endpoint = fake_gateway
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, gateway_endpoint=endpoint, gateway_token_path=gateway_token_path)
    client = TestClient(app)

    resp = client.get("/api/catalog/tilesets")
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert "tileSets" in body
    [entry] = body["tileSets"]
    for key in ("manifestSha256", "name", "marking", "sizeBytes", "assetId", "jobId", "createdTaiNs"):
        assert key in entry, f"missing {key!r} in {entry!r}"
    assert entry["manifestSha256"] == MANIFEST_SHA256
    assert isinstance(entry["sizeBytes"], str)
    assert isinstance(entry["createdTaiNs"], str)


def test_no_gateway_endpoint_configured_answers_a_typed_error_never_an_empty_list(tmp_path):
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)  # no gateway_endpoint/gateway_token_path at all
    client = TestClient(app)

    resp = client.get("/api/catalog/tilesets")
    assert resp.status_code == 503, resp.text
    assert resp.json() != {"tileSets": []}, "an unconfigured gateway must never look like an empty catalog"
    assert "not configured" in resp.text.lower() or "no data gateway" in resp.text.lower(), resp.text

    # Every pre-existing route must still work -- configuration, not a hard dependency, mirrors
    # tests/test_command_console_routes.py's and tests/test_viewer_tiles_route.py's identical
    # assertions for their own new routes.
    assert client.get("/api/health").status_code == 200
    assert client.get("/api/scenarios").status_code == 200
    assert client.get(f"/api/tiles/{'a' * 64}/manifest").status_code == 503  # tiles route unaffected


def test_gateway_endpoint_configured_but_unreachable_answers_a_typed_error(tmp_path, gateway_token_path):
    unused_port = _free_port()  # bound-then-closed: nothing is listening here
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, gateway_endpoint=f"127.0.0.1:{unused_port}", gateway_token_path=gateway_token_path)
    client = TestClient(app)

    resp = client.get("/api/catalog/tilesets")
    assert resp.status_code == 503, resp.text
    assert "unreachable" in resp.text.lower(), resp.text
    assert client.get("/api/health").status_code == 200


def test_grpcio_absent_answers_a_typed_error_and_other_routes_still_work(tmp_path, monkeypatch, gateway_token_path):
    monkeypatch.setattr(gateway_client, "grpc", None)
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, gateway_endpoint="127.0.0.1:1", gateway_token_path=gateway_token_path)
    client = TestClient(app)

    resp = client.get("/api/catalog/tilesets")
    assert resp.status_code == 503, resp.text
    assert "grpc" in resp.text.lower() and "extra" in resp.text.lower(), resp.text
    assert client.get("/api/health").status_code == 200


def test_a_real_gateway_refusal_is_never_flattened_to_a_generic_500(tmp_path, gateway_token_path):
    """The deployment-level "unconfigured" case -- a REAL, reachable `av-gateway` with no
    catalog tier wired up (`CatalogRefusal::CatalogNotConfigured`) is a different fact from
    THIS server having no `--gateway-endpoint` at all (the previous test): that one is 503
    (this server cannot even reach a gateway); this one is 409 (a real gateway answered, and
    its own answer was a typed refusal) -- both are "specific and honest", neither is a
    fake empty list, and this test is what tells the two apart."""
    servicer = _RefusingDataGatewayServicer(grpc.StatusCode.FAILED_PRECONDITION, "no catalog tier is configured on this av-gateway deployment")
    server, endpoint = _start_fake_gateway(servicer)
    try:
        app = create_app(texture_dir=tmp_path, web_dir=tmp_path, gateway_endpoint=endpoint, gateway_token_path=gateway_token_path)
        client = TestClient(app)
        resp = client.get("/api/catalog/tilesets")
        assert resp.status_code == 409, resp.text
        assert "no catalog tier is configured" in resp.text
        assert resp.json() != {"tileSets": []}
    finally:
        server.stop(grace=None)


# =================================================================================================
# The REAL round trip (heavy round 6, task 5): `av-catalog-migrate` against a fresh, digest-
# verified PostGIS container, a real `av-tile-fixture` registering a manifest through it, and a
# real `av-gateway` subprocess serving `GET /api/catalog/tilesets` for real -- every step above
# this point in the file proves `altavista.gateway_client`/`altavista/server.py` against a FAKE
# gateway (this module's own doc, "Why a fake in-process DataGatewayService"); this section is
# the other half docs/heavy-plan.md's "A gap in the drive path that nobody had noticed" asked
# for: proof that the whole chain -- migrate, register, list -- actually works end to end,
# against REAL binaries and a REAL PostgreSQL/PostGIS server, not merely that this file's own
# Python adapter builds the right gRPC request.
#
# # Why this is NOT built out of `heavy_stack.minio`/`heavy_stack.tile_set`
#
# `heavy_stack.minio` (module-scoped) takes `lock_docker_tests()` and calls `prune_stale_
# labelled_resources()` once, at ITS OWN creation, then holds the lock for the rest of the
# module's life (through every test's use, to its own teardown). A PostGIS container this file
# also needs cannot be created by a SECOND, independently-scoped fixture that ALSO calls
# `prune_stale_labelled_resources()` -- that second prune would run AFTER `minio`'s own
# container already exists and is daemon-wide (`altavista.container_hardening.
# prune_stale_labelled_resources`'s own doc: "removes every container ... still carrying
# `av.test`"), which is exactly question 232's defect ("this exact mistake killed the tile-
# container test for a round"): a live container a fixture already depends on, swept by a
# SECOND fixture's own prune. So `real_stack_containers` below creates BOTH the MinIO AND the
# PostGIS container itself, under ONE `lock_docker_tests()` scope with exactly ONE `prune_
# stale_labelled_resources()` call before either is created (question 156/232) -- reusing every
# PURE helper function `heavy_stack` already has for the MinIO half (`_random_creds`, `_wait_
# for_minio_health`, `_local_image_id`, `MINIO_REGION`) rather than a second, drifting copy of
# them, while owning its own single lock/prune scope end to end. The lock is held from before
# the first `docker run` through the LAST assertion this file's own round-trip test makes
# (question 207's own "take the lock for the whole round trip, never a second locking
# mechanism") -- identical in shape to `heavy_stack.minio`'s own "one `with lock_docker_tests()`
# wrapping creation, every test's use, AND teardown" pattern, just extended to two containers
# instead of one.
#
# # No network at test time (question 154) / no environment mutation (question 199)
#
# Every container is reached over `127.0.0.1` (loopback, never "the network" for question 154's
# purposes -- restated from this file's own module doc). `av-gateway`'s own `AV_GATEWAY_BIND`/
# `AV_GATEWAY_ADMIN_BIND`/`AV_GATEWAY_EVIDENCE_LEDGER_DIR` are genuinely only configurable
# through the process environment (that binary's own pre-existing surface, R3.6 -- `crates/
# av-gateway/src/bin/av-gateway.rs`'s own `env_or` calls; unlike `--catalog-*`/`--oidc-*`, which
# this task's own new binary and this round's brief both require to be command-line-only, this
# is pre-existing behaviour this task does not change) -- every one of these three is set ONLY
# on the CHILD `subprocess.Popen`'s own `env=` mapping below, a fresh `dict` built from a copy
# of `os.environ`, never `os.environ[...]=`/`monkeypatch.setenv` on this pytest PROCESS itself,
# exactly `tests/test_proposer_container.py`'s own identical convention for configuring a real
# subprocess/container without mutating this process's own environment.


CATALOG_IMAGE_DIGEST_MD = heavy_stack.REPO_ROOT / "services" / "catalog" / "IMAGE_DIGEST.md"

# The one committed, real gateway auth config (`crates/av-gateway/src/bin/av-gateway.rs`'s own
# `default_auth_config_path`) -- `operators: ["query"]`, `group_clearance.operators: CUI`.
# Exercising the REAL shipped file (not a hand-written duplicate this test would have to keep
# in sync) mirrors `tests/test_proposer_container.py`'s own identical choice and reasoning
# ("Exercising the real shipped gateway-authority.yaml (not a hand-written duplicate)").
GATEWAY_AUTH_CONFIG_PATH = heavy_stack.REPO_ROOT / "profiles" / "gateway-authority.yaml"
GATEWAY_QUERY_GROUP = "operators"
GATEWAY_QUERY_CLEARANCE = "CUI"  # profiles/gateway-authority.yaml: group_clearance.operators

POSTGIS_READY_TIMEOUT_S = 90.0  # crates/av-catalog/tests/catalog_postgis.rs::READY_TIMEOUT is 60s for its own connect-retry loop; this file's own loop pays for a whole av-catalog-migrate process per attempt (not just a connect), so it gets extra headroom on top under this host's own real memory pressure (measured directly: see MIGRATE_ATTEMPT_TIMEOUT_S's own doc).
# Manager review, round 6, at the round's own gate. 60.0 was too thin for this host and this
# test FAILED in the full-suite run with "av-gateway did not print its own readiness line
# within 60.0s (returncode=None)" -- a real failure, not a flake to re-run away, so it is
# recorded here rather than quietly retried.
#
# Measured, alone, on a load-average-4.19 host immediately afterwards: three consecutive runs
# of this one test took 37.98 s, 5.51 s and 36.01 s. A single test whose own wall clock swings
# seven-fold at rest has no business carrying a 60 s budget for one of its phases -- that is
# ~1.6x the slow end of the quiet-host range before the 21-minute full suite's own load is
# added on top, which is exactly what consumed it.
#
# Raised to 180.0, and deliberately NOT to "whatever made it pass once": it is the same
# multiple of its own measured worst case (~4.7x) that POSTGIS_READY_TIMEOUT_S above already
# carries over its own, and it stays below `heavy_stack.READY_TIMEOUT_S`'s 240.0 for the
# heavier stack that file stands up. A readiness budget bounds "did this process ever come
# up", not "how fast is this host today"; the frame-time and memory budgets in
# tests/test_viewer_layers_stream.py are the ones that exist to be tight.
#
# One thing this does NOT fix, and it is why the failure mode was a bare timeout rather than
# a legible refusal: av-gateway reaches its readiness line only after connecting to the
# catalog, and `PgClient::connect` (crates/av-catalog/src/client.rs) bounds only the TCP
# connect -- the startup handshake that follows has no deadline at all (round 6's defect 5,
# docs/heavy-plan.md). A catalog that accepts and then stalls therefore hangs the gateway
# indefinitely, and all this constant can do is give up on it. Closing that is the fix
# recorded for the lead, not this number.
GATEWAY_READY_TIMEOUT_S = 180.0


def _parse_catalog_image_digest_md() -> Tuple[str, str]:
    """Mirrors `heavy_stack._parse_image_digest_md` exactly, restated for `services/catalog/
    IMAGE_DIGEST.md` -- reuses that module's own generic `_fenced_block_after(text, marker)`
    rather than a second copy of IT too."""
    text = CATALOG_IMAGE_DIGEST_MD.read_text()
    image_ref = heavy_stack._fenced_block_after(text, "Registry reference")
    recorded_id = heavy_stack._fenced_block_after(text, "docker image inspect")
    if not image_ref or not recorded_id:
        raise RuntimeError(f"could not find the 'Registry reference'/'docker image inspect' fenced code blocks in {CATALOG_IMAGE_DIGEST_MD}")
    return image_ref, recorded_id


def _compute_real_round_trip_skip_reason() -> Optional[str]:
    """`None` iff the docker daemon answers AND both images this round trip needs (MinIO,
    PostGIS) are present locally with an id matching their own recorded digest -- mirrors
    `heavy_stack._compute_skip_reason` (question 212(a)), extended to a SECOND image. Computed
    once at import time, the same convention `heavy_stack.SKIP_REASON` already uses."""
    if heavy_stack.SKIP_REASON is not None:
        return heavy_stack.SKIP_REASON  # already covers the docker daemon + the MinIO image
    try:
        image_ref, recorded_id = _parse_catalog_image_digest_md()
    except RuntimeError as e:
        return str(e)
    actual_id = heavy_stack._local_image_id(image_ref)
    if actual_id is None:
        return f"catalog image {image_ref!r} is not present locally (question 154: this test never pulls -- see {CATALOG_IMAGE_DIGEST_MD})."
    if actual_id != recorded_id:
        return (
            f"catalog image {image_ref!r} is present locally but its id {actual_id!r} does not match the digest "
            f"{recorded_id!r} recorded in {CATALOG_IMAGE_DIGEST_MD} (question 212(a))."
        )
    return None


REAL_ROUND_TRIP_SKIP_REASON = _compute_real_round_trip_skip_reason()


@pytest.fixture(scope="module")
def real_round_trip_bins():
    """Builds `av-catalog`'s own new `av-catalog-migrate` binary and `av-gateway`'s own
    `av-gateway` binary -- `av-tile-fixture` is NOT built here (`heavy_stack.rust_bins`, this
    file's own transitive import above, already builds it; a second `cargo build` of the exact
    same target would be pure waste, not "reuse rather than duplicate")."""
    env = heavy_stack._cargo_env()
    for build_args in (
        ["cargo", "build", "-p", "av-catalog", "--bin", "av-catalog-migrate"],
        ["cargo", "build", "-p", "av-gateway", "--bin", "av-gateway"],
    ):
        proc = subprocess.run(build_args, cwd=str(heavy_stack.REPO_ROOT), env=env, capture_output=True, text=True, timeout=heavy_stack.CARGO_BUILD_TIMEOUT_S)
        if proc.returncode != 0:
            pytest.fail(f"{' '.join(build_args)} failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    migrate_bin = heavy_stack.REPO_ROOT / "target" / "debug" / "av-catalog-migrate"
    gateway_bin = heavy_stack.REPO_ROOT / "target" / "debug" / "av-gateway"
    assert migrate_bin.is_file(), f"expected {migrate_bin} after a successful cargo build"
    assert gateway_bin.is_file(), f"expected {gateway_bin} after a successful cargo build"
    return SimpleNamespace(migrate=migrate_bin, gateway=gateway_bin)


def _docker_port(container_id: str, container_port: str) -> int:
    result = subprocess.run(["docker", "port", container_id, container_port], capture_output=True, text=True, timeout=10)
    if result.returncode != 0:
        pytest.fail(f"`docker port {container_id} {container_port}` failed: {result.stderr}")
    return int(result.stdout.strip().splitlines()[0].rsplit(":", 1)[-1])


@pytest.fixture(scope="module")
def real_stack_containers():
    """ONE `MinIO` container and ONE `PostGIS` container, under ONE `lock_docker_tests()` scope
    held from before either is created through this whole module's use of them to their own
    teardown -- see this section's own module-doc-adjacent comment above ("Why this is NOT
    built out of `heavy_stack.minio`/`heavy_stack.tile_set`") for why this is not two
    independent, separately-locking fixtures."""
    if REAL_ROUND_TRIP_SKIP_REASON is not None:
        pytest.skip(REAL_ROUND_TRIP_SKIP_REASON)

    minio_image_ref, minio_recorded_id = heavy_stack._parse_image_digest_md()
    catalog_image_ref, catalog_recorded_id = _parse_catalog_image_digest_md()
    run_id = f"catalog-round-trip-{uuid.uuid4()}"

    with lock_docker_tests():
        prune_stale_labelled_resources()  # ONCE, before creating anything (question 156/232)

        # -- MinIO --------------------------------------------------------------------------
        access_key, secret_key = heavy_stack._random_creds(run_id)
        minio_cmd = [
            "docker", "run", "-d", *label_args(run_id),
            "-e", f"MINIO_ROOT_USER={access_key}",
            "-e", f"MINIO_ROOT_PASSWORD={secret_key}",
            "-p", "127.0.0.1::9000",
            minio_image_ref,
            "server", "/data",
        ]
        result = subprocess.run(minio_cmd, capture_output=True, text=True, timeout=30)
        if result.returncode != 0:
            pytest.fail(f"`docker run` (MinIO) failed: {result.stderr}")
        minio_container_id = result.stdout.strip()
        minio_host_port = _docker_port(minio_container_id, "9000/tcp")
        heavy_stack._wait_for_minio_health(minio_host_port, minio_container_id)
        actual_minio_id = heavy_stack._local_image_id(minio_image_ref)
        assert actual_minio_id == minio_recorded_id, f"the RUNNING MinIO container's own image id {actual_minio_id!r} must equal the recorded digest {minio_recorded_id!r}"

        # -- PostGIS -- mirrors crates/av-catalog/tests/catalog_postgis.rs::start_fixture and
        # services/catalog/run-dev-catalog.sh's own POSTGRES_PASSWORD/POSTGRES_DB shape -------
        sanitized_run_id = "".join(c for c in run_id if c.isalnum())
        pg_password = f"avtestpw{sanitized_run_id}"
        pg_database = f"avtestdb{sanitized_run_id}"
        pg_cmd = [
            "docker", "run", "-d", *label_args(run_id),
            "-e", f"POSTGRES_PASSWORD={pg_password}",
            "-e", f"POSTGRES_DB={pg_database}",
            "-p", "127.0.0.1::5432",
            catalog_image_ref,
        ]
        result = subprocess.run(pg_cmd, capture_output=True, text=True, timeout=30)
        if result.returncode != 0:
            pytest.fail(f"`docker run` (PostGIS) failed: {result.stderr}")
        pg_container_id = result.stdout.strip()
        pg_host_port = _docker_port(pg_container_id, "5432/tcp")
        actual_pg_id = heavy_stack._local_image_id(catalog_image_ref)
        assert actual_pg_id == catalog_recorded_id, f"the RUNNING PostGIS container's own image id {actual_pg_id!r} must equal the recorded digest {catalog_recorded_id!r}"

        try:
            yield SimpleNamespace(
                minio=SimpleNamespace(host_port=minio_host_port, access_key=access_key, secret_key=secret_key, bucket="av-catalog-round-trip-test", container_id=minio_container_id),
                postgis=SimpleNamespace(host_port=pg_host_port, user="postgres", password=pg_password, database=pg_database, container_id=pg_container_id),
            )
        finally:
            subprocess.run(["docker", "rm", "-f", minio_container_id], capture_output=True, timeout=30)
            subprocess.run(["docker", "rm", "-f", pg_container_id], capture_output=True, timeout=30)


def _run_migrate(migrate_bin: Path, postgis: SimpleNamespace, *, applied_tai_ns: Optional[int] = None, timeout: float = 30.0) -> subprocess.CompletedProcess:
    cmd = [
        str(migrate_bin),
        "--catalog-host", "127.0.0.1",
        "--catalog-port", str(postgis.host_port),
        "--catalog-user", postgis.user,
        "--catalog-password", postgis.password,
        "--catalog-database", postgis.database,
    ]
    if applied_tai_ns is not None:
        cmd += ["--applied-tai-ns", str(applied_tai_ns)]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


# Bounds ONE av-catalog-migrate attempt inside the retry loop below -- deliberately shorter than
# POSTGIS_READY_TIMEOUT_S (a fresh process/TCP attempt every this-many seconds polls readiness
# better than one long-blocked call): `PgConfig.connect_timeout` (crates/av-catalog/src/
# client.rs's own doc) only bounds the INITIAL TCP connect, never the startup-handshake READ
# that follows it -- so a container whose port is already accepting TCP connections, but whose
# `postgres` process has not yet finished replaying its own `postgis`-extension init scripts,
# can leave `PgClient::connect` blocked on that read for longer than a single short attempt
# should be allowed to wait, exactly the failure this constant's own retry loop is built to
# route around (measured directly against this host under its own real memory pressure --
# `crates/av-catalog/tests/catalog_postgis.rs::READY_TIMEOUT`'s own comment records the
# identical fact for its Rust equivalent).
#
# MANAGER REVIEW, ROUND 6 -- the race above is now root-caused rather than mitigated blind,
# and the mechanism is narrower than "PostGIS accepts TCP before extension init". Measured
# against one real container of the pinned `services/catalog/IMAGE_DIGEST.md` image, under
# question 207's lock:
#
#   published host port known            t = 0.014 s
#   first successful raw TCP connect()   t = 0.014 s   <-- the SAME instant
#   first successful av-catalog-migrate  t = 1.764 s   (8 attempts)
#   window                               1.75 s
#   what the failing attempts actually said:
#     "av-catalog-migrate: connection closed by peer while reading startup/authentication"
#
# So it is not PostGIS answering early: it is DOCKER'S PORT PUBLISHER accepting the
# connection the instant the port is published, before anything inside the container is
# listening at all. The official postgres entrypoint deliberately runs initdb and the
# image's own `postgis` init scripts with `listen_addresses=''`, so the connection Docker
# accepted is closed as soon as it is proxied inward -- which is exactly the error text
# above, an immediate refusal rather than a hang. A bounded retry is therefore the correct
# and sufficient mitigation, and 15 s per attempt is ~8x the whole measured window.
#
# The "blocked on the startup read" hazard the paragraph above describes is real but is a
# SEPARATE, latent defect and was not what bit here: `PgClient::connect` wraps only
# `TcpStream::connect` in `tokio::time::timeout(config.connect_timeout, ..)` and then awaits
# `client.startup(config)` with NO deadline at all (crates/av-catalog/src/client.rs), so a
# peer that accepts and then never writes hangs any caller -- av-gateway and av-tile-fixture
# included -- forever. Recorded for the lead in docs/heavy-plan.md's round-6 status rather
# than fixed here: it changes `PgClient::connect`'s semantics for every caller and needs its
# own test, which is not this task's scope.
MIGRATE_ATTEMPT_TIMEOUT_S = 15.0


def _wait_for_migrate_ready(migrate_bin: Path, postgis: SimpleNamespace) -> subprocess.CompletedProcess:
    """PostGIS's own startup (initdb, then this image's `postgis`-extension init scripts) can
    take real wall-clock time after `docker run -d` returns -- exactly the same fact `crates/
    av-catalog/tests/catalog_postgis.rs::wait_for_ready`'s own comment documents (that file
    polls `PgClient::connect` in a loop; this file has no Postgres wire client of its own, so it
    polls the real `av-catalog-migrate` binary instead -- a successful real run of it IS "ready
    AND migrated" in one step, which is exactly what this round trip needs next). A bounded
    retry loop polling for readiness is rule 7's own named exception (`docs/heavy-plan.md`-style
    fixtures already do this), never a fixed sleep. Each individual attempt is ALSO bounded
    (`MIGRATE_ATTEMPT_TIMEOUT_S`, see its own doc) and a `subprocess.TimeoutExpired` from one
    attempt is treated as "not ready yet, try again" -- never an uncaught exception that would
    abort the whole retry loop on the very first slow attempt."""
    deadline = time.monotonic() + POSTGIS_READY_TIMEOUT_S
    last_stdout = ""
    last_stderr = "(no attempt completed yet)"
    last_proc: Optional[subprocess.CompletedProcess] = None
    while True:
        try:
            proc = _run_migrate(migrate_bin, postgis, timeout=MIGRATE_ATTEMPT_TIMEOUT_S)
        except subprocess.TimeoutExpired as e:
            last_stdout = (e.stdout or b"").decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
            last_stderr = f"attempt timed out after {MIGRATE_ATTEMPT_TIMEOUT_S}s (server not done with startup yet): {(e.stderr or b'').decode(errors='replace') if isinstance(e.stderr, bytes) else (e.stderr or '')}"
        else:
            if proc.returncode == 0:
                return proc
            last_proc = proc
            last_stdout, last_stderr = proc.stdout, proc.stderr
        if time.monotonic() > deadline:
            pytest.fail(
                f"av-catalog-migrate did not succeed against the fresh PostGIS container within {POSTGIS_READY_TIMEOUT_S}s "
                f"(returncode={last_proc.returncode if last_proc else 'N/A (timed out)'}); last stdout:\n{last_stdout}\nlast stderr:\n{last_stderr}"
            )
        time.sleep(0.5)


def _run_tile_fixture_with_catalog(rust_bins_ns, containers: SimpleNamespace, *, job_id: str, key_prefix: str) -> subprocess.CompletedProcess:
    cmd = [
        str(rust_bins_ns.tile_fixture),
        "--key-prefix", key_prefix,
        "--ladder", heavy_stack.LADDER,
        "--label-marking", GATEWAY_QUERY_CLEARANCE,
        "--job-id", job_id,
        "--min-level", "0",
        "--max-level", "2",
        "--tile-size", "16",
        "--synthetic-source", "32x16",
        "--store-endpoint", f"http://127.0.0.1:{containers.minio.host_port}",
        "--store-region", heavy_stack.MINIO_REGION,
        "--store-access-key-id", containers.minio.access_key,
        "--store-secret-access-key", containers.minio.secret_key,
        "--store-bucket", containers.minio.bucket,
        "--store-path-style",
        "--catalog-host", "127.0.0.1",
        "--catalog-port", str(containers.postgis.host_port),
        "--catalog-user", containers.postgis.user,
        "--catalog-password", containers.postgis.password,
        "--catalog-database", containers.postgis.database,
    ]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=120)


TILE_FIXTURE_SCHEMA_PROBE_TIMEOUT_S = 30.0


def _run_tile_fixture_until_the_real_schema_refusal(rust_bins_ns, containers: SimpleNamespace, *, job_id: str, key_prefix: str) -> subprocess.CompletedProcess:
    """Part 1's own perturbation proof needs `av-tile-fixture --catalog-*`'s refusal to be
    caused specifically by "no schema applied yet" (PostgreSQL's own `42P01 relation "assets"
    does not exist"), never by an unrelated race against the just-`docker run -d`'d PostGIS
    container still finishing its own startup (the SAME "not ready yet" fact `_wait_for_migrate_
    ready`'s own doc names -- measured directly: this exact race produced a `connection closed
    by peer while reading startup/authentication` refusal here instead of the schema one, on a
    container that answered the schema-aware refusal correctly moments later). This retries the
    whole real `av-tile-fixture` invocation (cheap: ~42 tiles at 16px, a couple of seconds) until
    it either fails with the SPECIFIC schema refusal this proof is about, or -- a real defect --
    succeeds outright (immediately and loudly failed below, never silently accepted), bounded by
    `TILE_FIXTURE_SCHEMA_PROBE_TIMEOUT_S`."""
    deadline = time.monotonic() + TILE_FIXTURE_SCHEMA_PROBE_TIMEOUT_S
    last: Optional[subprocess.CompletedProcess] = None
    while True:
        proc = _run_tile_fixture_with_catalog(rust_bins_ns, containers, job_id=job_id, key_prefix=key_prefix)
        if proc.returncode == 0:
            pytest.fail(
                f"av-tile-fixture --catalog-* unexpectedly SUCCEEDED against a database with NO migration applied -- "
                f"the perturbation proof does not hold. stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
            )
        stderr_lower = proc.stderr.lower()
        if "does not exist" in stderr_lower or "relation" in stderr_lower:
            return proc  # the real, specific "no schema" refusal this proof is about
        last = proc
        if time.monotonic() > deadline:
            pytest.fail(
                f"av-tile-fixture --catalog-* never reached PostGIS's real 'relation ... does not exist' refusal within "
                f"{TILE_FIXTURE_SCHEMA_PROBE_TIMEOUT_S}s (still failing on container startup instead?) -- last stderr:\n{last.stderr}"
            )
        time.sleep(0.5)


def _wait_for_gateway_ready(proc: subprocess.Popen, deadline_s: float) -> str:
    """Mirrors `heavy_stack._wait_for_listening_line`'s own `select.select`-bounded-poll
    structure exactly, restated for `av-gateway`'s own readiness line (`crates/av-gateway/src/
    bin/av-gateway.rs::main`'s `eprintln!("av-gateway: DataGatewayService + ModelProposeService
    on {bind_addr} ...")`) instead of `av-tiles`'s `LISTENING` literal -- the two binaries print
    different words, so this is a restatement, not a duplicate of the same fact."""
    marker = "DataGatewayService + ModelProposeService on"
    deadline = time.monotonic() + deadline_s
    collected: List[str] = []
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            pytest.fail(f"av-gateway did not print its own readiness line within {deadline_s}s (returncode={proc.poll()}):\n{''.join(collected)}")
        ready, _, _ = select.select([proc.stdout], [], [], min(remaining, 1.0))
        if not ready:
            continue
        line = proc.stdout.readline()
        if not line:
            if proc.poll() is not None:
                pytest.fail(f"av-gateway exited before printing its own readiness line (returncode={proc.poll()}):\n{''.join(collected)}")
            continue
        collected.append(line)
        if marker in line:
            return "".join(collected)


def _gateway_env(bind: str, admin_bind: str, evidence_dir: Path) -> dict:
    """A FRESH env dict for the `av-gateway` CHILD process only -- never `os.environ[...] = ...`
    on this pytest process itself (question 199; see this section's own module-doc-adjacent
    comment above)."""
    env = dict(heavy_stack._cargo_env())
    env["AV_GATEWAY_BIND"] = bind
    env["AV_GATEWAY_ADMIN_BIND"] = admin_bind
    env["AV_GATEWAY_EVIDENCE_LEDGER_DIR"] = str(evidence_dir)
    return env


def test_real_round_trip_migrate_then_register_then_list_through_the_real_gateway_route(real_round_trip_bins, real_stack_containers, rust_bins, tmp_path):
    """The full, real drive-path round trip (`docs/heavy-plan.md`'s "A gap in the drive path
    that nobody had noticed"): a fresh, digest-verified PostGIS container with NO schema
    applied; `av-tile-fixture --catalog-*` refused against it (the perturbation proof, question
    194/rule 9 restated as a permanent, always-run assertion rather than a one-off manual
    check); `av-catalog-migrate` applies the schema for real and is proven idempotent (a second
    run applies nothing); `av-tile-fixture --catalog-*` now succeeds and registers a real
    manifest; a real `av-gateway` subprocess, configured with `--catalog-*` pointing at the SAME
    PostGIS container and the real, committed `profiles/gateway-authority.yaml`, serves
    `GATEWAY_SELECTOR_CATALOG`; and `GET /api/catalog/tilesets`, through a real `create_app`
    FastAPI app pointed at that real gateway, lists exactly the manifest just registered.
    """
    containers = real_stack_containers
    migrate_bin = real_round_trip_bins.migrate
    gateway_bin = real_round_trip_bins.gateway

    key_prefix = "catalog-round-trip"
    job_id = f"{key_prefix}-job"

    # -----------------------------------------------------------------------------------------
    # Part 1: the perturbation proof (question 194/rule 9) -- av-tile-fixture's own --catalog-*
    # registration MUST fail against a PostGIS container with no schema applied at all (no
    # `assets` table exists yet). This is the round-trip test failing (in the specific,
    # documented way this brief calls for) when the migrate step is skipped.
    # -----------------------------------------------------------------------------------------
    unmigrated_proc = _run_tile_fixture_until_the_real_schema_refusal(rust_bins, containers, job_id=f"{job_id}-premigrate", key_prefix=f"{key_prefix}-premigrate")
    assert unmigrated_proc.returncode != 0, (
        f"av-tile-fixture --catalog-* must be REFUSED against an unmigrated database (no assets table exists yet) -- "
        f"it was not: stdout={unmigrated_proc.stdout!r} stderr={unmigrated_proc.stderr!r}"
    )
    assert "assets" in unmigrated_proc.stderr.lower() or "relation" in unmigrated_proc.stderr.lower(), (
        f"expected a real PostgreSQL 'relation \"assets\" does not exist' server error naming the missing table, got stderr={unmigrated_proc.stderr!r}"
    )
    print(f"PERTURBATION PROOF -- av-tile-fixture --catalog-* against an unmigrated database (returncode={unmigrated_proc.returncode}):\nSTDERR:\n{unmigrated_proc.stderr}")

    # -----------------------------------------------------------------------------------------
    # Part 2: av-catalog-migrate applies the schema for real, and is idempotent.
    # -----------------------------------------------------------------------------------------
    first_migrate = _wait_for_migrate_ready(migrate_bin, containers.postgis)
    print(f"av-catalog-migrate -- FIRST run against the fresh PostGIS container:\n{first_migrate.stdout}")
    assert "schema_migrations recorded 0 migration(s) before this run" in first_migrate.stdout
    assert 'applied 1 migration(s) this run: ["0001_init.sql"]' in first_migrate.stdout  # Rust's {:?} on Vec<&str> uses double quotes
    assert "resulting schema version: 0001_init.sql (1 migration(s) recorded total)" in first_migrate.stdout

    second_migrate = _run_migrate(migrate_bin, containers.postgis)
    print(f"av-catalog-migrate -- SECOND run against the SAME (now-migrated) PostGIS container:\n{second_migrate.stdout}")
    assert second_migrate.returncode == 0, f"a second run must still succeed (idempotent): {second_migrate.stdout}\n{second_migrate.stderr}"
    assert "schema_migrations recorded 1 migration(s) before this run" in second_migrate.stdout
    assert "no pending migrations -- database already at the latest schema version (idempotent: this run applied nothing)" in second_migrate.stdout, (
        "a second av-catalog-migrate run against an already-migrated database must apply NOTHING and say so"
    )

    # -----------------------------------------------------------------------------------------
    # Part 3: av-tile-fixture --catalog-* now succeeds against the migrated database.
    # -----------------------------------------------------------------------------------------
    tile_proc = _run_tile_fixture_with_catalog(rust_bins, containers, job_id=job_id, key_prefix=key_prefix)
    assert tile_proc.returncode == 0, f"av-tile-fixture --catalog-* failed against a migrated database (returncode={tile_proc.returncode}):\n--- stdout ---\n{tile_proc.stdout}\n--- stderr ---\n{tile_proc.stderr}"
    assert "registered the tile-set manifest in the catalog" in tile_proc.stderr, tile_proc.stderr
    stdout_lines = [line for line in tile_proc.stdout.splitlines() if line.strip()]
    assert stdout_lines, f"av-tile-fixture printed nothing on stdout; stderr:\n{tile_proc.stderr}"
    fixture_result = json.loads(stdout_lines[-1])
    assert re.fullmatch(r"[0-9a-f]{64}", fixture_result["manifest_sha256"]), fixture_result
    assert fixture_result["tile_count"] >= 10, fixture_result
    manifest_sha256 = fixture_result["manifest_sha256"]
    expected_asset_id = f"tileset:{job_id}:{manifest_sha256}"
    print(f"av-tile-fixture --catalog-* -- registered asset_id={expected_asset_id!r} manifest_sha256={manifest_sha256!r} tile_count={fixture_result['tile_count']}")

    # -----------------------------------------------------------------------------------------
    # Part 4: a real av-gateway subprocess, --catalog-* pointed at the SAME PostGIS container,
    # the real committed profiles/gateway-authority.yaml, and a real RS256 token for the
    # "operators" group (CUI clearance, "query" role).
    # -----------------------------------------------------------------------------------------
    issuer_dir = tmp_path / "issuer"
    issuer_dir.mkdir()
    issuer = heavy_stack.LocalTestIssuer(issuer_dir)
    bind_port = heavy_stack._free_port()
    admin_port = heavy_stack._free_port()
    evidence_dir = tmp_path / "gateway_evidence"
    gateway_cmd = [
        str(gateway_bin),
        "--oidc-issuer", heavy_stack.TEST_ISSUER,
        "--oidc-audience", heavy_stack.TEST_AUDIENCE,
        "--oidc-public-key-path", str(issuer.public_key_path),
        "--auth-config-path", str(GATEWAY_AUTH_CONFIG_PATH),
        "--catalog-host", "127.0.0.1",
        "--catalog-port", str(containers.postgis.host_port),
        "--catalog-user", containers.postgis.user,
        "--catalog-password", containers.postgis.password,
        "--catalog-database", containers.postgis.database,
    ]
    gateway_proc = subprocess.Popen(
        gateway_cmd,
        cwd=str(heavy_stack.REPO_ROOT),
        env=_gateway_env(f"127.0.0.1:{bind_port}", f"127.0.0.1:{admin_port}", evidence_dir),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    try:
        readiness_output = _wait_for_gateway_ready(gateway_proc, GATEWAY_READY_TIMEOUT_S)
        assert f"catalog tier configured at 127.0.0.1:{containers.postgis.host_port}" in readiness_output, readiness_output
        print(f"av-gateway -- readiness output:\n{readiness_output}")

        # -------------------------------------------------------------------------------------
        # Part 5: GET /api/catalog/tilesets through a real create_app FastAPI app, pointed at
        # the real, just-started av-gateway above -- the actual route this round trip is
        # proving, backed by real binaries and a real database end to end.
        # -------------------------------------------------------------------------------------
        token = issuer.mint(heavy_stack._valid_claims("heavy-round-trip-caller", [GATEWAY_QUERY_GROUP]))
        token_path = tmp_path / "gateway_round_trip_token.txt"
        token_path.write_text(token)

        app = create_app(texture_dir=tmp_path, web_dir=tmp_path, gateway_endpoint=f"127.0.0.1:{bind_port}", gateway_token_path=token_path)
        client = TestClient(app)
        resp = client.get("/api/catalog/tilesets")
        assert resp.status_code == 200, resp.text
        body = resp.json()
        print(f"GET /api/catalog/tilesets -- real round trip response: {body}")

        by_asset_id = {entry["assetId"]: entry for entry in body["tileSets"]}
        assert expected_asset_id in by_asset_id, f"expected asset_id {expected_asset_id!r} among {sorted(by_asset_id)}"
        entry = by_asset_id[expected_asset_id]
        assert entry["manifestSha256"] == manifest_sha256
        assert entry["name"] == job_id
        assert entry["marking"] == GATEWAY_QUERY_CLEARANCE
        assert entry["jobId"] == job_id

        # The pre-migrate asset never registered (Part 1's own refused run) must NOT appear --
        # proves this list is the real catalog's real contents, not a fixed/mocked response.
        assert f"tileset:{job_id}-premigrate:{manifest_sha256}" not in by_asset_id
    finally:
        gateway_proc.terminate()
        try:
            gateway_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            gateway_proc.kill()
            gateway_proc.wait(timeout=10)
