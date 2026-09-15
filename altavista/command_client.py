"""Real gRPC (+ admin-HTTP) client for `altavista.v1.CommandAuthorityService`, backing the
`/api/command/` routes `altavista/server.py` declares (R3.5a, `docs/aiplane-plan.md`
milestone A5 -- the command console panel's server half).

This module NEVER re-implements any part of the command authority's state machine, policy
evaluation, or ledger: every function below is a thin adapter over a REAL, already-running
`av-command` gRPC service (`crates/av-command`) -- never a Python-side reader of that
service's ledger files, and never a second copy of `crates/av-command/src/state.rs`'s edges.

Design rules, mirrored from `altavista/server.py`'s own `POST /api/cdm/sweep/sample` (that
route's own docstring is the precedent for "identity, never a path"; the precedent here is
"the SERVICE ENDPOINT, never a request parameter"):

- The gRPC endpoint and the admin-HTTP endpoint are CONFIGURATION (`CommandServiceConfig`,
  built once and handed to `create_app`/`serve` -- see `altavista/server.py`'s `create_app`
  and `altavista/__main__.py`'s `--command-endpoint`/`--command-admin-endpoint` flags), never
  a request body field or a query parameter. There is no code path from an HTTP request to a
  `grpc.insecure_channel(...)` target anywhere in this module -- a caller of any
  `/api/command/*` route cannot make this server dial an arbitrary host.
- `grpcio` stays an optional extra (`pyproject.toml`'s `[project.optional-dependencies].grpc`)
  -- importing this module, or `altavista.server`, must never require it to be installed
  (question 85's rule, applied here). `grpc` and the generated `authority_pb2_grpc` stub are
  imported at MODULE scope inside a `try`/`except ImportError`, mirroring
  `altavista/pb/__init__.py`'s own identical convention for that stub -- never a deferred
  import inside a route handler (which would make the failure mode depend on *which* route
  happened to run first).
- Every way this module can fail to answer at all -- no config, grpcio absent, the service
  configured but unreachable, or the service's own typed refusal -- is its own
  `CommandServiceError` subclass, carrying enough for a caller to answer with a specific HTTP
  status and a real message (never a flattened generic 500). `altavista/server.py`'s route
  handlers catch `CommandServiceError` (never a bare `Exception`) and map each concrete
  subclass through `command_service_error_to_http_status`.
- No token, anywhere. `authorize_command` forwards `principal_token` to the real service
  verbatim, as `AuthorizeRequest.principal_token`, and this module never stores, caches, or
  logs it -- see that function's own docstring. A refusal's message text is the real
  service's own typed error `Display` text, which (`crates/av-command/src/service.rs`'s own
  module doc, and its `tests/grpc_service.rs::authorize_refusal_message_never_contains_the_
  token_or_signature`) never contains the token or its signature -- this module adds no
  scrubbing of its own because there is nothing in that text to scrub in the first place.
"""
from __future__ import annotations

import dataclasses
import json
import logging
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

from .pb import authority_pb2
from .pb.altavista.v1 import command_pb2

try:
    import grpc
except ImportError:  # grpcio is the optional "grpc" extra (pyproject.toml) -- see module doc.
    grpc = None  # type: ignore[assignment]

log = logging.getLogger("altavista.command_client")

DEFAULT_TIMEOUT_S = 5.0


class CommandServiceError(Exception):
    """Base class for every typed way this module can fail to answer a `/api/command/*`
    route. Callers catch this, never a bare `Exception` -- see the module doc."""


class GrpcioNotInstalledError(CommandServiceError):
    """`grpcio` (the `[grpc]` extra) is not importable in this interpreter."""

    def __init__(self) -> None:
        super().__init__(
            "the command console routes need the optional 'grpc' extra "
            "(pip install -e '.[grpc]') -- grpcio is not installed in this interpreter"
        )


