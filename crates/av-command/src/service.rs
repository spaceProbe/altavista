//! `altavista.v1.CommandAuthorityService` (A1.3, `docs/aiplane-plan.md` milestone A1's final
//! piece): `Propose`, `Check`, `Authorize`, `Dispatch`, `Ack`, `Query`, `VerifyLedger` over
//! the state machine ([`crate::state`]) and ledger ([`crate::ledger::Ledger`]) this crate's
//! library modules already implement. Every RPC below is a thin wire adapter: it never
//! contains state-machine or policy logic of its own, only request validation, a call into
//! [`crate::state`]/[`crate::authority`]/[`crate::ledger`], a ledger append (when the state
//! module itself did not already do one -- [`crate::authority::check_command`] appends for
//! `Check`; every other RPC appends here), and a [`tonic::Status`] mapping for the result.
//!
//! # In-memory index
//!
//! [`CommandAuthorityServiceImpl`] holds every `Command` it has ever `Propose`d, keyed by
//! `Command.id`, in a `BTreeMap` (ADR-004's determinism rule: no `HashMap` iteration on any
//! output path -- [`CommandAuthorityServiceImpl::query`]'s "by entity" case iterates this map
//! and its result order is therefore deterministic, sorted by `Command.id`, never insertion
//! order). This index is this process's only memory of a command between RPCs; it is not
//! itself durable -- the durable record is the ledger, and [`CommandAuthorityServiceImpl::
//! verify_ledger`] answers straight from `Ledger::verify`, never from this map.
//!
//! # `tonic::Status` code per refusal kind
//!
//! Every typed refusal this module can produce maps to exactly one gRPC status code,
//! deliberately chosen, and the mapping ([`to_status`]) always preserves the typed error's
//! own `Display` text verbatim as the status message (never a generic "operation failed"):
//!
//! - [`state::CommandError::IllegalTransition`] -> **`FAILED_PRECONDITION`**. The request
//!   itself is well-formed; what is wrong is that the `Command` this service is holding for
//!   `command_id` is not currently in a state that has an edge to the one this RPC attempts
//!   -- gRPC's own definition of `FAILED_PRECONDITION` ("the system is not in a state
//!   required for the operation's execution") names exactly this case. `Authorize`-before-
//!   `Check`, `Dispatch`-before-`Authorize` and `Ack`-before-`Dispatch` all land here.
//! - [`state::CommandError::AlreadyStarted`] -> **`INVALID_ARGUMENT`**. Unlike the case
//!   above, this is a property of the *request's own payload*: `Propose` was given a
//!   `Command` whose `state`/`transitions` are already populated, which `Propose` never
//!   accepts regardless of what this service is holding in its index (indeed, this can fire
//!   for a `command_id` this service has never seen at all) -- a bad request, not a stored
//!   command in the wrong state.
//! - [`state::CommandError::EnvelopeNotAllowed`] -> **`INVALID_ARGUMENT`**, same reasoning:
//!   `envelope_id` is a field on the request's own `Command` that question 53 (propose-only)
//!   forbids setting at all in this track, not a precondition of anything already stored.
//! - **Unknown `command_id`** ([`ServiceError::NotFound`]) -> **`NOT_FOUND`**: the plain gRPC
//!   meaning, this service's in-memory index has no `Command` under that id.
//! - **A `Dispatch` whose `Command.idempotency_key` was already dispatched**
//!   ([`ServiceError::DuplicateIdempotencyKey`]) -> **`ALREADY_EXISTS`**: gRPC's own meaning
//!   ("the entity that a client attempted to create already exists") fits a repeat dispatch
//!   exactly -- the dispatch this request asks for has already happened.
//! - **Ledger/rate-source I/O failure** ([`ServiceError::Io`], and
//!   [`crate::authority::CheckCommandError::Io`]) -> **`INTERNAL`**: an unexpected
//!   server-side storage fault, never something the caller's request could have avoided.
//! - **`Authorize`'s `principal_token` fails OIDC verification** ([`crate::oidc::
//!   TokenError`], A2.1) -> **`UNAUTHENTICATED`**: chosen deliberately over
//!   `PERMISSION_DENIED` or `INVALID_ARGUMENT` because gRPC's own definition of
//!   `UNAUTHENTICATED` ("the request does not have valid authentication credentials for the
//!   operation") names exactly this case -- the request's *identity claim itself* could not
//!   be established, which is a different failure from "this identity is known but not
//!   allowed" (`PERMISSION_DENIED`, A2.2's job, not built here) or "this request is malformed"
//!   (`INVALID_ARGUMENT`, already used above for a genuinely malformed request payload). The
//!   status message is the [`crate::oidc::TokenError`]'s own `Display` text, which -- see
//!   that module's doc -- never contains the raw token string or the raw signature bytes;
//!   `tests/grpc_service.rs`'s
//!   `authorize_refusal_message_never_contains_the_token_or_signature` (an integration test,
//!   since it needs a real running service) asserts this directly against a real refusal.
//!
//! # Idempotency ([`CommandAuthorityServiceImpl::dispatch`]) -- a guarantee that survives a
//! restart, not just an in-memory guard
//!
//! A non-empty `Command.idempotency_key` is recorded in an in-memory `BTreeSet` the instant
//! a `Dispatch` for it succeeds, under the same lock guard that checked it was absent and
//! that performed the state transition and ledger append -- so a second `Dispatch` racing
//! the first either sees the key already present (refused, `ALREADY_EXISTS`, no second
//! ledger record) or blocks until the first has finished recording it. That in-memory set is
//! **not** built empty at construction: [`CommandAuthorityServiceImpl::new`] rebuilds it from
//! the ledger itself ([`crate::ledger::Ledger::scan_dispatched_idempotency_keys`]), so a
//! process that restarts and reopens the same ledger directory refuses exactly the keys a
//! prior process lifetime already dispatched, before it has served a single RPC of its own
//! -- `command.proto`'s own doc comment on `Command.idempotency_key` ("the edge never
//! dispatches the same key twice") carries no process-lifetime qualifier, and this is what
//! makes that true from the durable ledger, not from RAM alone. `LedgerRecord.
//! idempotency_key` (`authority.proto`, A1.3) is what makes the rebuild possible: every
//! record carries it (mirroring `command_class`'s own "carried on every record" rule), so the
//! rebuild scan reads it straight off a `COMMAND_STATE_DISPATCHED` record with no second
//! index or join.
//!
//! An **empty** `idempotency_key` is never tracked at all (a command that opts out of
//! deduplication by not declaring one) -- neither at `Dispatch` time nor by the ledger
//! rebuild scan; this is a deliberate reading of `command.proto`'s own doc comment as being
//! about the key's *value*, not about "every command, keyed or not, dispatches at most
//! once" -- the latter is already true by construction (`DISPATCHED` has no legal re-entry
//! from itself; `Dispatch` can only ever be called once per command's own lifetime via the
//! `AUTHORIZED -> DISPATCHED` edge).
//!
//! **What is *not* rebuilt from the ledger**: the `commands` index behind `Query` (and every
//! other RPC's `command_id` lookup) is process-lifetime only, a documented completeness gap,
//! not a safety one -- see [`CommandAuthorityServiceImpl`]'s own doc comment for exactly what
//! that means and why it is not closed by this task.
//!
//! # `DispatchSink` -- not A3
//!
//! [`DispatchSink`] is the one seam this module defines for A3 to fill: the real kernel
//! telecommand binding (`docs/aiplane-plan.md` milestone A3) is out of this task's scope --
//! this task's brief says plainly "do not build it" and "do not touch `crates/av-kernel`".
//! [`RecordingDispatchSink`] is the only implementor this crate ships, recording every
//! dispatched `Command` for a test to inspect; nothing behind it talks to any transport.
//!
//! # A2.1: `Authorize` verifies *who*, not *whether* -- that split is deliberate
//!
//! [`CommandAuthorityServiceImpl::authorize`] now verifies `AuthorizeRequest.principal_token`
//! for real, against the [`crate::oidc::IssuerConfig`] this service was constructed with
//! ([`crate::oidc::verify`], evaluated against `self.clock.now_tai_ns()` -- the injected
//! clock, never the wall clock read a second time here). An unverifiable token never reaches
//! [`crate::state::authorize`] at all: it is refused `UNAUTHENTICATED` before any state
//! transition is attempted or any ledger record is appended (see the module doc's status-code
//! section for why `UNAUTHENTICATED`). On success, the **verified** `Principal.sub` --
//! never the raw `principal_token` string -- is what gets recorded as `CommandTransition.
//! principal`, with [`AUTHORIZE_VERIFIED_REASON`] replacing A1.3's `AUTHORIZE_UNVERIFIED_
//! REASON` (which no longer exists in this crate -- there is no path left that claims
//! identity is unverified when it is not).
//!
//! **This is identity, not authorization.** A2.2 (`docs/aiplane-plan.md`) is the milestone
//! that reads `Principal.groups`/`amr`/`acr` for a role/MFA gate and evaluates
//! `AuthorizeRequest.delegation_id` against the injected clock for expiry. A2.1 (this task)
//! does neither: `delegation_id` is recorded on the transition exactly as A1.3 left it --
//! carried through unevaluated -- and nothing here reads `Principal.groups`/`amr`/`acr` for
//! any decision at all. A verified principal with *any* subject and *any* claims authorizes
//! *any* command class today; closing that gap is explicitly A2.2's job, not this task's, per
//! `docs/aiplane-plan.md`'s own milestone split and this task's brief ("do not build it").
//! `docs/compliance/av-command/control-matrix.md`'s IA rows (3.5.x) reflect A2.1 landing;
//! its AC rows (3.1.x, the authorization half) are left exactly as they were.
//!
//! # Transport (question 155/84)
//!
//! [`resolve_loopback_bind_address`] refuses a non-loopback bind address with a typed [`BindAddressError`]
//! naming question 155, mirroring `crates/av-kernel/src/drm/binding.rs`'s own
//! `is_loopback_address` (a private copy here -- this crate must not depend on or edit
//! `crates/av-kernel`): the literal string `"localhost"` (case-insensitive) or an IPv4/IPv6
//! loopback literal are the only addresses this service's gRPC and admin listeners ever
//! bind. mTLS across a host boundary is the service-owned nginx front's job (question 84),
//! never this crate's own transport -- this crate adds no crypto-adjacent dependency of any
//! kind beyond the `openssl` crate [`crate::ledger`] already uses for SHA-256.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tonic::{Code, Request, Response, Status};

