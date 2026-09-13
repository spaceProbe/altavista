//! `altavista.v1.CommandAuthorityService` (A1.3, `docs/aiplane-plan.md` milestone A1's final
//! piece): `Propose`, `Check`, `Authorize`, `Dispatch`, `Ack`, `Query`, `VerifyLedger` over
//! the state machine ([`crate::state`]) and ledger ([`crate::ledger::Ledger`]) this crate's
//! library modules already implement. Every RPC below is a thin wire adapter: it never
//! contains state-machine or policy logic of its own, only request validation, a call into
//! [`crate::state`]/[`crate::authority`]/[`crate::ledger`], a ledger append (when the state
//! module itself did not already do one -- [`crate::authority::check_command`] appends for
//! `Check`; every other RPC appends here), and a [`tonic::Status`] mapping for the result.
//!
//! # In-memory index -- rebuilt from the ledger, not durable on its own (question 203(a))
//!
//! [`CommandAuthorityServiceImpl`] holds every `Command` it knows about, keyed by
//! `Command.id`, in a `BTreeMap` (ADR-004's determinism rule: no `HashMap` iteration on any
//! output path -- [`CommandAuthorityServiceImpl::query`]'s "by entity" case iterates this map
//! and its result order is therefore deterministic, sorted by `Command.id`, never insertion
//! order). The map itself is process-memory, not the durable record -- the ledger is, and
//! [`CommandAuthorityServiceImpl::verify_ledger`] answers straight from `Ledger::verify`,
//! never from this map -- but [`CommandAuthorityServiceImpl::new`] rebuilds it from the
//! ledger ([`crate::ledger::Ledger::scan_commands`]) before serving a single RPC, exactly the
//! way it already rebuilds `dispatched_idempotency_keys` (see the "Idempotency" section
//! below): a process that restarts and reopens the same ledger directory answers `Query` (and
//! every other RPC's `command_id` lookup) for a command a *prior* process lifetime `Propose`d,
//! not only ones this process lifetime has itself seen. This is what closes the manager's own
//! open item from the previous round ("`Query` does not survive a restart") -- the lead's
//! ratified fix (question 203(a)): "carrying the full `Command` on `LedgerRecord` ..., not by
//! re-deriving it" (`authority.proto`'s `LedgerRecord.command` doc comment, field 11).
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
//!   allowed" (`PERMISSION_DENIED`, see the next bullet) or "this request is malformed"
//!   (`INVALID_ARGUMENT`, already used above for a genuinely malformed request payload). The
//!   status message is the [`crate::oidc::TokenError`]'s own `Display` text, which -- see
//!   that module's doc -- never contains the raw token string or the raw signature bytes;
//!   `tests/grpc_service.rs`'s
//!   `authorize_refusal_message_never_contains_the_token_or_signature` (an integration test,
//!   since it needs a real running service) asserts this directly against a real refusal.
//! - **`Authorize`'s role/MFA/delegation gate refuses** ([`crate::authz::AuthzError`], A2.2)
//!   -> **`PERMISSION_DENIED`**, deliberately distinct from the bullet above's
//!   `UNAUTHENTICATED`: by the time [`crate::authz::authorize_command`] ever runs,
//!   [`crate::oidc::verify`] has *already* established who the caller is (a real, verified
//!   `Principal`) -- what `AuthzError` reports is that this real, known identity is not
//!   allowed to do the specific thing it asked for, which is exactly gRPC's own definition of
//!   `PERMISSION_DENIED` ("the caller does not have permission to execute the specified
//!   operation ... it must not be used for rejections caused by exhausting some resource ...
//!   It must not be used if the caller cannot be identified" -- the last clause is precisely
//!   why `UNAUTHENTICATED` and `PERMISSION_DENIED` cannot be the same code here). A denied
//!   `Authorize` attempt (wrong role, missing MFA, or an invalid/expired delegation) still
//!   emits an [`crate::audit`] line before this module returns the `Status` -- see
//!   [`CommandAuthorityServiceImpl::authorize`]'s own body.
//! - **`Dispatch`/`Ack`/`Expire`/`Fail`'s `service_token` fails OIDC verification** (R3.1,
//!   [`crate::oidc::TokenError`]) -> **`UNAUTHENTICATED`**, the identical mapping and identical
//!   reasoning as `Authorize`'s `principal_token` above -- there is no second verifier and no
//!   second status-code decision for it.
//! - **A verified `service_token` names no service role granting the RPC being called** (R3.1,
//!   [`crate::authz::ServiceAuthzError`]) -> **`PERMISSION_DENIED`**, the identical reasoning as
//!   `Authorize`'s `AuthzError` bullet above: identity is already established
//!   ([`crate::oidc::verify`] already succeeded), what is refused is whether this real, known
//!   identity may do this specific thing. This is also the refusal a **purely human** token
//!   gets on any of these four RPCs (its groups are never a `service_roles` key at all,
//!   [`crate::authz::check_service_roles_disjoint`]'s own construction) -- see the module doc's
//!   "R3.1: a service principal, verified like a human token" section below for why that is
//!   exactly the right refusal, not a distinct one.
//! - **`Ack`/`Expire`/`Fail`'s caller-declared `principal` disagrees with the verified
//!   `service_token` subject** (R3.1, [`ServiceError::PrincipalMismatch`]) ->
//!   **`INVALID_ARGUMENT`**: a property of the request's own payload (a self-contradictory
//!   declared label), the identical reasoning `AlreadyStarted`/`EnvelopeNotAllowed` above
//!   already use for the same code -- never `PERMISSION_DENIED` (this is not about whether the
//!   verified identity may act; it already may, by the point this check runs) and never
//!   `UNAUTHENTICATED` (identity is already established).
//!
//! # R3.1: a service principal, verified like a human token (`docs/aiplane-plan.md` round 2's
//! declared gap; `docs/open-questions.md` question 206's open item)
//!
//! `Dispatch`, `Ack`, `Expire` and `Fail` each now authenticate a real OIDC **service**
//! subject through [`CommandAuthorityServiceImpl::authenticate_service_principal`] -- the
//! **same** [`oidc::verify`] path `Authorize`'s `principal_token` already takes: the same
//! [`IssuerConfig`] this service was constructed with, the same RS256-only allow-list, the
//! same boundary-nanosecond expiry against [`Self::clock`]. There is no second verifier
//! anywhere in this crate, and none of [`crate::oidc::TokenError`]'s sixteen typed refusals
//! was relaxed to make this possible.
//!
//! **How a service subject is distinguished from a human one**: structurally, not by any
//! claim the secsso contract declares (`authority.proto`'s own `Principal` doc comment
//! explains why that message has no human-vs-service field). A verified token is treated as a
//! service principal *exactly when* at least one of its groups grants -- via
//! [`crate::authz::ServiceRoleTable`], the profile's `authority.service_roles` block -- the
//! specific RPC being called. [`crate::authz::check_service_roles_disjoint`] (enforced at
//! [`crate::authz::load_profile_authz_config`] load time, before this service ever serves an
//! RPC) is what makes this distinction real rather than accidental: `service_roles` and the
//! human `authority.roles` table can never share a role name, so a group that grants a human
//! command class can never *also* grant a service RPC, and a token minted for a person (which
//! carries that group precisely because they hold a human authorization role) can therefore
//! never drive the asset through these four RPCs. A purely human token is refused the
//! identical [`crate::authz::ServiceAuthzError::ServiceRoleNotGranted`] an unrecognized or
//! under-scoped service role gets -- one refusal, not two, because "service principal" is
//! *defined* as "holds a granting service role," and a human token simply never does.
//!
//! **The `principal`-disagreement rule** (`Ack`/`Expire`/`Fail`, `AckRequest.principal`'s own
//! doc comment restated here at the enforcement site): the verified `service_token` subject is
//! always what lands on `CommandTransition.principal` -- never the request's own `principal`
//! field, which is a caller-*declared* label (e.g. the kernel binding's own identity string),
//! recorded only in `CommandTransition.reason` via [`format_service_reason`]. When that
//! declared label is non-empty and disagrees with the verified subject,
//! [`CommandAuthorityServiceImpl::check_declared_principal`] refuses the request
//! (`INVALID_ARGUMENT`) rather than silently preferring either value: this crate's rule
//! against a generic refusal applies here too (a typed [`ServiceError::PrincipalMismatch`],
//! not a warning-and-continue), and a discrepancy between what a caller claims and what it is
//! verified to be is exactly the kind of fact ADR-004's "everything rejected is counted" rule
//! exists to surface. An empty declared `principal` declares nothing and is always accepted.
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
//! **The `commands` index behind `Query`** (and every other RPC's `command_id` lookup) is
//! rebuilt from the ledger the identical way, by [`crate::ledger::Ledger::scan_commands`] --
//! see this module's own "In-memory index" section above for that fix (question 203(a)).
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
//! principal`. (A1.3's own reason constant claiming identity was unverified no longer exists
//! in this crate at all -- there is no path left that could say so when it is not true; A2.2,
//! below, replaced A2.1's own successor constant in turn with a reason that names the real
//! authorization decision, not merely that identity was checked.)
//!
//! **This was identity, not authorization -- A2.2 (below) is authorization.**
//!
//! # A2.2: the role/MFA/delegation gate, and the audit line for every outcome
//!
//! [`CommandAuthorityServiceImpl::authorize`] now runs [`crate::authz::authorize_command`]
//! immediately after `crate::oidc::verify` succeeds and before [`crate::state::authorize`] is
//! ever called: a verified `Principal` that may not authorize this `Command`'s class (no
//! granting role, and no valid `delegation_id`), or that has not satisfied the MFA gate for a
//! hazardous class, is refused `PERMISSION_DENIED` (see the status-code section above) with
//! **no state transition attempted and no ledger record appended** -- exactly mirroring how
//! A2.1 already treats an unverifiable token. The one difference from A2.1's own refusal
//! path: **a refused `Authorize` attempt still writes an [`crate::audit`] line** (severity
//! `Warning`, `MSGID`/`state` = `"REFUSED"`) before this method returns its `Status` -- "a
//! denied authorization attempt is exactly what a SIEM needs to see" (this task's own brief).
//! An unverifiable token (A2.1's own refusal) gets the identical treatment, for the identical
//! reason -- both refusal kinds are audited by the same two-line pattern in this method's
//! body (compute the message, write the audit event, return the `Status`), not two different
//! mechanisms that could drift apart.
//!
//! On success, [`crate::authz::format_authz_reason`] -- A2.1's own reason constant no longer
//! exists in this crate at all -- becomes the `CommandTransition.reason`: which role granted
//! it (or which delegation), and how MFA was satisfied (or that it was not required).
//! `docs/compliance/av-command/control-matrix.md`'s AC rows (3.1.x) and
//! AU rows (3.3.x) are updated to reflect this landing; see that document for exactly what
//! moved from Gap/Partial and what is still open.
//!
//! Every RPC that reaches a real state transition -- not only `Authorize` -- writes an
//! [`crate::audit`] line too: `CommandAuthorityServiceImpl::append_last_transition` (shared
//! by `Propose`, `Authorize`'s success path, `Dispatch`, `Ack`, `Expire` and `Fail`) and
//! `Check`'s own body (which does not go through that helper -- see its own doc comment)
//! each call [`crate::audit::AuditWriter::write`] once the ledger append itself has already
//! succeeded.
//!
//! # `Expire`/`Fail` -- A3.2 (D2), closing the kernel-refusal-visibility gap
//!
//! `docs/aiplane-plan.md` milestone A3's binding (`crates/av-run`, out of this crate's own
//! dependency graph -- see this module doc's own "`DispatchSink` -- not A3" section) drives
//! `crates/av-kernel`'s `ExternalCommandSource`. Every one of that trait's
//! `CommandOutcome`s that means "this command will never reach `ACKED`" must still land on
//! this crate's own ledger -- a `DISPATCHED` command that silently stays `DISPATCHED`
//! forever, because the kernel refused or expired it, is exactly the "a failure that leaves
//! no trace" defect shape round 1's own review found six times. Two RPCs give the A3 binding
//! a way to record that: `Expire` (`CommandOutcome::Expired` -> [`state::expire`],
//! `DISPATCHED -> EXPIRED`) and `Fail` (`CommandOutcome::DuplicateIdempotencyKey`/`Refused`/
//! `NotDispatchedRunEnded` -> [`state::fail`], `DISPATCHED -> FAILED`). Both are thin wire
//! adapters exactly like `Dispatch`/`Ack` above: no state-machine logic of their own,
//! [`Self::append_last_transition`] for the ledger append and audit line, and a
//! [`tonic::Status`] mapping through [`to_status`] identical to every other
//! [`state::CommandError`] this module already handles. **R3.1 update**: the principal
//! recorded on either edge is no longer a fixed, unauthenticated service-identity string --
//! see the module doc's "R3.1: a service principal, verified like a human token" section
//! above. Neither RPC is a human-authorization gate (that is still `Authorize`'s own job,
//! untouched by R3.1); both now require a verified **service** subject instead.
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
    CommandResponse, CommandState, DispatchRequest, ExpireRequest, FailRequest, ProposeRequest, QueryByEntity, QueryRequest,
    QueryResponse, VerifyLedgerRequest, VerifyLedgerResponse,
};