class CommandServiceNotConfiguredError(CommandServiceError):
    """No endpoint of the named kind was given at `serve`/`create_app` startup."""

    def __init__(self, what: str) -> None:
        super().__init__(
            f"no {what} is configured for this viewer server (pass --command-endpoint/"
            f"--command-admin-endpoint to `python -m altavista serve`)"
        )


class CommandServiceUnreachableError(CommandServiceError):
    """The endpoint IS configured, but a real connection attempt did not succeed within
    `CommandServiceConfig.timeout_s`."""

    def __init__(self, endpoint: str, detail: str) -> None:
        self.endpoint = endpoint
        self.detail = detail
        super().__init__(f"command service at {endpoint!r} is configured but unreachable: {detail}")


class CommandServiceRpcError(CommandServiceError):
    """A REAL refusal from the running `av-command` service: its own `grpc.StatusCode` and
    message, carried verbatim -- never flattened to a generic failure. `code` is a real
    `grpc.StatusCode` member (raising this always means a real RPC already completed with a
    non-OK status, so grpcio is necessarily importable at that point)."""

    def __init__(self, code: Any, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"{code.name}: {message}")


@dataclasses.dataclass(frozen=True)
class CommandServiceConfig:
    """Where the command console routes reach the real `av-command` service -- built once and
    handed to `create_app`/`serve` (question 199: never the process environment; never a
    request parameter -- see the module doc). `grpc_endpoint`/`admin_endpoint` are plain
    `"host:port"` strings, exactly what `av-command --bind`/`--admin-bind` themselves take.
    `entities` is the server-side list of entity ids `list_proposed_commands` sweeps when a
    caller names none explicitly (`GET /api/command/proposals` with no `entity_id` query
    parameter)."""

    grpc_endpoint: Optional[str] = None
    admin_endpoint: Optional[str] = None
    entities: Tuple[str, ...] = ()
    timeout_s: float = DEFAULT_TIMEOUT_S


def _require_grpc() -> None:
    if grpc is None:
        raise GrpcioNotInstalledError()


def _stub(config: CommandServiceConfig):
    """A fresh `CommandAuthorityServiceStub` over a fresh `grpc.insecure_channel` -- never
    memoized across calls: a channel is cheap and lazy on its own (it opens no socket until
    the first RPC actually runs), so there is no lifecycle this module needs to manage and no
    stale channel a long-lived viewer process could be left holding across an `av-command`
    restart. Plaintext on loopback only, exactly like `crates/av-command/src/service.rs`'s own
    transport (question 155) -- this module places no TLS of any kind in front of it."""
    _require_grpc()
    if not config.grpc_endpoint:
        raise CommandServiceNotConfiguredError("command gRPC endpoint")
    from .pb import authority_pb2_grpc  # None when grpcio is absent (altavista/pb/__init__.py)

    if authority_pb2_grpc is None:
        raise GrpcioNotInstalledError()
    channel = grpc.insecure_channel(config.grpc_endpoint)
    return authority_pb2_grpc.CommandAuthorityServiceStub(channel)


def _call(config: CommandServiceConfig, method_name: str, request: Any) -> Any:
    """The ONE call site every function below goes through: builds a fresh stub, calls
    `method_name` with `request` under `config.timeout_s`, and maps every way it can fail to
    the typed `CommandServiceError` subclass this module's callers expect -- so that mapping
    is never duplicated at each of `list_proposed_commands`/`get_decision`/`get_trail`/
    `authorize_command`'s own call sites."""
    stub = _stub(config)
    rpc = getattr(stub, method_name)
    try:
        return rpc(request, timeout=config.timeout_s)
    except grpc.RpcError as exc:
        code = exc.code()
        if code in (grpc.StatusCode.UNAVAILABLE, grpc.StatusCode.DEADLINE_EXCEEDED):
            raise CommandServiceUnreachableError(config.grpc_endpoint or "<unset>", f"{code.name}: {exc.details()}") from None
        raise CommandServiceRpcError(code, exc.details() or "") from None


