"""H5b-1 (`docs/heavy-plan.md` H5, round 3, "the served through the viewer server with the
gateway's authentication" half): a thin `httpx` proxy client for the real `av-tiles` gateway
(`crates/av-tiles`), backing the `GET /api/tiles/*` routes `altavista/server.py` declares.

# Why the viewer never talks to `av-tiles` directly (question 51)

`docs/open-questions.md` question 51: the viewer must run "fully self-contained... CI runs the
viewer with egress blocked to prove it" -- no fetch to any origin but the one that served the
page. So the browser only ever calls `/api/tiles/...` on THIS server; this module is the one
place that dials out to the separate `av-tiles` gateway process, over `httpx`, on the server's
own behalf.

# Design rules, mirrored from `altavista/command_client.py`'s own template

This module is `command_client.py`'s own sibling for a second real backend service, restated
for a bearer token and a byte-stream proxy instead of a gRPC stub and a JSON adapter:

- The gateway endpoint and the token FILE PATH are CONFIGURATION (`TilesServiceConfig`, built
  once and handed to `create_app`/`serve`, question 199), never a request parameter, header,
  cookie, or body of the INCOMING request. There is no code path from an HTTP request this
  server receives to the `httpx` target this module dials -- a caller of `/api/tiles/*` cannot
  make this server proxy to an arbitrary host.
- **Rule 1 (this task's own brief): the bearer token is read from `tiles_token_path` AT
  REQUEST TIME**, never cached across requests -- so rotating the file on disk takes effect on
  the very next request -- and is placed ONLY in the outbound `Authorization: Bearer ...`
  header this module sends to the gateway. It is NEVER taken from a query parameter, a request
  header, a cookie, or a request body of the incoming request: a caller-supplied credential
  would let a browser choose its own clearance, exactly what question 45's per-layer label
  enforcement in the gateway exists to prevent. `altavista/server.py`'s own two routes read
  ONLY `Range`/`If-None-Match` off the incoming request (see that module's own `_forwarded_
  request_headers`) -- an incoming `Authorization` header, if a browser sent one at all, is
  never even looked at by this module or by those routes.
- **Rule 5: no URL this module builds ever carries the token, or any other secret.** The
  token lives in exactly one place outside its own file: the `Authorization` request header
  this module sends to the gateway. `_build_url` below joins only `config.endpoint` and the
  caller-supplied `path` (itself built entirely from path parameters this server's own routes
  already validated the shape of -- a manifest hash, a level/x/y triple -- never from anything
  containing the token).
- **Rule 2: the token appears nowhere observable.** This module logs nothing that could carry
  it (no request/response dump, ever), and forwards back only a fixed, small allowlist of
  RESPONSE headers from the gateway (`_PASSTHROUGH_RESPONSE_HEADERS` below) -- the gateway
  itself never echoes a caller's own `Authorization` back in a response header in the first
  place (`crates/av-tiles/src/server.rs`'s own `response_bytes` never writes one), so this is
  a second, independent gate on top of that, not the only one.
- Every way this module can fail to answer at all -- no config, or the gateway configured but
  unreachable -- is its own `TilesServiceError` subclass; `altavista/server.py`'s route
  handlers catch this (never a bare `Exception`) and map it through
  `tiles_service_error_to_http_status`, the same shape `command_client.command_service_error_
  to_http_status` already established for the command console routes.
- **Question 199, restated for this module specifically**: nothing here reads or mutates
  `os.environ`. `httpx.Client(trust_env=False)` is passed explicitly on every call so this
  module never picks up `HTTP_PROXY`/`NO_PROXY`/`SSL_CERT_FILE`/etc from the process
  environment either -- the ONE environment-shaped read in this whole module is
  `tiles_token_path`'s own file content, and that path is itself a `create_app` argument, not
  an environment variable.
"""
from __future__ import annotations

import dataclasses
import logging
import os
from pathlib import Path
from typing import Dict, Optional, Tuple

import httpx

log = logging.getLogger("altavista.tiles_client")

DEFAULT_TIMEOUT_S = 10.0

# Response headers from the real av-tiles gateway this module forwards back verbatim
# (this task's own rule 4: "the gateway's status code, ETag, Cache-Control and Content-Type
# are passed through"; Content-Range for a 206/416). Case-insensitive lookup against
# `httpx.Headers`, which is itself already case-insensitive. Deliberately excludes
# `Connection`/`Date`/every other transport-shaped header a raw proxy could otherwise
# forward by accident -- and, as important as any of the four it does forward, excludes
# `Authorization`/`WWW-Authenticate`: the gateway never sends either back (see the module
# doc's "Rule 2"), and this allowlist is this module's own second, independent guarantee of
# that even if it someday did.
_PASSTHROUGH_RESPONSE_HEADERS: Tuple[str, ...] = ("etag", "cache-control", "content-type", "content-range")

# Incoming-request headers `altavista/server.py`'s own two routes forward to the gateway --
# see the module doc's "Rule 1": never `Authorization`, never anything else.
PASSTHROUGH_REQUEST_HEADERS: Tuple[str, ...] = ("range", "if-none-match")


class TilesServiceError(Exception):
    """Base class for every typed way this module can fail to answer an `/api/tiles/*` route.
    Callers catch this, never a bare `Exception` -- see the module doc."""


class TilesNotConfiguredError(TilesServiceError):
    """No `tiles_endpoint`/`tiles_token_path` was given at `serve`/`create_app` startup."""

    def __init__(self) -> None:
        super().__init__(
            "no tiles gateway is configured for this viewer server (pass --tiles-endpoint/"
            "--tiles-token-path to `python -m altavista serve`)"
        )