use crate::audit::{self, AuditWriter};
use crate::authority::{self, CheckCommandError};
use crate::authz::{self, AuthzError, DelegationTable, RoleTable, ServiceAuthzError, ServiceRoleTable, ServiceRpc};
use crate::clock::Clock;
use crate::counters::{Counted, Counters};
use crate::ledger::Ledger;
use crate::oidc::{self, IssuerConfig};
use crate::policy::PolicyBundle;
use crate::rate::LedgerRateSource;
use crate::state::{self, CommandError};

pub use crate::pb::command_authority_service_server::{CommandAuthorityService as CommandAuthorityServiceTrait, CommandAuthorityServiceServer};

/// The `CommandTransition.reason` text `Dispatch` writes -- naming the seam A3 fills, not a
/// real transport (see the module doc's "`DispatchSink` -- not A3" section). R3.1: the
/// *principal* `Dispatch` records is no longer a fixed constant (`DISPATCH_PRINCIPAL`, the
/// literal `"ground-segment"`, used to be this whole story) -- it is now the verified service
/// subject `Dispatch.service_token` names, so that constant (and `EXPIRE_PRINCIPAL`/
/// `FAIL_PRINCIPAL`, the identical shape for `Expire`/`Fail`) is deleted rather than kept as a
/// misleading leftover: none of the three means anything once identity is real. This reason
/// text still means something (it still names *why* -- the DispatchSink seam -- even though it
/// no longer needs to also carry *who*), so it stays, now composed with the granting service
/// role by [`format_service_reason`].
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

