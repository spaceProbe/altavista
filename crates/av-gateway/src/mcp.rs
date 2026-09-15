//! D3: a hand-rolled JSON-RPC 2.0 server over stdio. There is no MCP or JSON-RPC crate
//! anywhere in this workspace's `Cargo.lock`, `av-cdm`'s or `av-command`'s dependency trees,
//! or in `/Users/probe/code/spoore` (checked directly against source before this module was
//! written -- `grep -ri mcp` and `grep -i jsonrpc` over both trees' `Cargo.toml`/
//! `Cargo.lock` turned up nothing; see this task's own report for the exact commands). This
//! module hand-parses the envelope with `serde_json` (already a workspace dependency),
//! following `crates/av-command/src/admin.rs`'s precedent of a hand-rolled minimal server
//! rather than a framework -- the same discipline, over stdio instead of a `TcpListener`.
//!
//! ## Transport: injected streams, never the process's real stdin/stdout in a test
//!
//! [`serve`] is generic over any `AsyncBufRead + Unpin`/`AsyncWrite + Unpin` pair (question
//! 199: no test may mutate or depend on the *process's own* stdio) -- one JSON value per
//! line (newline-delimited, the standard MCP stdio framing), read with `read_line`, written
//! back with a trailing `\n` and an explicit `flush`. This module's own
//! `tests::stdio_transport_roundtrips_a_real_request_over_an_in_memory_duplex_pipe` drives
//! it through an in-memory `tokio::io::duplex` pipe, never the real process streams; every
//! other test below drives [`McpHandler::handle_message`] directly (no I/O at all) for speed
//! and precision.
//!
//! ## Deny-by-default allow-list (D3) -- one source, not two lists
//!
//! [`GatewayTool::ALL`] is the only place the allow-list is spelled out. `tools/list`
//! ([`McpHandler::handle_tools_list`]) renders it; `tools/call`'s dispatch
//! ([`GatewayTool::from_name`]) looks a name up against the SAME slice. There is no second,
//! hand-maintained list anywhere in this module for the two to disagree against --
//! `tests::tools_list_and_the_dispatch_table_can_never_disagree` asserts this by
//! construction, not by comparing two literals.
//!
//! ## Every refusal typed and counted (D3)
//!
//! A malformed frame ([`McpRefusal::ParseError`]/[`InvalidRequest`]), an unrecognized
//! top-level method ([`McpRefusal::MethodNotFound`] -- this is how a crafted `"method":
//! "authorize"` frame, D4's own acceptance line, is refused: `"authorize"` is not
//! `"initialize"`/`"tools/list"`/`"tools/call"`), and a `tools/call` naming a tool outside
//! [`GatewayTool::ALL`] ([`McpRefusal::ToolNotAllowed`] -- D4's other own acceptance line, a
//! `tools/call` naming `"authorize"`) each carry the correct JSON-RPC error code and
//! increment [`crate::counters::Counters`] before this module returns a response -- never a
//! silent drop, never a panic.
//!
//! ## R5.1/question 208(b): which methods authenticate, and why (a written, declared decision)
//!
//! `tools/call` naming `"query"` or `"propose_command"` authenticates every caller --
//! [`McpHandler::handle_query`]/[`McpHandler::handle_propose_command`] call through the exact
//! same [`crate::auth::AuthContext`] gate the gRPC surfaces use, via [`authenticated_query`]/
//! [`authenticated_propose_command`], never a second copy. **`initialize` and `tools/list` do
//! NOT authenticate, by deliberate decision, not omission:**
//!
//! - `initialize` is the MCP handshake itself. This hand-rolled server's own wire shape has no
//!   field on the `initialize` request an MCP client could even carry a token in before it has
//!   completed the handshake that tells it how to -- refusing it for a missing credential the
//!   protocol gives no way to present yet would make every MCP client unable to ever get far
//!   enough to learn this server's own `capabilities`. Its response body
//!   ([`McpHandler::handle_initialize`]) carries only `protocolVersion`/`serverInfo`/
//!   `capabilities` -- no product data, no clearance, no ledger content.
//! - `tools/list` renders exactly [`GatewayTool::ALL`]'s own `name()`/`description()` --
//!   values already present, verbatim, in this crate's own committed source code (this file).
//!   There is nothing in that response a caller could not already read directly from this
//!   repository; authenticating it would add a check with no confidentiality boundary behind
//!   it to protect.
//!
//! Both are recorded, with this exact reasoning, in `docs/compliance/av-gateway/control-
//! matrix.md`'s own IA rows -- a declared decision, never a silent gap.

use std::sync::Arc;

use av_cdm::pb::{GatewayQueryRequest, GatewaySelector, RunIdentity};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use av_command::clock::Clock;
use av_command::ledger::Ledger;

use crate::auth::{AuthContext, AuthRefusal};
use crate::counters::{Counted, Counters};
use crate::gateway::{authenticated_query, AuthenticatedQueryError, GatewayCore};
use crate::propose_flow::{authenticated_propose_command, AuthenticatedProposeError, ProposeCommandInput, ProposeFlowError};
use crate::propose_only::ProposeOnlyAuthority;