def _int64(value: int) -> str:
    """An ``int64`` rendered for JSON as a decimal STRING, not a number.

    TAI nanosecond epochs on this platform are around ``1.77e18``, comfortably past
    JavaScript's ``Number.MAX_SAFE_INTEGER`` (``2**53 - 1``, about ``9.01e15``). A plain JSON
    number therefore reaches the browser already rounded -- ``JSON.parse`` does it silently,
    with no error anywhere, so a console showing a transition epoch would show a wrong time
    and nothing would say so. That is the "failure that leaves no trace" shape this track's
    reviews keep finding, and it is also exactly what protobuf's own canonical JSON mapping
    avoids by specifying that ``int64`` is encoded as a string. This function is that mapping,
    applied at the one boundary where these values leave Python.
    """
    return str(int(value))


def _transition_to_dict(t: Any) -> Dict[str, Any]:
    return {
        "state": command_pb2.CommandState.Name(t.state),
        "taiNs": _int64(t.tai_ns),
        "principal": t.principal,
        "reason": t.reason,
        "ackLevel": command_pb2.AckLevel.Name(t.ack_level),
        "delegationId": t.delegation_id,
    }


def _policy_input_to_dict(pi: Any) -> Dict[str, Any]:
    return {
        "commandId": pi.command_id,
        "entityId": pi.entity_id,
        "commandClass": pi.command_class,
        "hazardous": pi.hazardous,
        "envelopeId": pi.envelope_id,
        "rate": {
            "countsByClass": dict(pi.rate.counts_by_class),
            "windowNs": _int64(pi.rate.window_ns),
        }
        if pi.HasField("rate")
        else None,
    }


def _decision_to_dict(d: Any) -> Dict[str, Any]:
    return {
        "decisionId": d.decision_id,
        "allow": d.allow,
        "policyHash": d.policy_hash,
        "reasons": list(d.reasons),
        "matchedRulePath": d.matched_rule_path,
        "evaluatedTaiNs": _int64(d.evaluated_tai_ns),
        "input": _policy_input_to_dict(d.input) if d.HasField("input") else None,
    }


def list_proposed_commands(config: CommandServiceConfig, entity_id: Optional[str] = None) -> List[Dict[str, Any]]:
    """The `PROPOSED` commands, with their rationale and evidence ids, for `entity_id` -- or,
    when `entity_id` is `None`, for every entity `config.entities` names (A5: "proposals with
    their rationale and evidence"). Real `Query(QueryByEntity{state_filter=PROPOSED})` calls,
    one per entity swept -- never a client-side re-filter of an unfiltered query, so the real
    service's own filter is what decides "proposed", not a second copy of that rule here.
    """
    entity_ids = [entity_id] if entity_id else list(config.entities)
    out: List[Dict[str, Any]] = []
    for eid in entity_ids:
        request = authority_pb2.QueryRequest(entity=authority_pb2.QueryByEntity(entity_id=eid, state_filter=command_pb2.COMMAND_STATE_PROPOSED))
        response = _call(config, "Query", request)
        for command in response.commands:
            proposal = response.proposals[command.id] if command.id in response.proposals else None
            out.append(
                {
                    "commandId": command.id,
                    "entityId": command.entity_id,
                    "commandClass": command.command_class,
                    "hazardous": command.hazardous,
                    "idempotencyKey": command.idempotency_key,
                    "rationale": proposal.rationale if proposal is not None else "",
                    "evidenceIds": list(proposal.evidence_ids) if proposal is not None else [],
                }
            )
    return out