/// A2.2's construction-time configuration for [`CommandAuthorityServiceImpl::new`] -- see
/// that function's own doc for why this is a group rather than four more bare parameters.
/// R3.1 adds `service_role_table`/`counters` to this same group for the identical reason
/// (clippy's `too_many_arguments`, and this crate's rule against silencing it): `new` was
/// already at seven parameters before this round.
pub struct AuthzConfig {
    pub role_table: Arc<RoleTable>,
    pub delegations: Arc<DelegationTable>,
    pub mfa_amr_methods: Arc<Vec<String>>,
    pub mfa_acr: Arc<String>,
    pub audit: Arc<AuditWriter>,
    /// R3.1: the profile's declared service-role table (`authority.service_roles`) --
    /// `Dispatch`/`Ack`/`Expire`/`Fail` each check the verified `service_token`'s
    /// `Principal.groups` against this table for a role granting that specific RPC. Disjoint
    /// from `role_table` above by construction (checked at config load,
    /// `crate::authz::load_profile_authz_config`) -- see [`crate::authz::
    /// check_service_roles_disjoint`]'s own doc for why that matters.
    pub service_role_table: Arc<ServiceRoleTable>,
    /// R3.1: ADR-004's "everything rejected is counted" primitive (moved here from
    /// `crates/av-gateway/src/counters.rs`, `crate::counters`) -- every refusal `Authorize`/
    /// `Dispatch`/`Ack`/`Expire`/`Fail` can produce increments through this shared instance,
    /// which is also handed to `crate::evidence::AdminState` so `/admin/api/evidence` reports
    /// the identical counts as observable evidence, not just in-memory state invisible outside
    /// this process.
    pub counters: Arc<Counters>,
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
    /// A2.2: `Authorize`'s role/MFA/delegation gate refused. See [`crate::authz::AuthzError`]
    /// for the full refusal vocabulary and the module doc's status-code section for why this
    /// maps to `PERMISSION_DENIED`, distinct from [`Self::TokenInvalid`]'s `UNAUTHENTICATED`.
    #[error(transparent)]
    Authz(#[from] AuthzError),
    /// R3.1: a verified `service_token` names no service role granting the RPC being called
    /// (`Dispatch`/`Ack`/`Expire`/`Fail`) -- see [`crate::authz::ServiceAuthzError`] for the
    /// full refusal (today, exactly one shape) and the module doc's status-code section for
    /// why this maps to `PERMISSION_DENIED`, the identical reasoning as [`Self::Authz`]:
    /// identity is already established by this point, what is refused is whether that real,
    /// known identity may do this specific thing.
    #[error(transparent)]
    ServiceAuthz(#[from] ServiceAuthzError),
    /// R3.1: `Ack`/`Expire`/`Fail`'s caller-declared `principal` label disagrees with the
    /// verified `service_token` subject -- see the module doc's "R3.1: the `principal`
    /// disagreement rule" section for the full reasoning behind refusing rather than silently
    /// preferring one value. `INVALID_ARGUMENT`: this is a property of the request's own
    /// payload (a self-contradictory declared label), not of who is calling or whether they
    /// may -- the identical reasoning `Self::DuplicateIdempotencyKey`'s sibling refusals
    /// above (`AlreadyStarted`/`EnvelopeNotAllowed`) already use for the same code.
    #[error(
        "principal {declared:?} disagrees with the verified service subject {verified:?} -- a \
         caller-declared principal label must agree with the verified service_token subject or \
         be empty; it is never silently overridden"
    )]
    PrincipalMismatch { declared: String, verified: String },
}