/// The gateway's deny-by-default MCP tool allow-list -- see the module doc's "one source,
/// not two lists" section. Adding a tool means adding a variant here AND to [`Self::ALL`];
/// there is no other place a name needs to be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayTool {
    /// The read-only, label-aware query (D1/D2), over [`crate::gateway::GatewayCore`].
    Query,
    /// D4: proposes over [`crate::propose_only::ProposeOnlyAuthority`], never anything past
    /// what a real `Propose` RPC itself produces. Question 209(a): that real RPC now runs
    /// the check edge automatically, so a successful call here lands at `CHECKED` (or is
    /// refused outright by a policy denial) -- this tool still can never itself authorize,
    /// dispatch, ack, expire or fail a command; the propose-only structural guarantee (D4)
    /// is about which RPCs this tool can reach, not which state one of them ends at.
    ProposeCommand,
}

impl GatewayTool {
    /// The complete allow-list. `tools/list` and `tools/call`'s dispatch both derive from
    /// this one slice -- see the module doc.
    pub const ALL: &'static [GatewayTool] = &[GatewayTool::Query, GatewayTool::ProposeCommand];

    pub fn name(&self) -> &'static str {
        match self {
            GatewayTool::Query => "query",
            GatewayTool::ProposeCommand => "propose_command",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            GatewayTool::Query => "Read-only, label-aware query over one run's products (trajectories, events, scores, measurements), by run identity.",
            GatewayTool::ProposeCommand => "Propose a command -- the check edge now runs automatically (question 209(a)), so a successful call lands the command at CHECKED, or it is refused outright by a policy denial. This tool can never authorize, dispatch, ack, expire or fail a command.",
        }
    }

    /// Looks `name` up against [`Self::ALL`] -- the ONE dispatch table this module has; a
    /// name absent from it (e.g. `"authorize"`) is `None`, refused by the caller as
    /// [`McpRefusal::ToolNotAllowed`].
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.name() == name)
    }
}

/// Every way this module can refuse a JSON-RPC frame or a `tools/call`. Each carries its
/// own JSON-RPC 2.0 error code ([`McpRefusal::json_rpc_code`]) and is `Counted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpRefusal {
    /// The frame is not valid JSON at all. JSON-RPC 2.0 code `-32700`.
    ParseError { detail: String },
    /// The frame is valid JSON but not a valid JSON-RPC 2.0 request object (missing/wrong
    /// `jsonrpc`, missing `method`, `method` not a string). Code `-32600`.
    InvalidRequest { detail: String },
    /// `method` is not `"initialize"`/`"tools/list"`/`"tools/call"`. Code `-32601`. This is
    /// how a raw `{"method": "authorize"}` frame is refused (D4).
    MethodNotFound { method: String },
    /// `tools/call`'s own `params.name` is missing, empty, or not on [`GatewayTool::ALL`].
    /// Code `-32001` (a server-defined code, JSON-RPC reserves `-32000..-32099` for these).
    /// This is how a `tools/call` naming `"authorize"` is refused (D4).
    ToolNotAllowed { tool: String },
    /// `tools/call`'s own `params.arguments` do not match the named tool's expected shape.
    /// Code `-32602`.
    InvalidParams { detail: String },
    /// R5.1: `arguments.caller_token` was absent/empty, or failed verification --
    /// [`crate::auth::AuthRefusal::MissingToken`]/[`crate::auth::AuthRefusal::TokenInvalid`].
    /// A server-defined code distinct from every refusal above, so a client can tell "who are
    /// you" apart from "your request was malformed" or "you may not" by CODE alone (invariant
    /// F), never by parsing this variant's own message text.
    Unauthenticated { detail: String },
    /// R5.1: the verified token names no role granting this surface, or a declared
    /// `caller_clearance`/`principal` disagreed with the verified one --
    /// [`crate::auth::AuthRefusal`]'s remaining variants.
    PermissionDenied { detail: String },
}

impl McpRefusal {
    pub fn json_rpc_code(&self) -> i64 {
        match self {
            McpRefusal::ParseError { .. } => -32700,
            McpRefusal::InvalidRequest { .. } => -32600,
            McpRefusal::MethodNotFound { .. } => -32601,
            McpRefusal::ToolNotAllowed { .. } => -32001,
            McpRefusal::InvalidParams { .. } => -32602,
            McpRefusal::Unauthenticated { .. } => -32002,
            McpRefusal::PermissionDenied { .. } => -32003,
        }
    }
}

impl Counted for McpRefusal {
    fn code(&self) -> &'static str {
        match self {
            McpRefusal::ParseError { .. } => "mcp_parse_error",
            McpRefusal::InvalidRequest { .. } => "mcp_invalid_request",
            McpRefusal::MethodNotFound { .. } => "mcp_method_not_found",
            McpRefusal::ToolNotAllowed { .. } => "mcp_tool_not_allowed",
            McpRefusal::InvalidParams { .. } => "mcp_invalid_params",
            // R5.1: these two variants are never routed through `Self::refuse` (the
            // underlying `crate::auth::AuthRefusal` has already recorded its own, more
            // specific `gateway_auth_*` counter by the time either is constructed -- see
            // `auth_refusal_to_mcp`'s own doc comment) -- these codes exist only so
            // `Counted` is total; mirrors `crate::gateway`/`crate::mcp`'s own existing
            // convention of never double-counting a wrapped refusal (e.g. `query`'s
            // pre-existing `RefusalReason` mapping to `InvalidParams` below).
            McpRefusal::Unauthenticated { .. } => "mcp_unauthenticated",
            McpRefusal::PermissionDenied { .. } => "mcp_permission_denied",
        }
    }
}