def get_decision(config: CommandServiceConfig, command_id: str) -> Optional[Dict[str, Any]]:
    """The `PolicyDecision` for `command_id` (decision id, policy hash, allow/deny and
    reasons, and the `PolicyInput` it was decided over) -- `None` when `command_id` exists but
    has not been `Checked` yet (still `PROPOSED`). A `command_id` this service has never held
    at all is a real `Query` `NOT_FOUND`, raised by `_call` as `CommandServiceRpcError`, never
    silently returned as `None` (that would make "never checked" and "does not exist" the
    same answer, which they are not)."""
    request = authority_pb2.QueryRequest(command_id=command_id)
    response = _call(config, "Query", request)
    if command_id not in response.decisions:
        return None
    return _decision_to_dict(response.decisions[command_id])


def get_trail(config: CommandServiceConfig, command_id: str) -> List[Dict[str, Any]]:
    """Every `CommandTransition` `command_id` has recorded, in order, with its principal,
    reason, ack level and delegation. An unknown `command_id` is `_call`'s own `NOT_FOUND`."""
    request = authority_pb2.QueryRequest(command_id=command_id)
    response = _call(config, "Query", request)
    command = response.commands[0]
    return [_transition_to_dict(t) for t in command.transitions]


def authorize_command(config: CommandServiceConfig, command_id: str, principal_token: str, delegation_id: str = "") -> Dict[str, Any]:
    """Forwards `principal_token` to the real service's `Authorize` rpc, VERBATIM, as
    `AuthorizeRequest.principal_token` -- this function (and everything it calls) never
    stores, caches, or logs `principal_token`; the token exists in this process's memory only
    as this call's own local parameter and as a field on the one request message built here,
    for the duration of this one call. On success, returns the authorized command's new state
    and its transition trail (never the token, which the service does not echo back either).
    On refusal, raises `CommandServiceRpcError` carrying the real service's own status code
    and message -- see the module doc for why that text is already guaranteed token-free.
    """
    request = authority_pb2.AuthorizeRequest(command_id=command_id, principal_token=principal_token, delegation_id=delegation_id or "")
    response = _call(config, "Authorize", request)
    command = response.command
    return {
        "commandId": command.id,
        "state": command_pb2.CommandState.Name(command.state),
        "transitions": [_transition_to_dict(t) for t in command.transitions],
    }


def get_counters(config: CommandServiceConfig) -> Dict[str, Any]:
    """Proxies `av-command`'s own `GET /admin/api/evidence` (R3.1 extended it with a sorted
    `refusals` object) verbatim -- this module never counts anything itself; the real
    service's own `Counters` (`crates/av-command/src/counters.rs`) is the one source of truth
    for "everything rejected is counted" (ADR-004)."""
    if not config.admin_endpoint:
        raise CommandServiceNotConfiguredError("command admin endpoint")
    url = f"http://{config.admin_endpoint}/admin/api/evidence"
    try:
        with urllib.request.urlopen(url, timeout=config.timeout_s) as resp:  # noqa: S310 (fixed, configured loopback endpoint)
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.URLError as exc:
        raise CommandServiceUnreachableError(config.admin_endpoint, str(exc)) from None


def command_service_error_to_http_status(exc: CommandServiceError) -> Tuple[int, str]:
    """Maps any `CommandServiceError` to an `(http_status, message)` pair --
    `altavista/server.py`'s route handlers turn this straight into a `fastapi.HTTPException`.
    A real `CommandServiceRpcError` keeps the real service's own status code's ordinary gRPC
    meaning (`crates/av-command/src/service.rs`'s own module doc names each one); every other
    subclass (no config, grpcio absent, unreachable) is `503 Service Unavailable` -- this
    server genuinely cannot serve the route right now, for a reason named in the message."""
    if isinstance(exc, CommandServiceRpcError):
        status = {
            "NOT_FOUND": 404,
            "INVALID_ARGUMENT": 400,
            "FAILED_PRECONDITION": 409,
            "ALREADY_EXISTS": 409,
            "UNAUTHENTICATED": 401,
            "PERMISSION_DENIED": 403,
            "UNAVAILABLE": 503,
            "DEADLINE_EXCEEDED": 503,
        }.get(exc.code.name, 502)
        return status, exc.message or exc.code.name
    return 503, str(exc)