/// R3.1: every [`ServiceError`] this module can produce now counts through
/// [`crate::counters::Counters`] -- ADR-004's "everything rejected is counted" rule, applied
/// uniformly rather than per call site. Delegates to the wrapped typed error's own
/// [`Counted::code`] wherever one exists (so there is exactly one place each nested enum's
/// code is spelled), and supplies this type's own code directly for its own, non-delegating
/// variants.
impl Counted for ServiceError {
    fn code(&self) -> &'static str {
        match self {
            ServiceError::NotFound(_) => "not_found",
            ServiceError::State(e) => e.code(),
            ServiceError::Check(CheckCommandError::State(e)) => e.code(),
            ServiceError::Check(CheckCommandError::Io(_)) => "check_io_error",
            ServiceError::DuplicateIdempotencyKey(_) => "duplicate_idempotency_key",
            ServiceError::Io(_) => "io_error",
            ServiceError::TokenInvalid(e) => e.code(),
            ServiceError::Authz(e) => e.code(),
            ServiceError::ServiceAuthz(e) => e.code(),
            ServiceError::PrincipalMismatch { .. } => "principal_mismatch",
        }
    }
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
                // A3.2/D2: the `ACKED -> ACKED` edge itself exists (this is not an
                // IllegalTransition); what is refused is the specific non-increasing
                // ack_level value being re-asserted against a `Command` already in the
                // required state -- FAILED_PRECONDITION for the identical reason
                // IllegalTransition is: "the system is not in a state required for the
                // operation's execution" (this ack_level can never legally apply now).
                CommandError::AckLevelNotIncreasing { .. } => Code::FailedPrecondition,
            };
            Status::new(code, err.to_string())
        }
        ServiceError::Check(CheckCommandError::State(ref state_err)) => {
            let code = match state_err {
                CommandError::IllegalTransition { .. } => Code::FailedPrecondition,
                CommandError::AlreadyStarted { .. } => Code::InvalidArgument,
                CommandError::EnvelopeNotAllowed { .. } => Code::InvalidArgument,
                // A3.2/D2: the `ACKED -> ACKED` edge itself exists (this is not an
                // IllegalTransition); what is refused is the specific non-increasing
                // ack_level value being re-asserted against a `Command` already in the
                // required state -- FAILED_PRECONDITION for the identical reason
                // IllegalTransition is: "the system is not in a state required for the
                // operation's execution" (this ack_level can never legally apply now).
                CommandError::AckLevelNotIncreasing { .. } => Code::FailedPrecondition,
            };
            Status::new(code, err.to_string())
        }
        ServiceError::Check(CheckCommandError::Io(_)) => Status::new(Code::Internal, err.to_string()),
        ServiceError::DuplicateIdempotencyKey(_) => Status::new(Code::AlreadyExists, err.to_string()),
        ServiceError::Io(_) => Status::new(Code::Internal, err.to_string()),
        ServiceError::TokenInvalid(_) => Status::new(Code::Unauthenticated, err.to_string()),
        ServiceError::Authz(_) => Status::new(Code::PermissionDenied, err.to_string()),
        ServiceError::ServiceAuthz(_) => Status::new(Code::PermissionDenied, err.to_string()),
        ServiceError::PrincipalMismatch { .. } => Status::new(Code::InvalidArgument, err.to_string()),
    }
}