impl std::fmt::Display for McpRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpRefusal::ParseError { detail } => write!(f, "parse error: {detail}"),
            McpRefusal::InvalidRequest { detail } => write!(f, "invalid request: {detail}"),
            McpRefusal::MethodNotFound { method } => write!(f, "method not found: {method:?} -- only initialize, tools/list, tools/call are recognized"),
            McpRefusal::ToolNotAllowed { tool } => write!(f, "tool not allowed: {tool:?} is not on this gateway's deny-by-default allow-list"),
            McpRefusal::InvalidParams { detail } => write!(f, "invalid params: {detail}"),
            McpRefusal::Unauthenticated { detail } => write!(f, "unauthenticated: {detail}"),
            McpRefusal::PermissionDenied { detail } => write!(f, "permission denied: {detail}"),
        }
    }
}

/// R5.1: classifies a [`crate::auth::AuthRefusal`] into the two new [`McpRefusal`] variants
/// above by CODE, not by re-deriving it from message prose -- `MissingToken`/`TokenInvalid`
/// ("who are you") become [`McpRefusal::Unauthenticated`]; every other variant ("you may not")
/// becomes [`McpRefusal::PermissionDenied`]. Deliberately does NOT call `McpHandler::refuse`
/// (so does not double-count): the underlying `AuthRefusal` has already recorded its own,
/// specific `gateway_auth_*` counter inside `crate::auth::AuthContext` itself, the identical
/// "the underlying refusal's own counter is what moved, not a second wrapper counter"
/// convention this module's own pre-existing `query`/`propose_command` error paths already
/// follow for `RefusalReason`/`ProposeFlowError`.
fn auth_refusal_to_mcp(e: AuthRefusal) -> McpRefusal {
    match &e {
        AuthRefusal::MissingToken { .. } | AuthRefusal::TokenInvalid { .. } => McpRefusal::Unauthenticated { detail: e.to_string() },
        AuthRefusal::RoleNotGranted { .. } | AuthRefusal::NoClearanceForSubject { .. } | AuthRefusal::ClearanceMismatch { .. } | AuthRefusal::PrincipalMismatch { .. } => {
            McpRefusal::PermissionDenied { detail: e.to_string() }
        }
    }
}

/// Everything a `propose_command` call needs beyond [`crate::propose_only::
/// ProposeOnlyAuthority`] itself: where to record the evidence topic (D5/D6) and the
/// injected clock (D8).
pub struct McpContext {
    pub gateway: Arc<GatewayCore>,
    pub authority: Arc<ProposeOnlyAuthority>,
    pub evidence_ledger: Arc<Ledger>,
    pub clock: Arc<dyn Clock>,
    pub counters: Arc<Counters>,
    /// R5.1/question 208(b): the same [`crate::auth::AuthContext`] the gRPC surfaces use --
    /// `query`/`propose_command` authenticate through this, never a second copy.
    pub auth: Arc<AuthContext>,
}

/// Handles one JSON-RPC 2.0 message at a time -- no I/O of its own (see [`serve`] for the
/// stdio loop around this). Kept separate from the stdio loop so this crate's tests can
/// drive it directly with crafted strings, no stream needed at all.
pub struct McpHandler {
    ctx: McpContext,
}

fn error_object(refusal: &McpRefusal) -> Value {
    json!({ "code": refusal.json_rpc_code(), "message": refusal.to_string() })
}

fn get_str<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

impl McpHandler {
    pub fn new(ctx: McpContext) -> Self {
        Self { ctx }
    }

    fn refuse(&self, refusal: McpRefusal) -> McpRefusal {
        self.ctx.counters.record(&refusal);
        refusal
    }