use av_cdm::pb::{
    query_request::Selector as QuerySelector, AckLevel, AckRequest, AuthorizeRequest, ChainVerification, CheckRequest, Command,
    CommandResponse, CommandState, DispatchRequest, ProposeRequest, QueryByEntity, QueryRequest, QueryResponse,
    VerifyLedgerRequest, VerifyLedgerResponse,
};

use crate::authority::{self, CheckCommandError};
use crate::clock::Clock;
use crate::ledger::Ledger;
use crate::oidc::{self, IssuerConfig};
use crate::policy::PolicyBundle;
use crate::rate::LedgerRateSource;
use crate::state::{self, CommandError};

pub use crate::pb::command_authority_service_server::{CommandAuthorityService as CommandAuthorityServiceTrait, CommandAuthorityServiceServer};

/// The principal recorded on a `DISPATCHED` transition -- this service itself, not a
/// caller-supplied identity; matches `crates/av-command/src/state.rs`'s own test convention
/// (`dispatch(c, "ground-segment", ...)`).
pub const DISPATCH_PRINCIPAL: &str = "ground-segment";

/// The `CommandTransition.reason` text `Authorize` (A2.1) writes for every request that
/// reaches a transition (i.e. every request whose token verified) -- see the module doc's
/// "A2.1: `Authorize` verifies *who*, not *whether*" section. This text is not a refusal, it
/// is what a *successful* A2.1 `Authorize` honestly says about itself: identity is real,
/// authorization is not yet gated.
pub const AUTHORIZE_VERIFIED_REASON: &str =
    "authorize: principal_token verified against the configured OIDC issuer (crate::oidc); \
     the recorded principal is the token's verified sub claim, not the raw token. Role, MFA \
     and delegation-expiry gating are A2.2 (docs/aiplane-plan.md), not yet built -- this \
     milestone (A2.1) verifies who the caller is, not whether they may authorize this command \
     class; delegation_id is recorded unevaluated for A2.2 to enforce.";