/// The `CommandAuthorityService` implementation. Owns a [`Ledger`], a [`PolicyBundle`], the
/// injected [`Clock`], a [`DispatchSink`] and two pieces of in-process state, both now
/// durability-backed and both rebuilt from the ledger at construction ([`Self::new`]), before
/// this process serves a single RPC of its own:
///
/// - `dispatched_idempotency_keys` is **rebuilt from the ledger** at every construction
///   ([`crate::ledger::Ledger::scan_dispatched_idempotency_keys`]) -- the duplicate-dispatch
///   guarantee this set backs (`command.proto`'s own doc comment on `Command.
///   idempotency_key`: "the edge never dispatches the same key twice", stated with no
///   process-lifetime qualifier) therefore survives a process restart: a second
///   `CommandAuthorityServiceImpl` constructed over the *same* ledger directory refuses the
///   same key `Dispatch::dispatch` would have refused in the first process, before this
///   process has ever handled a single RPC of its own.
/// - `commands` (the in-memory `Command` index the module doc describes) is likewise
///   **rebuilt from the ledger** at every construction ([`crate::ledger::Ledger::
///   scan_commands`]) -- question 203(a)'s fix, this round, for the previous round's own
///   declared gap ("`Query` does not survive a restart"). `LedgerRecord.command`
///   (`authority.proto`, A1.3-round-2) is what makes the rebuild possible: every record now
///   carries the full `Command` as it stood at the time of that transition, not only the four
///   scalar fields (`partition`/`command_id`/`command_class`/`idempotency_key`) the ledger
///   already carried -- so `Check`/`Authorize`/`Dispatch`/`Ack`/`Query` against a `command_id`
///   a *previous* process lifetime `Propose`d now succeed after a restart exactly as they
///   would have without one. See `docs/compliance/av-command/control-matrix.md`'s Deficiency 7
///   for the compliance-facing record of this fix.
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
    /// A2.2: the profile's declared role table -- see [`crate::authz::RoleTable`].
    role_table: Arc<RoleTable>,
    /// A2.2: the profile's declared delegations -- see [`crate::authz::DelegationTable`].
    delegations: Arc<DelegationTable>,
    /// A2.2: `amr` values that satisfy the MFA gate for a hazardous command class.
    mfa_amr_methods: Arc<Vec<String>>,
    /// A2.2: the `acr` value that satisfies the MFA gate, or empty for "not configured".
    mfa_acr: Arc<String>,
    /// A2.2: every transition, and every `Authorize` refusal, as one RFC 5424 line -- see
    /// [`crate::audit`].
    audit: Arc<AuditWriter>,
    /// R3.1: the profile's declared service-role table -- see [`crate::authz::
    /// ServiceRoleTable`] and [`AuthzConfig::service_role_table`].
    service_role_table: Arc<ServiceRoleTable>,
    /// R3.1: ADR-004's "everything rejected is counted" primitive -- see [`AuthzConfig::
    /// counters`].
    counters: Arc<Counters>,
    commands: Mutex<BTreeMap<String, Command>>,
    dispatched_idempotency_keys: Mutex<BTreeSet<String>>,
}