    /// Handles exactly one line of input. Returns `None` for a JSON-RPC notification (a
    /// well-formed request with no `id` -- per JSON-RPC 2.0, a server must never reply to
    /// one) and `Some(response_object)` otherwise -- a parse error always gets a response
    /// with `id: null`, per the same spec.
    pub async fn handle_message(&self, raw: &str) -> Option<Value> {
        let parsed: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                let refusal = self.refuse(McpRefusal::ParseError { detail: e.to_string() });
                return Some(json!({"jsonrpc": "2.0", "id": Value::Null, "error": error_object(&refusal)}));
            }
        };
        let Some(obj) = parsed.as_object() else {
            let refusal = self.refuse(McpRefusal::InvalidRequest { detail: "top-level JSON value must be an object".to_string() });
            return Some(json!({"jsonrpc": "2.0", "id": Value::Null, "error": error_object(&refusal)}));
        };
        let id = obj.get("id").cloned();
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            let refusal = self.refuse(McpRefusal::InvalidRequest { detail: "\"jsonrpc\" must be exactly \"2.0\"".to_string() });
            return Some(json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": error_object(&refusal)}));
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            let refusal = self.refuse(McpRefusal::InvalidRequest { detail: "\"method\" must be present and a string".to_string() });
            return Some(json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": error_object(&refusal)}));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);

        // A well-formed request with no "id" is a JSON-RPC notification: process it for
        // side effects (there are none for this server's own three methods, but the rule
        // is general) and never respond -- per JSON-RPC 2.0.
        let Some(id) = id else {
            let _ = self.dispatch(method, &params).await;
            return None;
        };

        let result = self.dispatch(method, &params).await;
        Some(match result {
            Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
            Err(refusal) => json!({"jsonrpc": "2.0", "id": id, "error": error_object(&refusal)}),
        })
    }

    async fn dispatch(&self, method: &str, params: &Value) -> Result<Value, McpRefusal> {
        match method {
            "initialize" => Ok(self.handle_initialize()),
            "tools/list" => Ok(self.handle_tools_list()),
            "tools/call" => self.handle_tools_call(params).await,
            other => Err(self.refuse(McpRefusal::MethodNotFound { method: other.to_string() })),
        }
    }

    fn handle_initialize(&self) -> Value {
        json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {"name": "av-gateway", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"tools": {}},
        })
    }

    /// Renders [`GatewayTool::ALL`] -- see the module doc: this and [`GatewayTool::
    /// from_name`] are the same one list, never two.
    fn handle_tools_list(&self) -> Value {
        let tools: Vec<Value> = GatewayTool::ALL
            .iter()
            .map(|t| {
                json!({
                    "name": t.name(),
                    "description": t.description(),
                    "inputSchema": {"type": "object"},
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    async fn handle_tools_call(&self, params: &Value) -> Result<Value, McpRefusal> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err(self.refuse(McpRefusal::InvalidParams { detail: "params.name must be a string".to_string() }));
        };
        let Some(tool) = GatewayTool::from_name(name) else {
            return Err(self.refuse(McpRefusal::ToolNotAllowed { tool: name.to_string() }));
        };
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        match tool {
            GatewayTool::Query => self.handle_query(&arguments),
            GatewayTool::ProposeCommand => self.handle_propose_command(&arguments).await,
        }
    }

    fn handle_query(&self, args: &Value) -> Result<Value, McpRefusal> {
        if !args.is_object() {
            return Err(self.refuse(McpRefusal::InvalidParams { detail: "arguments must be an object".to_string() }));
        }
        let run_id = get_str(args, "run_id")
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: "arguments.run_id must be a string".to_string() }))?;
        let config_hash = get_str(args, "config_hash").unwrap_or("");
        let caller_clearance = get_str(args, "caller_clearance")
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: "arguments.caller_clearance must be a string".to_string() }))?;
        let selector_name = get_str(args, "selector")
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: "arguments.selector must be a string".to_string() }))?;
        let selector = GatewaySelector::from_str_name(selector_name)
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: format!("arguments.selector {selector_name:?} is not a recognized GatewaySelector") }))?;
        // D1: this field exists ONLY so a crafted call can attempt the caller-supplied-path
        // attack and be refused -- see crate::gateway's own ordered chain, which checks
        // this field first, regardless of what else is well-formed above.
        let caller_supplied_products_uri = get_str(args, "caller_supplied_products_uri").unwrap_or("");
        // R5.1/question 208(b): absent means empty, refused by crate::auth::AuthContext's own
        // MissingToken check -- there is no "no token supplied" default that means "allow".
        let caller_token = get_str(args, "caller_token").unwrap_or("");

        let request = GatewayQueryRequest {
            run: Some(RunIdentity { run_id: run_id.to_string(), config_hash: config_hash.to_string() }),
            caller_clearance: caller_clearance.to_string(),
            selector: selector as i32,
            caller_supplied_products_uri: caller_supplied_products_uri.to_string(),
            caller_token: caller_token.to_string(),
        };
        let response = authenticated_query(&self.ctx.gateway, &self.ctx.auth, &self.ctx.counters, caller_token, request).map_err(|e| match e {
            AuthenticatedQueryError::Auth(a) => auth_refusal_to_mcp(a),
            AuthenticatedQueryError::Refusal(r) => McpRefusal::InvalidParams { detail: r.to_string() },
        })?;
        Ok(json!({
            "run_id": response.run.as_ref().map(|r| r.run_id.clone()).unwrap_or_default(),
            "config_hash": response.run.as_ref().map(|r| r.config_hash.clone()).unwrap_or_default(),
            "product_label": response.product_label.as_ref().map(|l| json!({"marking": l.marking, "caveats": l.caveats})),
            "trajectory_ids": response.trajectories.keys().cloned().collect::<Vec<_>>(),
            "event_count": response.events.len(),
            "score_names": response.scores.keys().cloned().collect::<Vec<_>>(),
            "measurement_count": response.measurements.len(),
            "query_id": response.query_id,
        }))
    }

    /// D4/D5/D6/R3.2: builds a [`crate::propose_flow::ProposeCommandInput`] from `args` and
    /// calls the ONE shared implementation, [`crate::propose_flow::propose_command`] -- the
    /// same function `crate::propose_flow::ModelProposeServiceImpl` (the gRPC surface) calls
    /// (see that module's own doc for why there is exactly one implementation, not two).
    /// Note this constructor NEVER reads a `"state"` or `"transitions"` key even if the
    /// caller's JSON supplies one, so this tool's own JSON schema cannot itself construct an
    /// already-started `Command`; the `AlreadyStarted` refusal (D4) is reachable only by
    /// calling [`crate::propose_only::ProposeOnlyAuthority::propose`] directly with a
    /// hand-built `Command`, proven by `crates/av-gateway/tests/propose_only.rs`, not through
    /// this tool's own JSON surface. A non-empty `envelope_id` IS settable here (question 53)
    /// precisely so that refusal is reachable end to end through this tool, as D4 requires.
    async fn handle_propose_command(&self, args: &Value) -> Result<Value, McpRefusal> {
        if !args.is_object() {
            return Err(self.refuse(McpRefusal::InvalidParams { detail: "arguments must be an object".to_string() }));
        }
        let command_id = get_str(args, "command_id")
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: "arguments.command_id must be a string".to_string() }))?;
        let entity_id = get_str(args, "entity_id")
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: "arguments.entity_id must be a string".to_string() }))?;
        let command_class = get_str(args, "command_class").unwrap_or("");
        let principal = get_str(args, "principal")
            .ok_or_else(|| self.refuse(McpRefusal::InvalidParams { detail: "arguments.principal (the model identity) must be a string".to_string() }))?;
        let model_version = get_str(args, "model_version").unwrap_or("").to_string();
        let hazardous = args.get("hazardous").and_then(Value::as_bool).unwrap_or(false);
        let envelope_id = get_str(args, "envelope_id").unwrap_or("").to_string();
        let idempotency_key = get_str(args, "idempotency_key").unwrap_or("").to_string();
        let rationale = get_str(args, "rationale").unwrap_or("").to_string();
        let evidence_ids: Vec<String> =
            args.get("evidence_ids").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let run_id = get_str(args, "run_id").unwrap_or("").to_string();
        let config_hash = get_str(args, "config_hash").unwrap_or("").to_string();
        let query_ids: Vec<String> =
            args.get("query_ids").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        // R5.1/question 208(b): absent means empty, refused by crate::auth::AuthContext's own
        // MissingToken check -- there is no "no token supplied" default that means "allow".
        let caller_token = get_str(args, "caller_token").unwrap_or("").to_string();

        let input = ProposeCommandInput {
            command_id: command_id.to_string(),
            entity_id: entity_id.to_string(),
            command_class: command_class.to_string(),
            hazardous,
            envelope_id,
            idempotency_key,
            rationale,
            evidence_ids,
            principal: principal.to_string(),
            model_version,
            run: Some(RunIdentity { run_id, config_hash }),
            query_ids,
        };

        match authenticated_propose_command(&self.ctx.authority, &self.ctx.evidence_ledger, &*self.ctx.clock, &self.ctx.counters, &self.ctx.auth, &caller_token, input).await {
            // Question 209(a): reads the REAL `Command.state` this call actually produced
            // (normally CHECKED now, never hard-coded as "PROPOSED") -- the same discipline
            // D9's console route (`altavista/command_client.py`) applies, never inferred.
            Ok(output) => {
                let state_name = av_cdm::pb::CommandState::try_from(output.command.state).unwrap_or(av_cdm::pb::CommandState::Unspecified).as_str_name();
                Ok(json!({"command_id": output.command.id, "state": state_name}))
            }
            Err(AuthenticatedProposeError::Auth(a)) => Err(auth_refusal_to_mcp(a)),
            Err(AuthenticatedProposeError::Flow(err)) => Err(propose_flow_error_to_mcp(err)),
        }
    }
}