/// The `CommandTransition.reason` text `Dispatch` writes -- naming the seam A3 fills, not a
/// real transport (see the module doc's "`DispatchSink` -- not A3" section).
pub const DISPATCH_REASON: &str = "handed to the DispatchSink (docs/aiplane-plan.md milestone A3 supplies the real kernel binding behind it)";

/// Hands an `AUTHORIZED`-turned-`DISPATCHED` [`Command`] to whatever transport reaches the
/// simulated asset. The one seam A3 (`docs/aiplane-plan.md`) fills with the real
/// `crates/av-kernel` telecommand binding; this crate defines the trait and one recording
/// implementor only -- see the module doc's "`DispatchSink` -- not A3" section.
pub trait DispatchSink: Send + Sync {
    fn dispatch(&self, command: &Command);
}

/// The only [`DispatchSink`] this crate ships: records every dispatched [`Command`] in
/// order, for a test (or an operator console, later) to inspect. Never talks to any
/// transport.
#[derive(Default)]
pub struct RecordingDispatchSink {
    dispatched: Mutex<Vec<Command>>,
}

impl RecordingDispatchSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `Command` handed to [`DispatchSink::dispatch`] so far, in the order it happened.
    pub fn dispatched(&self) -> Vec<Command> {
        self.dispatched.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl DispatchSink for RecordingDispatchSink {
    fn dispatch(&self, command: &Command) {
        self.dispatched.lock().unwrap_or_else(|p| p.into_inner()).push(command.clone());
    }
}

/// Everything a [`CommandAuthorityServiceImpl`] RPC can fail with, before it is mapped to a
/// [`tonic::Status`] by [`to_status`]. See the module doc's status-code section for the
/// mapping and the reasoning behind each choice.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// `command_id` names no `Command` this service's in-memory index has ever `Propose`d.
    #[error("command {0:?} not found")]
    NotFound(String),
    #[error(transparent)]
    State(#[from] CommandError),
    #[error(transparent)]
    Check(#[from] CheckCommandError),
    /// `Dispatch` was called for a `Command` whose `idempotency_key` (non-empty) was already
    /// dispatched once -- see the module doc's "Idempotency" section.
    #[error("dispatch refuses to dispatch idempotency_key {0:?} twice")]
    DuplicateIdempotencyKey(String),
    /// A ledger append or ledger/rate-source read failed.
    #[error("ledger/rate I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A2.1: `Authorize`'s `principal_token` failed OIDC verification. See
    /// [`crate::oidc::TokenError`] for the full refusal vocabulary and the module doc's
    /// status-code section for why this maps to `UNAUTHENTICATED`.
    #[error("authorize: token verification failed: {0}")]
    TokenInvalid(#[from] oidc::TokenError),
}

/// Maps one [`ServiceError`] to the [`tonic::Status`] the module doc's status-code section
/// commits to, preserving the typed error's own message text verbatim.
fn to_status(err: ServiceError) -> Status {
    match err {
        ServiceError::NotFound(_) => Status::new(Code::NotFound, err.to_string()),
        ServiceError::State(ref state_err) => {
            let code = match state_err {
                CommandError::IllegalTransition { .. } => Code::FailedPrecondition,
                CommandError::AlreadyStarted { .. } => Code::InvalidArgument,
                CommandError::EnvelopeNotAllowed { .. } => Code::InvalidArgument,
            };
            Status::new(code, err.to_string())
        }
        ServiceError::Check(CheckCommandError::State(ref state_err)) => {
            let code = match state_err {
                CommandError::IllegalTransition { .. } => Code::FailedPrecondition,
                CommandError::AlreadyStarted { .. } => Code::InvalidArgument,
                CommandError::EnvelopeNotAllowed { .. } => Code::InvalidArgument,
            };
            Status::new(code, err.to_string())
        }
        ServiceError::Check(CheckCommandError::Io(_)) => Status::new(Code::Internal, err.to_string()),
        ServiceError::DuplicateIdempotencyKey(_) => Status::new(Code::AlreadyExists, err.to_string()),
        ServiceError::Io(_) => Status::new(Code::Internal, err.to_string()),
        ServiceError::TokenInvalid(_) => Status::new(Code::Unauthenticated, err.to_string()),
    }
}

/// The `CommandAuthorityService` implementation. Owns a [`Ledger`], a [`PolicyBundle`], the
/// injected [`Clock`], a [`DispatchSink`] and two pieces of in-process state built at
/// construction ([`Self::new`]) -- see this module's doc for which of the two is a
/// durability-backed *safety* property and which is a documented, unclosed *completeness*
/// gap:
///
/// - `dispatched_idempotency_keys` is **rebuilt from the ledger** at every construction
///   ([`crate::ledger::Ledger::scan_dispatched_idempotency_keys`]) -- the duplicate-dispatch
///   guarantee this set backs (`command.proto`'s own doc comment on `Command.
///   idempotency_key`: "the edge never dispatches the same key twice", stated with no
///   process-lifetime qualifier) therefore survives a process restart: a second
///   `CommandAuthorityServiceImpl` constructed over the *same* ledger directory refuses the
///   same key `Dispatch::dispatch` would have refused in the first process, before this
///   process has ever handled a single RPC of its own.
/// - `commands` (the in-memory `Command` index the module doc describes) is **not** rebuilt
///   from the ledger at construction, and this is a **documented gap, not an oversight**:
///   `LedgerRecord` does not carry enough of `Command` to reconstruct one (no `entity_id`
///   beyond the partition key already implies it, no `payload`, no `deadline_tai_ns`, no
///   `label`/`provenance`) -- closing this needs either widening `LedgerRecord` to carry the
///   full `Command` or a separate durable command store, a ledger-shape decision out of this
///   task's scope. The practical effect: `Check`/`Authorize`/`Dispatch`/`Ack`/`Query` against
///   a `command_id` a *previous* process lifetime `Propose`d are refused `NOT_FOUND` after a
///   restart, even though the ledger itself still holds that command's full transition
///   history. See `docs/compliance/av-command/control-matrix.md`'s Deficiency 7 for the
///   compliance-facing record of this same gap.
///
/// Constructed once by `src/bin/av-command.rs` and shared (`Arc`) across every accepted
/// connection -- every field here is `Send + Sync` and every method takes `&self`.
pub struct CommandAuthorityServiceImpl {
    ledger: Arc<Ledger>,
    bundle: Arc<PolicyBundle>,
    rate_window_ns: i64,
    clock: Arc<dyn Clock>,
    dispatch_sink: Arc<dyn DispatchSink>,
    /// A2.1: the issuer `Authorize` verifies `AuthorizeRequest.principal_token` against --
    /// see the module doc's "A2.1: `Authorize` verifies *who*, not *whether*" section.
    issuer_config: Arc<IssuerConfig>,
    commands: Mutex<BTreeMap<String, Command>>,
    dispatched_idempotency_keys: Mutex<BTreeSet<String>>,
}

impl CommandAuthorityServiceImpl {
    /// Fails only if [`Ledger::scan_dispatched_idempotency_keys`] fails (a real I/O error
    /// reading the ledger directory this process is about to serve from) -- never silently
    /// starts with an empty duplicate-dispatch guard when the ledger it was asked to rebuild
    /// that guard from could not actually be read.
    pub fn new(
        ledger: Arc<Ledger>,
        bundle: Arc<PolicyBundle>,
        rate_window_ns: i64,
        clock: Arc<dyn Clock>,
        dispatch_sink: Arc<dyn DispatchSink>,
        issuer_config: Arc<IssuerConfig>,
    ) -> io::Result<Self> {
        let dispatched_idempotency_keys = ledger.scan_dispatched_idempotency_keys()?;
        Ok(Self {
            ledger,
            bundle,
            rate_window_ns,
            clock,
            dispatch_sink,
            issuer_config,
            commands: Mutex::new(BTreeMap::new()),
            dispatched_idempotency_keys: Mutex::new(dispatched_idempotency_keys),
        })
    }

    fn get_command(&self, command_id: &str) -> Result<Command, ServiceError> {
        self.commands
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(command_id)
            .cloned()
            .ok_or_else(|| ServiceError::NotFound(command_id.to_string()))
    }

    fn put_command(&self, command: Command) {
        self.commands.lock().unwrap_or_else(|p| p.into_inner()).insert(command.id.clone(), command);
    }

    /// Appends one ledger record for `command`'s own last transition, with no attached
    /// `PolicyDecision` (the `Check` RPC is the only caller that attaches one, and it goes
    /// through `crate::authority::check_command` instead of this helper -- see that
    /// function's own `Ledger::append` call).
    fn append_last_transition(&self, command: &Command) -> Result<(), ServiceError> {
        let transition = command
            .transitions
            .last()
            .expect("every state:: edge function appends exactly one transition before returning Ok")
            .clone();
        self.ledger
            .append(
                crate::ledger::CommandMeta::new(&command.entity_id, &command.id, &command.command_class, &command.idempotency_key),
                transition,
                None,
                &*self.clock,
            )
            .map_err(ServiceError::Io)?;
        Ok(())
    }
}

#[tonic::async_trait]
impl CommandAuthorityServiceTrait for CommandAuthorityServiceImpl {
    async fn propose(&self, request: Request<ProposeRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let proposal = req.proposal.ok_or_else(|| Status::invalid_argument("proposal is required"))?;
        let command = proposal.command.ok_or_else(|| Status::invalid_argument("proposal.command is required"))?;
        if command.id.is_empty() {
            return Err(Status::invalid_argument("proposal.command.id must not be empty"));
        }
        if command.entity_id.is_empty() {
            return Err(Status::invalid_argument("proposal.command.entity_id must not be empty"));
        }

        let proposed = state::propose(command, &req.principal, "proposed via CommandAuthorityService.Propose", &*self.clock).map_err(|e| to_status(e.into()))?;
        self.append_last_transition(&proposed).map_err(to_status)?;
        self.put_command(proposed.clone());
        Ok(Response::new(CommandResponse { command: Some(proposed), decision: None }))
    }

    async fn check(&self, request: Request<CheckRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let command = self.get_command(&req.command_id).map_err(to_status)?;
        let rate_source = LedgerRateSource::new(&self.ledger);
        let result = authority::check_command(command, &self.bundle, self.rate_window_ns, &rate_source, &self.ledger, &*self.clock)
            .map_err(|e| to_status(e.into()))?;
        self.put_command(result.command.clone());
        Ok(Response::new(CommandResponse { command: Some(result.command), decision: Some(result.decision) }))
    }

    async fn authorize(&self, request: Request<AuthorizeRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let command = self.get_command(&req.command_id).map_err(to_status)?;

        // A2.1: verify identity before attempting any state transition -- an unverifiable
        // token never reaches state::authorize, never appends a ledger record, and never
        // touches this service's in-memory index. See the module doc's "A2.1: `Authorize`
        // verifies *who*, not *whether*" section.
        let now_tai_ns = self.clock.now_tai_ns();
        let principal =
            oidc::verify(&req.principal_token, &self.issuer_config, now_tai_ns).map_err(|e| to_status(ServiceError::TokenInvalid(e)))?;

        let authorized = state::authorize(command, &principal.sub, AUTHORIZE_VERIFIED_REASON, &req.delegation_id, &*self.clock)
            .map_err(|e| to_status(e.into()))?;
        self.append_last_transition(&authorized).map_err(to_status)?;
        self.put_command(authorized.clone());
        Ok(Response::new(CommandResponse { command: Some(authorized), decision: None }))
    }

    async fn dispatch(&self, request: Request<DispatchRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let command = self.get_command(&req.command_id).map_err(to_status)?;

        // Held across the state edge and the ledger append (both synchronous, no `.await`
        // point in between) so a racing second Dispatch for the same key either sees it
        // already present or blocks until this one finishes recording it -- see the module
        // doc's "Idempotency" section.
        let mut seen = self.dispatched_idempotency_keys.lock().unwrap_or_else(|p| p.into_inner());
        if !command.idempotency_key.is_empty() && seen.contains(&command.idempotency_key) {
            return Err(to_status(ServiceError::DuplicateIdempotencyKey(command.idempotency_key.clone())));
        }

        let dispatched = state::dispatch(command, DISPATCH_PRINCIPAL, DISPATCH_REASON, &*self.clock).map_err(|e| to_status(e.into()))?;
        self.append_last_transition(&dispatched).map_err(to_status)?;
        if !dispatched.idempotency_key.is_empty() {
            seen.insert(dispatched.idempotency_key.clone());
        }
        drop(seen);

        self.dispatch_sink.dispatch(&dispatched);
        self.put_command(dispatched.clone());
        Ok(Response::new(CommandResponse { command: Some(dispatched), decision: None }))
    }

    async fn ack(&self, request: Request<AckRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let command = self.get_command(&req.command_id).map_err(to_status)?;
        let ack_level = AckLevel::try_from(req.ack_level).unwrap_or(AckLevel::Unspecified);
        let acked = state::ack(command, &req.principal, &req.reason, ack_level, &*self.clock).map_err(|e| to_status(e.into()))?;
        self.append_last_transition(&acked).map_err(to_status)?;
        self.put_command(acked.clone());
        Ok(Response::new(CommandResponse { command: Some(acked), decision: None }))
    }

    async fn query(&self, request: Request<QueryRequest>) -> Result<Response<QueryResponse>, Status> {
        let req = request.into_inner();
        let commands = self.commands.lock().unwrap_or_else(|p| p.into_inner());
        let results: Vec<Command> = match req.selector {
            Some(QuerySelector::CommandId(id)) => match commands.get(&id) {
                Some(c) => vec![c.clone()],
                None => return Err(Status::not_found(format!("command {id:?} not found"))),
            },
            Some(QuerySelector::Entity(QueryByEntity { entity_id, state_filter })) => commands
                .values()
                .filter(|c| c.entity_id == entity_id && (state_filter == CommandState::Unspecified as i32 || c.state == state_filter))
                .cloned()
                .collect(),
            None => return Err(Status::invalid_argument("selector (command_id or entity) is required")),
        };
        Ok(Response::new(QueryResponse { commands: results }))
    }

    async fn verify_ledger(&self, request: Request<VerifyLedgerRequest>) -> Result<Response<VerifyLedgerResponse>, Status> {
        let req = request.into_inner();
        let partitions: Vec<String> = if req.partition.is_empty() {
            self.ledger
                .partitions()
                .map_err(|e| to_status(ServiceError::Io(e)))?
                .into_iter()
                .map(|p| p.partition)
                .collect()
        } else {
            vec![req.partition]
        };

        let mut results: Vec<ChainVerification> = Vec::with_capacity(partitions.len());
        let mut ok = true;
        for partition in &partitions {
            let result = self.ledger.verify(partition).map_err(|e| to_status(ServiceError::Io(e)))?;
            ok &= result.ok;
            results.push(result);
        }
        Ok(Response::new(VerifyLedgerResponse { results, ok }))
    }
}

/// Every way validating or resolving a bind address can fail -- see the module doc's
/// "Transport (question 155/84)" section.
#[derive(Debug, Error)]
pub enum BindAddressError {
    /// `raw` is plaintext (this crate links no TLS stack of any kind) and is not a
    /// recognized loopback address.
    #[error(
        "bind address {raw:?} is plaintext and is not a recognized loopback address \
         (127.0.0.0/8, ::1, or \"localhost\") -- av-command's gRPC and admin listeners are \
         plaintext only on loopback within one host; mTLS across a host boundary is the \
         service-owned nginx front's job (question 84), not this crate's own transport \
         (question 155)"
    )]
    NotLoopback { raw: String },
    /// `raw` is the recognized loopback hostname `"localhost"`, spelled with no `:port` at
    /// all -- there is nothing to bind without a port.
    #[error("bind address {raw:?} is a recognized loopback host (\"localhost\") but has no :port")]
    MissingPort { raw: String },
    /// `raw` is `"localhost:<all-digit string>"` (the only way [`is_loopback_address`] can
    /// accept a non-numeric-`SocketAddr` address), but the digit string does not fit a
    /// `u16` port (e.g. it is larger than `65535`).
    #[error("bind address {raw:?} is a recognized loopback host but its port does not fit u16: {detail}")]
    UnparseablePort { raw: String, detail: String },
}

/// Question 155: is `address` (a bare `"host:port"` string) a recognized loopback endpoint?
/// A private copy of `crates/av-kernel/src/drm/binding.rs`'s `is_loopback_address` (this
/// crate must not depend on or edit `crates/av-kernel`) -- same rule, same reasoning:
/// recognizes the literal string `"localhost"` (case-insensitive) and any IPv4/IPv6 literal
/// `std::net::IpAddr::is_loopback` accepts, `[bracketed]` for IPv6 the way a `"host:port"`
/// string spells it. **Deliberately no DNS resolution**: an unrecognized hostname is treated
/// as non-loopback and refused, never resolved and then trusted (which would make the
/// refusal depend on the resolver's answer at load time, non-deterministic across
/// hosts/runs -- ADR-004).
fn is_loopback_address(address: &str) -> bool {
    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => address,
    };
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Validates `raw` against [`is_loopback_address`] and resolves it to a concrete
/// [`SocketAddr`] to actually bind -- with **no I/O of any kind, not even a local DNS
/// lookup**: a numeric address (`"127.0.0.1:50110"`, `"[::1]:50110"`) parses directly; the
/// literal hostname `"localhost"` is substituted with the IPv4 loopback address
/// [`Ipv4Addr::LOCALHOST`] by this function itself, never resolved through the OS resolver
/// (even though resolving `"localhost"` would, in practice, never reach the network -- this
/// function is a pure string transform regardless, so it needs no such argument to be
/// correct). Called by `src/bin/av-command.rs` for both the gRPC and admin bind addresses.
pub fn resolve_loopback_bind_address(raw: &str) -> Result<SocketAddr, BindAddressError> {
    if !is_loopback_address(raw) {
        return Err(BindAddressError::NotLoopback { raw: raw.to_string() });
    }
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }
    // is_loopback_address accepted `raw` but it did not parse directly as a numeric
    // SocketAddr -- the only other way its own guard can accept a string is either the bare
    // literal "localhost" with no ':' at all (its own `rsplit_once` returns `None`, so the
    // *whole* string is checked as the host), or "localhost:<all-ASCII-digit string>" (its
    // guard requires the port half to be all digits before it will even split on the last
    // ':' -- see is_loopback_address's own doc). Both are handled explicitly below, rather
    // than assumed, so a change to is_loopback_address's own guard can never turn this into
    // a panic.
    let Some((_, port)) = raw.rsplit_once(':') else {
        return Err(BindAddressError::MissingPort { raw: raw.to_string() });
    };
    let port: u16 = port
        .parse()
        .map_err(|e: std::num::ParseIntError| BindAddressError::UnparseablePort { raw: raw.to_string(), detail: e.to_string() })?;
    Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_loopback_bind_address_accepts_numeric_loopback_literals() {
        assert_eq!(resolve_loopback_bind_address("127.0.0.1:50110").unwrap(), "127.0.0.1:50110".parse().unwrap());
        assert_eq!(resolve_loopback_bind_address("[::1]:50110").unwrap(), "[::1]:50110".parse().unwrap());
    }

    #[test]
    fn resolve_loopback_bind_address_substitutes_localhost_with_no_io() {
        let addr = resolve_loopback_bind_address("localhost:50110").unwrap();
        assert_eq!(addr, SocketAddr::from((Ipv4Addr::LOCALHOST, 50110)));
        let addr = resolve_loopback_bind_address("LOCALHOST:50111").unwrap();
        assert_eq!(addr, SocketAddr::from((Ipv4Addr::LOCALHOST, 50111)));
    }

    /// **Question 155's own acceptance line, at this crate's bind boundary.**
    #[test]
    fn resolve_loopback_bind_address_refuses_every_non_loopback_spelling() {
        for raw in ["0.0.0.0:50110", "10.0.0.5:50110", "example.com:50110", "8.8.8.8:443", "[::]:50110"] {
            let err = resolve_loopback_bind_address(raw).unwrap_err();
            assert!(matches!(err, BindAddressError::NotLoopback { .. }), "{raw:?} -> {err:?}");
            assert!(err.to_string().contains("question 155"), "{err}");
        }
    }

    /// `is_loopback_address`'s own guard requires the port half to be all ASCII digits
    /// before it will treat the address as `"host:port"` at all -- a non-digit port makes
    /// the *whole* string get checked as the host, which fails, so this is `NotLoopback`
    /// (fail closed on a garbled port), never a distinct "bad port" error.
    #[test]
    fn resolve_loopback_bind_address_treats_a_non_digit_port_as_not_loopback_at_all() {
        let err = resolve_loopback_bind_address("localhost:not-a-port").unwrap_err();
        assert!(matches!(err, BindAddressError::NotLoopback { .. }), "{err:?}");
    }

    /// The bare literal `"localhost"` with no `:port` at all is a recognized loopback host
    /// (satisfies `is_loopback_address`) but this crate cannot bind it -- a typed
    /// `MissingPort`, never a panic.
    #[test]
    fn resolve_loopback_bind_address_refuses_a_bare_localhost_with_no_port() {
        let err = resolve_loopback_bind_address("localhost").unwrap_err();
        assert!(matches!(err, BindAddressError::MissingPort { .. }), "{err:?}");
    }

    /// An all-digit port that does not fit `u16` (larger than `65535`) is the one way
    /// `UnparseablePort` is actually reachable: `is_loopback_address`'s guard only checks
    /// that every byte is an ASCII digit, not that the number fits a 16-bit port.
    #[test]
    fn resolve_loopback_bind_address_refuses_a_port_that_overflows_u16() {
        let err = resolve_loopback_bind_address("localhost:99999999999").unwrap_err();
        assert!(matches!(err, BindAddressError::UnparseablePort { .. }), "{err:?}");
    }

    #[test]
    fn recording_dispatch_sink_records_every_command_in_order() {
        let sink = RecordingDispatchSink::new();
        let a = Command { id: "a".to_string(), ..Command::default() };
        let b = Command { id: "b".to_string(), ..Command::default() };
        sink.dispatch(&a);
        sink.dispatch(&b);
        let recorded = sink.dispatched();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].id, "a");
        assert_eq!(recorded[1].id, "b");
    }
}