class TilesUnreachableError(TilesServiceError):
    """The endpoint IS configured, but a real connection attempt did not succeed within
    `TilesServiceConfig.timeout_s`, or `tiles_token_path` could not be read."""

    def __init__(self, endpoint: str, detail: str) -> None:
        self.endpoint = endpoint
        self.detail = detail
        super().__init__(f"tiles gateway at {endpoint!r} is configured but unreachable: {detail}")


@dataclasses.dataclass(frozen=True)
class TilesServiceConfig:
    """Where the `/api/tiles/*` routes reach the real `av-tiles` gateway -- built once and
    handed to `create_app`/`serve` (question 199: never the process environment; never a
    request parameter -- see the module doc). `endpoint` is a plain `"host:port"` string,
    exactly what `av-tiles --bind` itself takes. `token_path` names a file this module reads
    fresh on every proxied request (see the module doc's "Rule 1") -- never a token value
    itself."""

    endpoint: Optional[str] = None
    token_path: Optional[os.PathLike] = None
    timeout_s: float = DEFAULT_TIMEOUT_S


class TilesResponse:
    """What [`proxy_get`] returns: exactly enough for `altavista/server.py`'s own route to
    build a `fastapi.Response` byte for byte from the gateway's own answer -- never a `dict`/
    JSON reinterpretation of what is, for the tile route, opaque binary tile bytes."""

    def __init__(self, status_code: int, headers: Dict[str, str], content: bytes) -> None:
        self.status_code = status_code
        self.headers = headers
        self.content = content


def _require_configured(config: TilesServiceConfig) -> None:
    if not config.endpoint or not config.token_path:
        raise TilesNotConfiguredError()


def _read_token(config: TilesServiceConfig) -> str:
    """Reads the bearer token from `config.token_path` FRESH, on every call -- see the module
    doc's "Rule 1". Raises `TilesUnreachableError` (never leaks the file's own content -- only
    the path itself, and the OS error, in the message) if it cannot be read."""
    assert config.token_path is not None  # _require_configured already checked this
    try:
        return Path(config.token_path).read_text(encoding="utf-8").strip()
    except OSError as exc:
        raise TilesUnreachableError(config.endpoint or "", f"could not read --tiles-token-path {config.token_path!r}: {exc}") from None


def _build_url(config: TilesServiceConfig, path: str) -> str:
    """Joins `config.endpoint` and `path` ONLY -- see the module doc's "Rule 5": no URL this
    module builds may ever carry the token or any other secret. `path` comes from
    `altavista/server.py`'s own two routes, built entirely from path parameters FastAPI has
    already extracted (a manifest hash, a level/x/y triple) -- never from a header, a query
    parameter, or anything this function itself reads a secret out of."""
    return f"http://{config.endpoint}{path}"


def proxy_get(config: TilesServiceConfig, path: str, forwarded_request_headers: Optional[Dict[str, str]] = None) -> TilesResponse:
    """Proxies one `GET <path>` to the real `av-tiles` gateway at `config.endpoint`, with the
    bearer token read fresh from `config.token_path` (module doc, "Rule 1") and ONLY the
    headers in `forwarded_request_headers` (normally exactly `PASSTHROUGH_REQUEST_HEADERS`)
    added on top -- an `Authorization` entry in `forwarded_request_headers`, were a caller to
    somehow put one there, is silently overwritten by this function's own token-derived value
    rather than trusted, since `altavista/server.py`'s own routes never populate one there in
    the first place (belt AND suspenders for the module doc's "Rule 1").

    Raises `TilesNotConfiguredError`/`TilesUnreachableError` for this module's OWN failure to
    reach the gateway at all. Never raises for a real answer FROM the gateway, however it is
    status-coded: a `403`/`404`/`416` (or any other status) from the gateway is not this
    module's failure to surface, it is the payload `altavista/server.py`'s route passes
    straight back to the browser, status code, allowlisted headers and body together (rule 4:
    "a label refusal (403) must arrive as a 403 ... never a 500 and never an empty 200").
    """
    _require_configured(config)
    token = _read_token(config)
    headers = dict(forwarded_request_headers or {})
    headers["Authorization"] = f"Bearer {token}"
    url = _build_url(config, path)
    try:
        # trust_env=False (module doc, question 199): never HTTP_PROXY/NO_PROXY/etc from the
        # process environment. follow_redirects=False: av-tiles never redirects (crates/
        # av-tiles/src/server.rs's own two routes), so a 3xx here would be this module
        # mis-dialling, not something to chase transparently.
        with httpx.Client(trust_env=False, timeout=config.timeout_s, follow_redirects=False) as client:
            resp = client.get(url, headers=headers)
    except httpx.HTTPError as exc:
        raise TilesUnreachableError(config.endpoint or "", str(exc)) from None

    passthrough_headers = {name: resp.headers[name] for name in _PASSTHROUGH_RESPONSE_HEADERS if name in resp.headers}
    return TilesResponse(resp.status_code, passthrough_headers, resp.content)


def tiles_service_error_to_http_status(exc: TilesServiceError) -> Tuple[int, str]:
    """Maps any `TilesServiceError` to an `(http_status, message)` pair --
    `altavista/server.py`'s route handlers turn this straight into a `fastapi.HTTPException`.
    Both subclasses are `503 Service Unavailable`: this server genuinely cannot serve the
    route right now, for a reason named in the message -- there is no gateway-side typed
    refusal to map here (unlike `command_client`'s `CommandServiceRpcError`), because a real
    answer from the gateway, however it is status-coded, never raises at all (see
    `proxy_get`'s own docstring)."""
    return 503, str(exc)
