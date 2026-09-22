"""Real gRPC client for `altavista.v1.DataGatewayService`'s `GATEWAY_SELECTOR_CATALOG`
selector (`crates/av-gateway/src/catalog_selector.rs::query_catalog`), backing the
`GET /api/catalog/tilesets` route `altavista/server.py` declares -- question 228's finding 2,
server half: "a catalog listing the viewer can read". This module never re-implements any
part of the catalog tier's own query semantics: every function below is a thin adapter over a
REAL, already-running `av-gateway` gRPC service, never a Python-side reader of `crates/
av-catalog`'s own PostgreSQL wire protocol.

# Design rules, mirrored from `altavista/command_client.py`'s own template

This module is `command_client.py`'s own sibling for a third real backend service (after the
command authority and the tiles gateway), restated for `DataGatewayService.Query`'s
`GATEWAY_SELECTOR_CATALOG` payload instead of `CommandAuthorityService`:

- The gRPC endpoint and the token FILE PATH are CONFIGURATION (`GatewayServiceConfig`, built
  once and handed to `create_app`/`serve`, question 199), never a request parameter, header,
  cookie, or body of the INCOMING request -- exactly `altavista/tiles_client.py`'s own rule for
  its bearer token, restated here for a gRPC request field instead of an HTTP header. There is
  no code path from an HTTP request this server receives to the `grpc.insecure_channel(...)`
  target this module dials.
- **The token is read from `token_path` AT REQUEST TIME**, never cached across requests, and
  is placed ONLY in the outbound `GatewayQueryRequest.caller_token` field this module builds.
  It is NEVER taken from a query parameter, a request header, a cookie, or a request body of
  the incoming request -- `altavista/server.py`'s own route never even reads an incoming
  `Authorization` header (mirrors that module's own `_forwarded_request_headers` for
  `/api/tiles/*`, restated for this route: there is nothing on this route's own allowlist,
  because there is nothing to forward at all).
- **`caller_clearance` is always sent EMPTY.** `proto/altavista/v1/authority.proto`'s own
  `GatewayQueryRequest.caller_clearance` doc (field 2) is explicit: the gateway verifies
  `caller_token` through the same `av_command::oidc::verify` path `AuthorizeRequest.
  principal_token` uses, and derives the caller's effective clearance from the verified
  token's groups; `caller_clearance` is never itself trusted as identity -- when non-empty it
  must AGREE with the token-derived clearance or the request is refused (never silently
  preferring the higher or the lower of the two), and when empty (this module's own choice,
  always) the token-derived clearance is used. Sending it empty is therefore the honest call:
  this module has no clearance of its own to assert, and asserting one it invented would only
  ever be redundant (agreeing) or refused (disagreeing) -- never useful.
- `caller_supplied_products_uri` is NEVER set (left at its wire default, the empty string): a
  non-empty value there is D1's own caller-supplied-path attack signature, refused outright by
  `crate::catalog_selector::query_catalog` before any other validation runs. This module has no
  reason to ever set it and does not.
- `grpcio` stays an optional extra (`pyproject.toml`'s `[project.optional-dependencies].grpc`)
  -- importing this module, or `altavista.server`, must never require it to be installed
  (question 85). `grpc` and the generated `authority_pb2_grpc` stub are imported at MODULE
  scope inside a `try`/`except ImportError`, mirroring `command_client.py`'s own identical
  convention -- never a deferred import inside a route handler.
- Every way this module can fail to answer at all -- no config, grpcio absent, the service
  configured but unreachable (including a `token_path` this process cannot read), or the
  gateway's own typed refusal -- is its own `GatewayServiceError` subclass, carrying enough for
  a caller to answer with a specific HTTP status and a real message (never a flattened generic
  500). A real refusal from the gateway (`GatewayRefusedError`) carries the gateway's OWN
  `grpc.StatusCode` and message VERBATIM -- never flattened to a generic failure, exactly
  `command_client.CommandServiceRpcError`'s own contract.
- No token, anywhere else. `list_tile_sets` reads the token from `config.token_path`, places it
  on the one `GatewayQueryRequest.caller_token` field, and this module never stores, caches, or
  logs it.

# Why this route is safe to leave unfiltered on `bbox`/`time`/`job_id`

`list_tile_sets` sends a `CatalogQuery` with `media_type` set (see `TILESET_MANIFEST_MEDIA_
TYPE` below) and every other filter field left at its wire default (`bbox`/`time`/`job_id`
unset -- "no filter", `heavy.proto`'s own documented default for each): the listing panel this
route backs (the browser half's own Layers panel, a later task) wants every tile set the
caller's clearance may see, not a spatial/temporal/job subset. The gateway's own label
enforcement (D2, `av_catalog::query::find_assets`'s SQL-level `WHERE marking = ANY($1)`) still
applies exactly as it does to any other catalog query -- this module adds no clearance logic of
its own on top.
"""
from __future__ import annotations