impl CommandAuthorityServiceImpl {
    /// Fails only if [`Ledger::scan_dispatched_idempotency_keys`] or [`Ledger::scan_commands`]
    /// fails (a real I/O error reading the ledger directory this process is about to serve
    /// from) -- never silently starts with an empty duplicate-dispatch guard or an empty
    /// `commands` index when the ledger it was asked to rebuild either from could not actually
    /// be read.
    ///
    /// `authz` groups every A2.2 field (role table, delegations, MFA config, the
    /// [`AuditWriter`]) -- exactly the way `crate::ledger::CommandMeta` groups `Ledger::
    /// append`'s own arguments -- so this constructor gains A2.2's fields without tripping
    /// clippy's `too_many_arguments` lint; this crate's rule against a lint-suppressing attribute on
    /// hand-written items means the fix is grouping the arguments, never silencing the lint.
    pub fn new(
        ledger: Arc<Ledger>,
        bundle: Arc<PolicyBundle>,
        rate_window_ns: i64,
        clock: Arc<dyn Clock>,
        dispatch_sink: Arc<dyn DispatchSink>,
        issuer_config: Arc<IssuerConfig>,
        authz: AuthzConfig,
    ) -> io::Result<Self> {
        let dispatched_idempotency_keys = ledger.scan_dispatched_idempotency_keys()?;
        // Question 203(a): rebuild the `commands` index from the ledger too, before the first
        // RPC is served -- the same construction-time discipline as the idempotency guard
        // above, now made possible by `LedgerRecord.command` (`authority.proto`, A1.3-round-2).
        let commands = ledger.scan_commands()?;
        let AuthzConfig { role_table, delegations, mfa_amr_methods, mfa_acr, audit, service_role_table, counters } = authz;
        Ok(Self {
            ledger,
            bundle,
            rate_window_ns,
            clock,
            dispatch_sink,
            issuer_config,
            role_table,
            delegations,
            mfa_amr_methods,
            mfa_acr,
            audit,
            service_role_table,
            counters,
            commands: Mutex::new(commands),
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
    /// function's own `Ledger::append` call), then writes the matching [`crate::audit`] line
    /// (A2.2: "every transition -- not only `Authorize` -- is written as one ... audit
    /// line"). The audit write happens only after the ledger append has already succeeded --
    /// an audit line is never written for a transition this crate cannot also prove it
    /// retained.
    fn append_last_transition(&self, command: &Command) -> Result<(), ServiceError> {
        let transition = command
            .transitions
            .last()
            .expect("every state:: edge function appends exactly one transition before returning Ok")
            .clone();
        self.ledger
            .append(
                crate::ledger::CommandMeta::new(&command.entity_id, &command.id, &command.command_class, &command.idempotency_key),
                transition.clone(),
                None,
                Some(command),
                &*self.clock,
            )
            .map_err(ServiceError::Io)?;
        self.audit.write(&audit::event_for_transition(command, &transition, None)).map_err(ServiceError::Io)?;
        Ok(())
    }

    /// Writes a `Warning`-severity, `REFUSED` [`crate::audit`] event for a denied `Authorize`
    /// attempt -- shared by both of `Self::authorize`'s refusal paths (an unverifiable token,
    /// A2.1; a role/MFA/delegation refusal, A2.2), so the two refusal kinds are audited by
    /// one code path, not two that could drift apart. `principal` is empty for a token
    /// refusal (identity was never established) and the verified `sub` for an A2.2 refusal.
    fn audit_authorize_refusal(&self, command: &Command, principal: &str, delegation_id: &str, message: &str) -> Result<(), ServiceError> {
        let now_tai_ns = self.clock.now_tai_ns();
        let event = audit::event_for_refusal(now_tai_ns, &command.id, &command.entity_id, &command.command_class, principal, delegation_id, message);
        self.audit.write(&event).map_err(ServiceError::Io)
    }

    /// R3.1: increments [`Self::counters`] for `err`'s own [`Counted::code`] and maps it to a
    /// [`tonic::Status`] via [`to_status`] -- the one place every RPC below turns a
    /// [`ServiceError`] into a wire response, so a refusal can never reach a caller without
    /// also leaving a trace in the counters `/admin/api/evidence` reports (ADR-004:
    /// "everything rejected is counted"). Takes `&self` (not a bare function) precisely so it
    /// can reach `self.counters` without every call site threading it through separately.
    fn to_status_counted(&self, err: ServiceError) -> Status {
        self.counters.record(&err);
        to_status(err)
    }

    /// R3.1: `Dispatch`/`Ack`/`Expire`/`Fail`'s shared service-principal gate -- see the
    /// module doc's "R3.1: a service principal, verified like a human token" section for the
    /// full contract. Verifies `service_token` through the **identical** [`oidc::verify`]
    /// path `Authorize`'s own `principal_token` takes (same [`IssuerConfig`], same clock
    /// reading, same RS256-only rule -- there is no second verifier anywhere in this crate),
    /// then checks the verified [`av_cdm::pb::Principal::groups`] against
    /// [`Self::service_role_table`] for a role granting `rpc`. Returns the verified principal
    /// together with the granting role's own name (folded into the transition's reason by
    /// [`format_service_reason`]) -- never partially: a token that verifies but grants no
    /// service role for this RPC returns [`ServiceError::ServiceAuthz`], not `Ok`.
    fn authenticate_service_principal(&self, service_token: &str, rpc: ServiceRpc, now_tai_ns: i64) -> Result<(av_cdm::pb::Principal, String), ServiceError> {
        let principal = oidc::verify(service_token, &self.issuer_config, now_tai_ns).map_err(ServiceError::TokenInvalid)?;
        let role = authz::authorize_service_call(&principal, rpc, &self.service_role_table).map_err(ServiceError::ServiceAuthz)?;
        Ok((principal, role))
    }

    /// R3.1: the `principal`-disagreement rule shared by `Ack`/`Expire`/`Fail` -- see
    /// `proto/altavista/v1/authority.proto`'s `AckRequest.principal` doc comment for the full
    /// contract this enforces. `declared` is the request's own caller-supplied `principal`
    /// field (a label, never trusted as identity on its own); `verified` is the
    /// [`authenticate_service_principal`]-verified subject, always authoritative. An empty
    /// `declared` value declares nothing and is never a disagreement; a non-empty value that
    /// differs from `verified` is refused [`ServiceError::PrincipalMismatch`] rather than one
    /// of the two being silently preferred.
    fn check_declared_principal(&self, declared: &str, verified: &str) -> Result<(), ServiceError> {
        if declared.is_empty() || declared == verified {
            Ok(())
        } else {
            Err(ServiceError::PrincipalMismatch { declared: declared.to_string(), verified: verified.to_string() })
        }
    }

    /// R3.1 (manager's review): the one refusal path for `Dispatch`/`Ack`/`Expire`/`Fail`'s
    /// **identity and authorization** refusals -- an unverifiable or missing `service_token`, a
    /// verified token granting no service role for this RPC, and a caller-declared `principal`
    /// that disagrees with the verified subject. It does what
    /// [`Self::audit_authorize_refusal`] already does for the human `Authorize` path: writes
    /// one RFC 5424 audit line (question 54's SIEM export) *as well as* counting the refusal,
    /// because a counter lives only in this process's memory and behind
    /// `/admin/api/evidence` -- a security refusal whose only trace is a counter is invisible
    /// to the sink a SIEM actually reads, and the asymmetry ("a human's refused authorization
    /// is exported, a service's refused dispatch is not") is not one any reader of ADR-004
    /// would predict.
    ///
    /// `entity` and `class` are deliberately empty: this refusal is decided **before** the
    /// command is loaded (see each RPC's own body for why that order is the secure one), so
    /// the service genuinely does not know them at this point, and
    /// [`crate::audit::AuditWriter::write`] omits an empty structured-data parameter rather
    /// than emitting a placeholder. `command_id` comes from the request, which is the one
    /// identifier the caller did supply.
    ///
    /// An audit-sink write failure is reported as `Internal` exactly as
    /// [`CommandAuthorityServiceImpl::authorize`] already reports it -- but the original
    /// refusal is counted **first**, so a failing sink can never erase the refusal's trace in
    /// the counters as well.
    fn refuse_service_call(&self, command_id: &str, principal: &str, err: ServiceError) -> Status {
        let message = err.to_string();
        let event = audit::event_for_refusal(self.clock.now_tai_ns(), command_id, "", "", principal, "", &message);
        match self.audit.write(&event) {
            Ok(()) => self.to_status_counted(err),
            Err(io) => {
                self.counters.record(&err);
                self.to_status_counted(ServiceError::Io(io))
            }
        }
    }
}

/// R3.1: composes the base reason text a `Dispatch`/`Ack`/`Expire`/`Fail` transition carries
/// with the service role that granted the call and (when the caller declared one, and it
/// agreed with the verified subject -- [`CommandAuthorityServiceImpl::check_declared_
/// principal`] already refused a disagreeing one before this is ever called) the caller's own
/// declared label -- so the ledger and audit line show not merely *that* a service principal
/// acted, but *which role* granted it, mirroring [`authz::format_authz_reason`]'s identical
/// "name which role granted it" convention for the human `Authorize` path.
fn format_service_reason(base_reason: &str, role: &str, declared_label: &str) -> String {
    if declared_label.is_empty() {
        format!("{base_reason} (service_role={role:?})")
    } else {
        format!("{base_reason} (service_role={role:?} declared_principal={declared_label:?})")
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

    /// Does not go through [`Self::append_last_transition`] (`crate::authority::check_command`
    /// already appends its own ledger record, with the `PolicyDecision` attached) -- so this
    /// method writes its own [`crate::audit`] line directly, once `check_command` has
    /// already succeeded, carrying that same `PolicyDecision` (`decisionId`/`policyHash` in
    /// the structured data -- see `crate::audit`'s module doc).
    async fn check(&self, request: Request<CheckRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let command = self.get_command(&req.command_id).map_err(to_status)?;
        let rate_source = LedgerRateSource::new(&self.ledger);
        let result = authority::check_command(command, &self.bundle, self.rate_window_ns, &rate_source, &self.ledger, &*self.clock)
            .map_err(|e| to_status(e.into()))?;
        let transition = result.command.transitions.last().expect("check_command always appends exactly one transition").clone();
        self.audit
            .write(&audit::event_for_transition(&result.command, &transition, Some(&result.decision)))
            .map_err(|e| to_status(ServiceError::Io(e)))?;
        self.put_command(result.command.clone());
        Ok(Response::new(CommandResponse { command: Some(result.command), decision: Some(result.decision) }))
    }

    /// A2.1 (identity) then A2.2 (authorization), in that order -- see the module doc's "A2.2:
    /// the role/MFA/delegation gate, and the audit line for every outcome" section for the
    /// full contract. Neither refusal kind ever reaches [`crate::state::authorize`], appends a
    /// ledger record, or touches this service's in-memory index; both write an
    /// [`crate::audit`] line before this method returns its `Status`.
    async fn authorize(&self, request: Request<AuthorizeRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let command = self.get_command(&req.command_id).map_err(|e| self.to_status_counted(e))?;

        // A2.1: verify identity before attempting any state transition.
        let now_tai_ns = self.clock.now_tai_ns();
        let principal = match oidc::verify(&req.principal_token, &self.issuer_config, now_tai_ns) {
            Ok(p) => p,
            Err(e) => {
                let message = e.to_string();
                self.audit_authorize_refusal(&command, "", &req.delegation_id, &message).map_err(|e| self.to_status_counted(e))?;
                return Err(self.to_status_counted(ServiceError::TokenInvalid(e)));
            }
        };

        // A2.2: role/MFA/delegation gate, now that identity is real.
        let ctx = authz::AuthzContext {
            role_table: &self.role_table,
            delegations: &self.delegations,
            mfa_amr_methods: &self.mfa_amr_methods,
            mfa_acr: &self.mfa_acr,
        };
        let decision = match authz::authorize_command(&principal, &command, &req.delegation_id, ctx, now_tai_ns) {
            Ok(d) => d,
            Err(e) => {
                let message = e.to_string();
                self.audit_authorize_refusal(&command, &principal.sub, &req.delegation_id, &message).map_err(|e| self.to_status_counted(e))?;
                return Err(self.to_status_counted(ServiceError::Authz(e)));
            }
        };

        let reason = authz::format_authz_reason(&command.command_class, &decision);
        let authorized = state::authorize(command, &principal.sub, &reason, &req.delegation_id, &*self.clock).map_err(|e| self.to_status_counted(e.into()))?;
        self.append_last_transition(&authorized).map_err(|e| self.to_status_counted(e))?;
        self.put_command(authorized.clone());
        Ok(Response::new(CommandResponse { command: Some(authorized), decision: None }))
    }

    /// R3.1: a real OIDC **service** subject, verified through [`Self::
    /// authenticate_service_principal`] -- the identical path `Authorize` takes -- gated on a
    /// service role granting `"dispatch"`. See the module doc's "R3.1: a service principal,
    /// verified like a human token" section for the full contract; every refusal below counts
    /// through [`Self::to_status_counted`].
    ///
    /// **Authentication happens before the command is loaded**, here and in `Ack`/`Expire`/
    /// `Fail` (manager's review, R3.1): with the lookup first, an unauthenticated caller could
    /// tell `NOT_FOUND` from `UNAUTHENTICATED` and so enumerate which command ids exist on this
    /// service -- an existence oracle available to a caller with no credential at all. The
    /// order below means an unauthenticated caller learns exactly one thing, that it is not
    /// authenticated.
    async fn dispatch(&self, request: Request<DispatchRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let now_tai_ns = self.clock.now_tai_ns();
        let (principal, role) = self
            .authenticate_service_principal(&req.service_token, ServiceRpc::Dispatch, now_tai_ns)
            .map_err(|e| self.refuse_service_call(&req.command_id, "", e))?;
        let command = self.get_command(&req.command_id).map_err(|e| self.to_status_counted(e))?;

        // Held across the state edge and the ledger append (both synchronous, no `.await`
        // point in between) so a racing second Dispatch for the same key either sees it
        // already present or blocks until this one finishes recording it -- see the module
        // doc's "Idempotency" section.
        let mut seen = self.dispatched_idempotency_keys.lock().unwrap_or_else(|p| p.into_inner());
        if !command.idempotency_key.is_empty() && seen.contains(&command.idempotency_key) {
            return Err(self.to_status_counted(ServiceError::DuplicateIdempotencyKey(command.idempotency_key.clone())));
        }

        let reason = format_service_reason(DISPATCH_REASON, &role, "");
        let dispatched = state::dispatch(command, &principal.sub, &reason, &*self.clock).map_err(|e| self.to_status_counted(e.into()))?;
        self.append_last_transition(&dispatched).map_err(|e| self.to_status_counted(e))?;
        if !dispatched.idempotency_key.is_empty() {
            seen.insert(dispatched.idempotency_key.clone());
        }
        drop(seen);

        self.dispatch_sink.dispatch(&dispatched);
        self.put_command(dispatched.clone());
        Ok(Response::new(CommandResponse { command: Some(dispatched), decision: None }))
    }

    /// R3.1: a real OIDC service subject gated on a role granting `"ack"` -- see
    /// [`Self::dispatch`]'s own doc comment for the shared contract. `req.principal` is now a
    /// caller-declared label (never trusted as identity); [`Self::check_declared_principal`]
    /// refuses one that disagrees with the verified subject before any state transition is
    /// attempted.
    async fn ack(&self, request: Request<AckRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let now_tai_ns = self.clock.now_tai_ns();
        let (principal, role) = self
            .authenticate_service_principal(&req.service_token, ServiceRpc::Ack, now_tai_ns)
            .map_err(|e| self.refuse_service_call(&req.command_id, "", e))?;
        self.check_declared_principal(&req.principal, &principal.sub).map_err(|e| self.refuse_service_call(&req.command_id, &principal.sub, e))?;
        let command = self.get_command(&req.command_id).map_err(|e| self.to_status_counted(e))?;

        let reason = format_service_reason(&req.reason, &role, &req.principal);
        let ack_level = AckLevel::try_from(req.ack_level).unwrap_or(AckLevel::Unspecified);
        let acked = state::ack(command, &principal.sub, &reason, ack_level, &*self.clock).map_err(|e| self.to_status_counted(e.into()))?;
        self.append_last_transition(&acked).map_err(|e| self.to_status_counted(e))?;
        self.put_command(acked.clone());
        Ok(Response::new(CommandResponse { command: Some(acked), decision: None }))
    }

    /// A3.2/D2: `DISPATCHED -> EXPIRED` (or `AUTHORIZED -> EXPIRED`, `state::expire`'s other
    /// legal source state) -- see the module doc's "`Expire`/`Fail`" section. R3.1: a real
    /// OIDC service subject gated on a role granting `"expire"`, and `req.principal` is now
    /// additive with the identical caller-declared-label contract [`Self::ack`] enforces --
    /// see [`Self::dispatch`]'s own doc comment for the shared verification contract.
    async fn expire(&self, request: Request<ExpireRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let now_tai_ns = self.clock.now_tai_ns();
        let (principal, role) = self
            .authenticate_service_principal(&req.service_token, ServiceRpc::Expire, now_tai_ns)
            .map_err(|e| self.refuse_service_call(&req.command_id, "", e))?;
        self.check_declared_principal(&req.principal, &principal.sub).map_err(|e| self.refuse_service_call(&req.command_id, &principal.sub, e))?;
        let command = self.get_command(&req.command_id).map_err(|e| self.to_status_counted(e))?;

        let reason = format_service_reason(&req.reason, &role, &req.principal);
        let expired = state::expire(command, &principal.sub, &reason, &*self.clock).map_err(|e| self.to_status_counted(e.into()))?;
        self.append_last_transition(&expired).map_err(|e| self.to_status_counted(e))?;
        self.put_command(expired.clone());
        Ok(Response::new(CommandResponse { command: Some(expired), decision: None }))
    }

    /// A3.2/D2: `DISPATCHED -> FAILED` -- see the module doc's "`Expire`/`Fail`" section.
    /// R3.1: identical service-principal and `principal`-disagreement contract as
    /// [`Self::expire`], gated on a role granting `"fail"`.
    async fn fail(&self, request: Request<FailRequest>) -> Result<Response<CommandResponse>, Status> {
        let req = request.into_inner();
        let now_tai_ns = self.clock.now_tai_ns();
        let (principal, role) = self
            .authenticate_service_principal(&req.service_token, ServiceRpc::Fail, now_tai_ns)
            .map_err(|e| self.refuse_service_call(&req.command_id, "", e))?;
        self.check_declared_principal(&req.principal, &principal.sub).map_err(|e| self.refuse_service_call(&req.command_id, &principal.sub, e))?;
        let command = self.get_command(&req.command_id).map_err(|e| self.to_status_counted(e))?;

        let reason = format_service_reason(&req.reason, &role, &req.principal);
        let failed = state::fail(command, &principal.sub, &reason, &*self.clock).map_err(|e| self.to_status_counted(e.into()))?;
        self.append_last_transition(&failed).map_err(|e| self.to_status_counted(e))?;
        self.put_command(failed.clone());
        Ok(Response::new(CommandResponse { command: Some(failed), decision: None }))
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
