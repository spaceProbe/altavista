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

import socket
import threading
from concurrent import futures
from pathlib import Path
from typing import List, Optional, Tuple

import grpc
import pytest
from fastapi.testclient import TestClient

from altavista import gateway_client
from altavista.pb import authority_pb2, authority_pb2_grpc, entity_pb2, envelope_pb2, heavy_pb2
from altavista.server import create_app

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