import dataclasses
import logging
import os
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .pb import authority_pb2, heavy_pb2

try:
    import grpc
except ImportError:  # grpcio is the optional "grpc" extra (pyproject.toml) -- see module doc.
    grpc = None  # type: ignore[assignment]

log = logging.getLogger("altavista.gateway_client")

DEFAULT_TIMEOUT_S = 10.0

# H2c's own `CatalogQuery.limit` doc (`heavy.proto`): refused, never clamped, at 0 or above
# `av_catalog::MAX_QUERY_LIMIT` (that crate's own constant, currently 1000 -- see
# `crates/av-catalog/src/query.rs`). This module has no way to import a Rust `pub const`
# across the language boundary, so this is a second, independent copy of that same number --
# unlike `TILESET_MANIFEST_MEDIA_TYPE` below, a STALE copy here fails LOUD (a `CatalogRefusal::
# LimitOutOfRange`, surfaced as a real 400 through `gateway_service_error_to_http_status`),
# never silently, so drift is self-correcting the first time this route is ever called after
# `MAX_QUERY_LIMIT` changes.
DEFAULT_LIST_LIMIT = 1000

# `crates/av-jobs/src/tiler.rs:92`'s `MANIFEST_MEDIA_TYPE` and `crates/av-tiles/src/config.rs
# :16`'s identical restatement of the same value (that module's own doc explains why it is
# restated rather than imported: `av-tiles` does not depend on `av-jobs`). This is therefore a
# THIRD copy of the same literal -- unavoidable across the Rust/Python boundary, since this
# module cannot import a Rust `pub const` any more than `av-tiles` can. Unlike `DEFAULT_LIST_
# LIMIT` above, a stale copy of THIS constant fails SILENTLY (the catalog query would simply
# match nothing, forever, with no error) -- see this task's own report for why introducing a
# fourth, generated copy was judged not worth the build-time machinery for one string, and for
# what the manager should check if tile sets ever stop appearing in this route's own listing.
TILESET_MANIFEST_MEDIA_TYPE = "application/vnd.altavista.tileset-manifest+pb"


class GatewayServiceError(Exception):
    """Base class for every typed way this module can fail to answer `GET /api/catalog/
    tilesets`. Callers catch this, never a bare `Exception` -- see the module doc."""


class GrpcioNotInstalledError(GatewayServiceError):
    """`grpcio` (the `[grpc]` extra) is not importable in this interpreter."""

    def __init__(self) -> None:
        super().__init__(
            "the catalog listing route needs the optional 'grpc' extra "
            "(pip install -e '.[grpc]') -- grpcio is not installed in this interpreter"
        )


class GatewayNotConfiguredError(GatewayServiceError):
    """No `--gateway-endpoint`/`--gateway-token-path` was given at `serve`/`create_app`
    startup. Never surfaced as an empty list -- an unconfigured gateway and a gateway with no
    tile sets catalogued are two different facts, and a caller (the Layers panel) needs to
    tell them apart."""

    def __init__(self) -> None:
        super().__init__(
            "no data gateway is configured for this viewer server (pass --gateway-endpoint/"
            "--gateway-token-path to `python -m altavista serve`)"
        )


class GatewayUnreachableError(GatewayServiceError):
    """The endpoint IS configured, but a real connection attempt did not succeed within
    `GatewayServiceConfig.timeout_s`, or `token_path` could not be read."""

    def __init__(self, endpoint: str, detail: str) -> None:
        self.endpoint = endpoint
        self.detail = detail
        super().__init__(f"data gateway at {endpoint!r} is configured but unreachable: {detail}")


class GatewayRefusedError(GatewayServiceError):
    """A REAL refusal from the running `av-gateway` service: its own `grpc.StatusCode` and
    message, carried verbatim -- never flattened to a generic failure (mirrors `command_client.
    CommandServiceRpcError`). `code` is a real `grpc.StatusCode` member (raising this always
    means a real RPC already completed with a non-OK status, so grpcio is necessarily
    importable at that point)."""

    def __init__(self, code: Any, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"{code.name}: {message}")