/// `ProposeFlowError` is already `Counted` and is counted inside [`propose_command`] itself;
/// this only reshapes it into an `McpRefusal` so `handle_message` has one error type to
/// serialize. `InvalidParams` (`-32602`) is the closest JSON-RPC meaning for "the server
/// refused the request's content", for every kind -- the real distinction (envelope vs.
/// already-started vs. transport vs. evidence-recording failure) survives in the message text
/// (`ProposeFlowError`'s own `Display`), which is what `crates/av-gateway/tests/propose_only.rs`
/// and `crates/av-gateway/tests/propose_flow_agreement.rs` actually assert against, not this
/// code.
fn propose_flow_error_to_mcp(err: ProposeFlowError) -> McpRefusal {
    McpRefusal::InvalidParams { detail: err.to_string() }
}

/// The stdio loop: reads one JSON value per line from `reader`, hands it to
/// [`McpHandler::handle_message`], writes any response back to `writer` with a trailing
/// newline and an explicit flush. Generic over any `AsyncBufRead`/`AsyncWrite` pair --
/// never the process's real stdin/stdout directly (a test drives this over an in-memory
/// `tokio::io::duplex` pipe; `crates/av-gateway/src/bin/av-gateway.rs` is the one call site
/// that hands it the real `tokio::io::stdin()`/`stdout()`).
pub async fn serve<R, W>(reader: R, mut writer: W, handler: McpHandler) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handler.handle_message(&line).await {
            let text = serde_json::to_string(&response).expect("a JSON-RPC response built from this module's own values always serializes");
            writer.write_all(text.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthContext, GroupClearanceMap};
    use crate::catalogue::{CatalogueEntry, RunCatalogue};
    use crate::labels::ClearanceLadder;
    use av_cdm::pb::{Label, Provenance, RunProducts};
    use av_command::authz::RoleTable;
    use av_command::clock::TestClock;
    use av_command::oidc::IssuerConfig;
    use av_command::test_support::{valid_claims, TestIssuer};
    use std::collections::BTreeMap;
    use tokio::io::AsyncReadExt as _;

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-gateway";
    const NOW_UNIX_S: i64 = 1_760_000_000;

    /// Mints a real, signed token against `issuer` -- the same [`TestIssuer`] `handler_with_
    /// catalogue`'s own [`AuthContext`] was built from -- carrying `groups`.
    fn mint(issuer: &TestIssuer, groups: &[&str]) -> String {
        let mut claims = valid_claims(ISSUER, AUDIENCE, "operator-1", NOW_UNIX_S, 3_600);
        claims["groups"] = serde_json::json!(groups);
        issuer.mint(&claims)
    }

    /// A ledger directory this call alone owns.
    ///
    /// The process id alone is NOT enough, and the manager's R3.2 review caught it failing for
    /// real: every one of the thirteen tests in this module reaches
    /// [`handler_with_catalogue`], which passed the *constant* name `"handler"`, so all
    /// thirteen derived the identical path -- and `cargo test` runs them concurrently on
    /// several threads. Two tests then interleave `remove_dir_all` with the other's
    /// `Ledger::open`, and the loser fails with `AlreadyExists` (EEXIST) from inside `open`.
    /// It is intermittent by construction: green on one run, `Os { code: 17 }` on the next,
    /// with nothing in the failure naming the shared path -- the same shape as question 199's
    /// intermittent panic, and the reason a monotonic per-call counter is folded in here
    /// rather than left to every caller to remember to pass a distinct `name`.
    fn temp_ledger_dir(name: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("av-gateway-mcp-test-{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A handler whose `authority` dials nothing real -- fine for every test in this module
    /// that never reaches `propose_command`'s success path (those live in
    /// `crates/av-gateway/tests/propose_only.rs`, which spins up a real
    /// `CommandAuthorityServiceImpl`). Connecting is deferred (async) so this stays a sync
    /// test helper: the channel is lazily built from an endpoint nothing will ever dial for
    /// the refusal-shape tests below (`tools/call` naming `"authorize"`, `MethodNotFound`,
    /// malformed frames) -- none of those reach `self.ctx.authority` at all.
    /// R5.1: `"operators"` grants `"query"` (human table) at clearance `"CUI"`;
    /// `"guests"` grants `"query"` at clearance `"UNCLASSIFIED"` (so a test can exercise D2's
    /// own over-clearance refusal with a genuinely lower, real, verified clearance rather than
    /// a caller-claimed one); `"proposer-service"` grants `"propose"` (service table, disjoint
    /// from the human one by construction).
    fn handler_with_catalogue() -> (McpHandler, TestIssuer, std::path::PathBuf) {
        let mut entries = BTreeMap::new();
        entries.insert(
            "run-a".to_string(),
            CatalogueEntry::from_run_products(
                Label { marking: "CUI".to_string(), caveats: vec![] },
                &RunProducts { run_id: "run-a".to_string(), provenance: Some(Provenance::default()), ..Default::default() },
            ),
        );
        let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
        let counters = Arc::new(Counters::new());
        let gateway = Arc::new(GatewayCore::new(RunCatalogue::new(entries), ladder, counters.clone()));
        let dir = temp_ledger_dir("handler");
        let evidence_ledger = Arc::new(Ledger::open(&dir).unwrap());
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(NOW_UNIX_S * 1_000_000_000));
        // A channel to an address nothing is listening on -- lazy-connect (tonic's
        // `Endpoint::connect_lazy`) never actually dials until the first RPC, and no test
        // in this module issues a successful `propose_command` call that reaches it.
        let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let authority = Arc::new(ProposeOnlyAuthority::from_channel(channel));

        let issuer = TestIssuer::new();
        let issuer_config = Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap());
        let human_roles = Arc::new(RoleTable::from_config(&BTreeMap::from([("operators".to_string(), vec!["query".to_string()]), ("guests".to_string(), vec!["query".to_string()])])));
        let service_roles = Arc::new(RoleTable::from_config(&BTreeMap::from([("proposer-service".to_string(), vec!["propose".to_string()])])));
        let group_clearance = Arc::new(GroupClearanceMap::new(BTreeMap::from([("operators".to_string(), "CUI".to_string()), ("guests".to_string(), "UNCLASSIFIED".to_string())])));
        let auth = Arc::new(AuthContext::new(issuer_config, human_roles, service_roles, group_clearance, clock.clone()));

        let ctx = McpContext { gateway, authority, evidence_ledger, clock, counters, auth };
        (McpHandler::new(ctx), issuer, dir)
    }

    #[tokio::test]
    async fn tools_list_returns_exactly_the_allow_list() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["query", "propose_command"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D3's own acceptance line: the two lists can never disagree, because there is only
    /// one -- this asserts `tools/list`'s rendered names equal `GatewayTool::ALL`'s names
    /// AND that `GatewayTool::from_name` recognizes every one of them (the dispatch side),
    /// derived from the same slice both times, never a second hand-typed list.
    #[tokio::test]
    async fn tools_list_and_the_dispatch_table_can_never_disagree() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await.unwrap();
        let listed: Vec<String> = resp["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        let dispatchable: Vec<String> = GatewayTool::ALL.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(listed, dispatchable);
        for name in &listed {
            assert!(GatewayTool::from_name(name).is_some(), "{name} is listed but not dispatchable");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tools_call_naming_authorize_is_refused_and_counted() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"authorize","arguments":{}}}"#).await.unwrap();
        assert_eq!(resp["error"]["code"], -32001);
        assert!(resp["error"]["message"].as_str().unwrap().contains("authorize"));
        assert_eq!(handler.ctx.counters.get("mcp_tool_not_allowed"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_raw_json_rpc_method_named_authorize_is_refused_as_method_not_found_and_counted() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"authorize","params":{}}"#).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(handler.ctx.counters.get("mcp_method_not_found"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tools_call_naming_check_dispatch_ack_expire_fail_are_all_refused_and_counted() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        for name in ["check", "dispatch", "ack", "expire", "fail", "verify_ledger", "not-a-real-tool"] {
            let raw = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#);
            let resp = handler.handle_message(&raw).await.unwrap();
            assert_eq!(resp["error"]["code"], -32001, "{name}");
        }
        assert_eq!(handler.ctx.counters.get("mcp_tool_not_allowed"), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_malformed_json_frame_is_refused_with_parse_error_and_counted_never_a_panic() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message("{not valid json at all").await.unwrap();
        assert_eq!(resp["error"]["code"], -32700);
        assert_eq!(resp["id"], Value::Null);
        assert_eq!(handler.ctx.counters.get("mcp_parse_error"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_frame_with_the_wrong_jsonrpc_version_is_invalid_request_and_counted() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message(r#"{"jsonrpc":"1.0","id":1,"method":"tools/list"}"#).await.unwrap();
        assert_eq!(resp["error"]["code"], -32600);
        assert_eq!(handler.ctx.counters.get("mcp_invalid_request"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_notification_with_no_id_gets_no_response_even_when_it_would_refuse() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let resp = handler.handle_message(r#"{"jsonrpc":"2.0","method":"authorize"}"#).await;
        assert!(resp.is_none());
        // Still counted -- a notification's side effects (including a refusal count) still
        // happen, only the response is suppressed (per JSON-RPC 2.0).
        assert_eq!(handler.ctx.counters.get("mcp_method_not_found"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn query_tool_end_to_end_returns_the_catalogued_run() {
        let (handler, issuer, dir) = handler_with_catalogue();
        let token = mint(&issuer, &["operators"]);
        let raw = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"query","arguments":{{"run_id":"run-a","caller_clearance":"CUI","selector":"GATEWAY_SELECTOR_ALL","caller_token":"{token}"}}}}}}"#);
        let resp = handler.handle_message(&raw).await.unwrap();
        assert_eq!(resp["result"]["run_id"], "run-a");
        assert_eq!(resp["result"]["product_label"]["marking"], "CUI");
        assert!(!resp["result"]["query_id"].as_str().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **R5.1 acceptance evidence: MCP `query` unauthenticated caller.** No `caller_token` at
    /// all -- refused `Unauthenticated` (code `-32002`), and the COUNTER (not merely the
    /// error) is what this test asserts moved.
    #[tokio::test]
    async fn query_tool_without_a_caller_token_is_refused_unauthenticated_and_the_counter_moves() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"query","arguments":{"run_id":"run-a","caller_clearance":"CUI","selector":"GATEWAY_SELECTOR_ALL"}}}"#;
        let resp = handler.handle_message(raw).await.unwrap();
        assert_eq!(resp["error"]["code"], -32002);
        assert_eq!(handler.ctx.counters.get("gateway_auth_missing_token"), 1, "the auth counter, not a RefusalReason counter, must have moved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A verified token whose groups grant no role at all (e.g. a name absent from the
    /// deployment's own role table) is refused `PermissionDenied` (code `-32003`), distinct
    /// from `Unauthenticated` -- "who are you" vs. "you may not".
    #[tokio::test]
    async fn query_tool_with_a_token_naming_no_granting_role_is_refused_permission_denied() {
        let (handler, issuer, dir) = handler_with_catalogue();
        let token = mint(&issuer, &["nobody"]);
        let raw = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"query","arguments":{{"run_id":"run-a","caller_clearance":"CUI","selector":"GATEWAY_SELECTOR_ALL","caller_token":"{token}"}}}}}}"#);
        let resp = handler.handle_message(&raw).await.unwrap();
        assert_eq!(resp["error"]["code"], -32003);
        assert_eq!(handler.ctx.counters.get("gateway_auth_role_not_granted"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The over-clearance product read (D2) is still reachable, exactly as before, once a
    /// caller authenticates at a genuinely lower, verified clearance (`"guests"` maps to
    /// `"UNCLASSIFIED"`) -- authentication is additive, never a replacement for D1/D2's own
    /// ordered refusal chain.
    #[tokio::test]
    async fn query_tool_over_clearance_is_refused_as_invalid_params_and_the_underlying_label_counter_still_increments() {
        let (handler, issuer, dir) = handler_with_catalogue();
        let token = mint(&issuer, &["guests"]);
        let raw = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"query","arguments":{{"run_id":"run-a","caller_clearance":"UNCLASSIFIED","selector":"GATEWAY_SELECTOR_ALL","caller_token":"{token}"}}}}}}"#);
        let resp = handler.handle_message(&raw).await.unwrap();
        assert_eq!(resp["error"]["code"], -32602);
        assert_eq!(handler.ctx.gateway.counters().get("label_over_clearance"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn query_tool_caller_supplied_products_uri_is_refused_and_counted() {
        let (handler, issuer, dir) = handler_with_catalogue();
        let token = mint(&issuer, &["operators"]);
        let raw = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"query","arguments":{{"run_id":"run-a","caller_clearance":"CUI","selector":"GATEWAY_SELECTOR_ALL","caller_supplied_products_uri":"/etc/passwd","caller_token":"{token}"}}}}}}"#
        );
        let resp = handler.handle_message(&raw).await.unwrap();
        assert_eq!(resp["error"]["code"], -32602);
        assert_eq!(handler.ctx.gateway.counters().get("gateway_caller_supplied_path"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **R5.1 acceptance evidence: MCP `propose_command` unauthenticated caller.** No
    /// `caller_token` -- refused `Unauthenticated`, and (unlike the pre-existing propose_only
    /// success tests, which need a real `CommandAuthorityService`) this never dials the lazily-
    /// connected, nothing-listening `authority` channel this handler was built with at all.
    #[tokio::test]
    async fn propose_command_tool_without_a_caller_token_is_refused_unauthenticated_and_the_counter_moves() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"propose_command","arguments":{"command_id":"cmd-1","entity_id":"sat-1","principal":"model-x"}}}"#;
        let resp = handler.handle_message(raw).await.unwrap();
        assert_eq!(resp["error"]["code"], -32002);
        assert_eq!(handler.ctx.counters.get("gateway_auth_missing_token"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A verified but purely-human token (only on the human role table, never the service one)
    /// cannot propose -- the disjointness guarantee invariant E requires, exercised through
    /// the MCP surface specifically.
    #[tokio::test]
    async fn propose_command_tool_with_a_human_only_token_is_refused_permission_denied() {
        let (handler, issuer, dir) = handler_with_catalogue();
        let token = mint(&issuer, &["operators"]); // "operators" grants "query", never "propose".
        let raw = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"propose_command","arguments":{{"command_id":"cmd-1","entity_id":"sat-1","principal":"model-x","caller_token":"{token}"}}}}}}"#);
        let resp = handler.handle_message(&raw).await.unwrap();
        assert_eq!(resp["error"]["code"], -32003);
        assert_eq!(handler.ctx.counters.get("gateway_auth_role_not_granted"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stdio_transport_roundtrips_a_real_request_over_an_in_memory_duplex_pipe() {
        let (handler, _issuer, dir) = handler_with_catalogue();
        // `client` stays ONE unsplit `DuplexStream` end (both `AsyncRead`/`AsyncWrite` take
        // `&mut self`, so one mutable handle suffices) so dropping it at the end of this
        // test closes the whole endpoint and the server sees a real EOF -- `tokio::io::
        // split`'s two halves share the underlying stream behind an `Arc`, so dropping only
        // one half (as an earlier version of this test did) never signals EOF to the peer
        // and the server's `read_line` blocks forever; this was a real, reproduced hang
        // (see this task's own report).
        let (mut client, server) = tokio::io::duplex(4096);
        let (server_read, server_write) = tokio::io::split(server);
        let server_task = tokio::spawn(async move { serve(server_read, server_write, handler).await });

        client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/list\"}\n").await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = client.read(&mut buf).await.unwrap();
        let line = String::from_utf8(buf[..n].to_vec()).unwrap();
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["result"]["tools"].as_array().unwrap().len(), 2);

        drop(client);
        // Bounded, not a live-timing dependency of this crate's own logic (D8 governs
        // production output paths, not a test-harness safety net): if the EOF-on-drop fix
        // above ever regresses, this fails the test in 5s instead of hanging the whole
        // suite the way the earlier, unfixed version of this test did.
        tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("server task must exit once the client side is dropped")
            .expect("server task must not panic")
            .expect("serve() must return Ok once it observes EOF");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