@dataclasses.dataclass(frozen=True)
class GatewayServiceConfig:
    """Where `GET /api/catalog/tilesets` reaches the real `av-gateway` `DataGatewayService` --
    built once and handed to `create_app`/`serve` (question 199: never the process
    environment; never a request parameter -- see the module doc). `endpoint` is a plain
    `"host:port"` string, exactly what `av-gateway --bind` itself takes. `token_path` names a
    file this module reads fresh on every call (see the module doc) -- never a token value
    itself."""

    endpoint: Optional[str] = None
    token_path: Optional[os.PathLike] = None
    timeout_s: float = DEFAULT_TIMEOUT_S


def _require_grpc() -> None:
    if grpc is None:
        raise GrpcioNotInstalledError()


def _require_configured(config: GatewayServiceConfig) -> None:
    if not config.endpoint or not config.token_path:
        raise GatewayNotConfiguredError()


def _read_token(config: GatewayServiceConfig) -> str:
    """Reads the bearer/OIDC token from `config.token_path` FRESH, on every call -- see the
    module doc. Raises `GatewayUnreachableError` (never leaks the file's own content -- only
    the path itself, and the OS error, in the message) if it cannot be read."""
    assert config.token_path is not None  # _require_configured already checked this
    try:
        return Path(config.token_path).read_text(encoding="utf-8").strip()
    except OSError as exc:
        raise GatewayUnreachableError(config.endpoint or "", f"could not read --gateway-token-path {config.token_path!r}: {exc}") from None


def _stub(config: GatewayServiceConfig):
    """A fresh `DataGatewayServiceStub` over a fresh `grpc.insecure_channel` -- never memoized
    across calls, mirroring `command_client._stub`'s own reasoning: a channel is cheap and
    lazy on its own, so there is no lifecycle this module needs to manage. Plaintext on
    loopback only (question 155) -- this module places no TLS of any kind in front of it."""
    _require_grpc()
    if not config.endpoint:
        raise GatewayNotConfiguredError()
    from .pb import authority_pb2_grpc  # None when grpcio is absent (altavista/pb/__init__.py)

    if authority_pb2_grpc is None:
        raise GrpcioNotInstalledError()
    channel = grpc.insecure_channel(config.endpoint)
    return authority_pb2_grpc.DataGatewayServiceStub(channel)


def _call(config: GatewayServiceConfig, method_name: str, request: Any) -> Any:
    """The ONE call site `list_tile_sets` goes through: builds a fresh stub, calls
    `method_name` with `request` under `config.timeout_s`, and maps every way it can fail to
    the typed `GatewayServiceError` subclass this module's callers expect -- mirrors
    `command_client._call` exactly, including which two status codes mean "unreachable" rather
    than "the service refused this specific request"."""
    stub = _stub(config)
    rpc = getattr(stub, method_name)
    try:
        return rpc(request, timeout=config.timeout_s)
    except grpc.RpcError as exc:
        code = exc.code()
        if code in (grpc.StatusCode.UNAVAILABLE, grpc.StatusCode.DEADLINE_EXCEEDED):
            raise GatewayUnreachableError(config.endpoint or "<unset>", f"{code.name}: {exc.details()}") from None
        raise GatewayRefusedError(code, exc.details() or "") from None


def _int64(value: int) -> str:
    """An `int64`/`uint64` rendered for JSON as a decimal STRING, not a number -- see
    `command_client._int64`'s own docstring for the full "silently rounds and nothing says so"
    reasoning (TAI nanosecond epochs are around 1.77e18, comfortably past `Number.
    MAX_SAFE_INTEGER`). Applied here to BOTH `created_tai_ns` (a TAI epoch, the brief's own
    named case) and `size_bytes` (a `uint64` that is not an epoch, but is drawn from the exact
    same "past 2**53 and nothing tells you" danger zone for a large enough tile set) -- the
    identical failure mode, so the identical fix, applied consistently rather than only where
    the brief happened to name it by example.
    """
    return str(int(value))


def _catalog_record_to_dict(record: Any) -> Dict[str, Any]:
    """One `heavy_pb2.CatalogRecord` (a tile set's manifest, since `list_tile_sets` only ever
    queries `media_type == TILESET_MANIFEST_MEDIA_TYPE`) as the JSON shape the browser's own
    Layers panel (a later task, against this route) reads. Carries at least what that task's
    own brief requires: the manifest's sha256 (the tile set's identity -- what the browser
    passes to `/api/tiles/<sha>/...`), a human label, the marking, and the size.

    `name` is `record.job_id` -- `av-tile-fixture`'s own `--job-id` is the one caller-chosen,
    human-legible identifier a tile set carries (`heavy.proto`'s `CatalogRecord`/`av_catalog::
    model::CatalogAsset` have no separate "display name" field at all, and this route adds no
    new proto field to invent one -- see this task's own report). Falls back to `asset_id`
    (this catalog's own primary key, still caller-chosen and stable) for the pathological case
    of a record with no `job_id` recorded, so `name` is never empty for a well-formed record.
    """
    asset = record.asset
    label = asset.label
    return {
        "assetId": record.asset_id,
        "manifestSha256": asset.sha256,
        "name": record.job_id or record.asset_id,
        "marking": label.marking,
        "caveats": list(label.caveats),
        "sizeBytes": _int64(asset.size_bytes),
        "mediaType": asset.media_type,
        "uri": asset.uri,
        "jobId": record.job_id,
        "createdTaiNs": _int64(record.created_tai_ns),
        "footprintWkt": record.footprint_wkt,
    }


def list_tile_sets(config: GatewayServiceConfig, limit: int = DEFAULT_LIST_LIMIT) -> List[Dict[str, Any]]:
    """Every tile-set manifest the caller's clearance may see -- one `GatewayQueryRequest`
    (`GATEWAY_SELECTOR_CATALOG`, `CatalogQuery.media_type == TILESET_MANIFEST_MEDIA_TYPE`,
    every other `CatalogQuery` filter left unset -- see the module doc, "Why this route is
    safe to leave unfiltered") against the real gateway, token read FRESH from `config.
    token_path` on every call and placed on `caller_token` -- never cached, never taken from
    anything an incoming HTTP request carries (see the module doc). `caller_clearance` is
    always sent empty (module doc); `caller_supplied_products_uri` is never set.

    Ordered `asset_id`-ascending -- `authority.proto`'s own `GatewayQueryResponse.catalog_
    records` doc: `av_catalog::find_assets`'s own `ORDER BY asset_id`, unchanged by this
    module.

    Raises a `GatewayServiceError` subclass for every way this module itself can fail to
    answer at all (no config, grpcio absent, unreachable, or the gateway's own typed refusal,
    including `CatalogRefusal::CatalogNotConfigured` when the DEPLOYMENT has no catalog tier
    wired up -- a real, distinct refusal from `GatewayNotConfiguredError`, which means THIS
    server has no `--gateway-endpoint` at all). Never returns an empty list to mean "I could
    not answer" -- an empty list from this function always means a real, successful query that
    matched zero tile sets.
    """
    _require_configured(config)
    token = _read_token(config)
    request = authority_pb2.GatewayQueryRequest(
        caller_clearance="",
        selector=authority_pb2.GATEWAY_SELECTOR_CATALOG,
        caller_token=token,
        catalog_query=heavy_pb2.CatalogQuery(media_type=TILESET_MANIFEST_MEDIA_TYPE, limit=limit),
    )
    response = _call(config, "Query", request)
    return [_catalog_record_to_dict(r) for r in response.catalog_records]


def gateway_service_error_to_http_status(exc: GatewayServiceError) -> Tuple[int, str]:
    """Maps any `GatewayServiceError` to an `(http_status, message)` pair --
    `altavista/server.py`'s route handler turns this straight into a `fastapi.HTTPException`.
    A real `GatewayRefusedError` keeps the real service's own status code's ordinary gRPC
    meaning (`crates/av-gateway/src/gateway.rs`'s own `to_status`/`auth::to_status` name each
    one this selector can produce); every other subclass (no config, grpcio absent,
    unreachable) is `503 Service Unavailable` -- this server genuinely cannot serve the route
    right now, for a reason named in the message. Mirrors `command_client.command_service_
    error_to_http_status` exactly, restated for the status codes THIS selector's own refusal
    chain (`crate::catalog_selector::to_status`) can actually produce; an unmapped code (there
    is none reachable today, but a future refusal reason might add one) falls through to `502
    Bad Gateway` -- this server reached the real gateway and got a real answer, just not one
    already in this table, so "the upstream said something this proxy does not recognise" (502)
    is the honest code, never a flattened 500 or a guessed 4xx."""
    if isinstance(exc, GatewayRefusedError):
        status = {
            "INVALID_ARGUMENT": 400,
            "NOT_FOUND": 404,
            "FAILED_PRECONDITION": 409,
            "PERMISSION_DENIED": 403,
            "UNAUTHENTICATED": 401,
        }.get(exc.code.name, 502)
        return status, exc.message or exc.code.name
    return 503, str(exc)
