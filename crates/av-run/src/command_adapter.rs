//! A3.2 (`docs/aiplane-plan.md`'s A3 milestone, D1): the one seam that wires the command-
//! authority service (`av_command::service::DispatchSink`) to the kernel's external
//! telecommand source (`av_kernel::drm::command_source::ExternalCommandSource`).
//!
//! `crates/av-command` must stay free of an `av-kernel` dependency (`av-kernel` pulls in GMAT
//! through `gmat-sys`, and `cargo test -p av-command` must not need GMAT -- D1's own ratified
//! reasoning). `crates/av-kernel` in turn knows nothing of gRPC, ledgers or policy. Neither
//! crate can implement this adapter itself; `av-run` already depends on both, so
//! [`KernelCommandAdapter`] lives here, and only here.
//!
//! # The two trait implementations, and why one queue is enough
//!
//! [`KernelCommandAdapter`] implements BOTH traits over one shared, `Mutex`-guarded
//! `VecDeque<Command>`:
//!
//! - [`av_command::service::DispatchSink::dispatch`] is called by
//!   `CommandAuthorityServiceImpl::dispatch`'s own gRPC handler (an async context, on
//!   whatever tokio worker thread happened to service that RPC) the instant a `Command`
//!   turns `AUTHORIZED -> DISPATCHED` on the ledger. It pushes a clone of that **exact**
//!   `Command` -- the one the gRPC service actually authorized, never a copy the caller
//!   reassembled -- onto the queue. `av_cdm::pb::Command` is the literal same generated Rust
//!   type on both sides of this file (`av-command`'s own `build.rs` `extern_path`s every
//!   `altavista.v1` message onto `av_cdm::pb`; `av-kernel`'s `command_source` module already
//!   takes `av_cdm::pb::Command` too) -- there is no conversion step here to accidentally
//!   diverge from what was actually authorized.
//! - [`av_kernel::drm::command_source::ExternalCommandSource::poll`] is called by
//!   `av_kernel::drm::executor::run_shared_group`, exactly once per run, at the run's own
//!   scenario-start epoch (that trait's own module doc, "Why one `poll`, not many") -- a
//!   synchronous call, on whatever thread is driving the kernel run. It drains the queue in
//!   one shot and hands the kernel every `Command` this service has dispatched so far, in
//!   dispatch order.
//!
//! # `report`: bridging a synchronous trait method back to a real async gRPC call
//!
//! `ExternalCommandSource::report(&self, outcome: CommandOutcome)` is deliberately
//! synchronous (`av-kernel`'s own batch-simulation executor calls it inline, never
//! `.await`s anything) -- but reporting an outcome back to the real service means a real
//! `Ack`/`Expire`/`Fail` RPC, which is unavoidably async (`tonic`'s generated client). The
//! bridge is `tokio::runtime::Handle::block_on`, which panics if called from a thread that
//! is **currently being polled** as part of that same runtime (a nested `block_on` inside an
//! already-running async task) -- but is exactly the supported "synchronous caller drives an
//! async runtime" pattern otherwise. `av_kernel::drm::execute` (which calls `poll`/`report`
//! synchronously, deep in its own call stack) must therefore itself be called from a plain,
//! non-`async` context that is not itself inside an active `block_on` frame -- **not** from
//! inside a `#[tokio::test] async fn` body directly (that body IS a task the runtime is
//! polling, on one of its own worker threads, even for its purely-synchronous statements),
//! and not from a nested `block_on` call. This crate's own `tests/command_dispatch_e2e.rs`
//! follows the pattern that satisfies this: every test there is a plain, synchronous
//! `#[test]` that builds its own `tokio::runtime::Runtime` and calls `rt.block_on(...)` only
//! for its own gRPC setup calls (`Propose`/`Check`/`Authorize`/`Dispatch`/`Query`), each call
//! returning before the next one starts; `execute()` itself is then called directly, with no
//! `block_on` wrapper at all, from that same plain OS thread -- a thread the runtime never
//! treats as one of its own workers, so `report`'s own (later, separate) `block_on` calls,
//! made from deep inside that `execute()` call, are fresh top-level calls, never nested
//! inside a still-running one. [`KernelCommandAdapter::new`]'s own doc comment repeats this
//! constraint at the construction site, where a caller actually commits to which thread the
//! kernel run (and therefore `report`) will execute on.
//!
//! # D2's state mapping, implemented here
//!
//! - [`CommandOutcome::Dispatched`]: no RPC. The `Dispatch` RPC itself already recorded
//!   `AUTHORIZED -> DISPATCHED` on the ledger, before this adapter's queue ever handed the
//!   `Command` to the kernel at all -- `DISPATCHED` therefore means "accepted into the
//!   asset's dispatch path," not "the frame reached the wire." `ACK_LEVEL_EDGE` (below) is
//!   the confirmation of the latter, at the real kernel epoch -- **the two are not the same
//!   epoch**, and a reader must not assume the ledger's `DISPATCHED` transition's own
//!   `tai_ns` is the wire epoch: it is the service's clock reading at `Dispatch`-RPC time,
//!   which in every test here (and in general) predates the kernel run itself entirely.
//! - [`CommandOutcome::Acked`]: one `Ack` RPC per level, `DISPATCHED -> ACKED` for the first
//!   one and `ACKED -> ACKED` (A3.2/D2's own tenth edge) for every one after, each carrying
//!   the kernel's own reported epoch and level in `reason`.
//! - [`CommandOutcome::Expired`]: one `Expire` RPC, `DISPATCHED -> EXPIRED`, `reason` naming
//!   both `deadline_tai_ns` and the kernel epoch the expiry was evaluated at.
//! - [`CommandOutcome::DuplicateIdempotencyKey`], [`CommandOutcome::Refused`] and
//!   [`CommandOutcome::NotDispatchedRunEnded`]: one `Fail` RPC each, `DISPATCHED -> FAILED`,
//!   `reason` naming the kernel's own typed refusal -- **every one of these lands on the
//!   ledger**; a kernel refusal that leaves a `Command` showing `DISPATCHED` forever is
//!   exactly the defect shape round 1's own review found six times (D2's own explicit
//!   instruction).
//!
//! An RPC that itself fails (the service unreachable, or refuses for some other reason) is
//! logged to stderr, never silently swallowed and never panics this synchronous trait method
//! -- a kernel run must not crash because one outcome report failed to land; the caller is
//! expected to check the ledger against the kernel's own recorded outcomes afterward (as
//! every test in `tests/` here does) to catch that case.
//!
//! # R3.1: one `service_token`, presented for the whole run -- a declared gap, not a solved
//! problem
//!
//! [`KernelCommandAdapter::new`] takes exactly one `service_token` (a compact-serialization
//! JWS), presented verbatim on every `Ack`/`Expire`/`Fail` this adapter makes for as long as
//! this instance exists. A real OIDC access token has a bounded lifetime (`exp`), and a batch
//! `av_kernel::drm::execute` run can, in general, outlive it: this adapter has **no token
//! refresh of any kind** -- it never re-mints, never re-fetches, and never rotates the token
//! it was constructed with. This is a **declared gap**, not an oversight papered over:
//!
//! - **What actually happens today**: once the token's `exp` passes (relative to the real
//!   service's own clock, which for a live deployment is [`av_command::clock::SystemClock`],
//!   wall time), every subsequent `Ack`/`Expire`/`Fail` this adapter attempts is refused
//!   `UNAUTHENTICATED` (`crates/av-command/src/oidc.rs::TokenError::Expired`) -- caught by the
//!   same "log to stderr, never panic" path every other RPC failure already takes (this
//!   module doc's own section above), so a run does not crash, but every outcome report after
//!   that point silently fails to land on the ledger. A `DISPATCHED` command whose `Ack`/
//!   `Expire`/`Fail` report was lost this way is exactly the "a failure that leaves no trace"
//!   defect shape this whole track's reviews have repeatedly found -- naming it here rather
//!   than leaving it undiscovered.
//! - **Why this was not fixed here**: this task's own brief is explicit -- "do not invent
//!   token refresh." A real refresh needs a refresh-token grant (or a re-issued token from
//!   whatever mints this one), a retry-with-fresh-token policy for the RPC that discovered the
//!   expiry, and a decision about what "the token expired mid-flight, mid-poll-batch" means
//!   for [`ExternalCommandSource::report`]'s own synchronous, `block_on`-bridged contract --
//!   none of which this task built or tested, and a half-built refresh path would be exactly
//!   the "untested extra surface" this crate's other modules (`crates/av-command/src/audit.rs`
//!   on a UDP sink, `crates/av-command/src/oidc.rs` on ES256/ES384) already decline to add for
//!   the identical reason.
//! - **The shape a real solution would take**, named so a later task does not have to
//!   rediscover it: (1) a token *source* trait (`fn current_token(&self) -> String`, or
//!   similar) in place of this field's plain `String`, letting a caller hand this adapter
//!   something that re-reads a file a sidecar keeps refreshed, or that itself refreshes via a
//!   client-credentials grant on a timer; (2) [`Self::report`]'s three RPC branches would call
//!   that source immediately before each `block_on`, rather than reading `self.service_token`
//!   once, so a mid-run rotation is picked up on the very next report; (3) a decision, tested,
//!   for what a *mid-flight* `Ack`/`Expire`/`Fail` refusal on `UNAUTHENTICATED` should do --
//!   retry once against a freshly-read token, or accept the lost report and rely on the
//!   ledger-vs-kernel-event reconciliation this module doc's own "An RPC that itself fails"
//!   paragraph already asks callers to do. None of this is built here.

use std::collections::VecDeque;
use std::sync::Mutex;

use av_cdm::pb::{AckRequest, Command, ExpireRequest, FailRequest};
use av_command::pb::command_authority_service_client::CommandAuthorityServiceClient;
use av_command::service::DispatchSink;
use av_kernel::drm::command_source::{CommandOutcome, ExternalCommandSource};
use tonic::transport::Channel;

/// The caller-declared `principal` label recorded on every `Ack`/`Expire`/`Fail` this adapter
/// reports back (R3.1: `AckRequest.principal`'s own doc comment) -- **not** an identity claim
/// on its own (`CommandTransition.principal` is now always the verified `service_token`
/// subject, `crates/av-command/src/service.rs`'s module doc, "R3.1: a service principal,
/// verified like a human token") but a label that must AGREE with it or every one of this
/// adapter's own `Ack`/`Expire`/`Fail` calls is refused `INVALID_ARGUMENT`
/// (`ServiceError::PrincipalMismatch`). This is a deliberate fail-closed choice, not an
/// oversight: whoever provisions [`KernelCommandAdapter::new`]'s `service_token` must mint (or
/// request) one whose verified `sub` is exactly this literal, `"kernel"` -- a misconfigured
/// token (any other `sub`) then fails loudly, immediately, on the very first `Ack`/`Expire`/
/// `Fail` call, rather than silently recording whatever the token happened to assert. The
/// alternative -- declaring no `principal` at all (an empty string, which the disagreement
/// rule accepts unconditionally) -- would remove that early, loud check in exchange for saving
/// this one required-token-shape constraint; this module keeps the check.
pub const KERNEL_PRINCIPAL: &str = "kernel";

/// The seam between [`DispatchSink`] and [`ExternalCommandSource`] -- see the module doc
/// comment for the full contract. One instance is shared (behind an `Arc`, ordinarily) by
/// both a `CommandAuthorityServiceImpl` (as a `DispatchSink`) and one kernel run (as an
/// `ExternalCommandSource`) -- constructing two adapters over the same running service would
/// simply mean each has its own dispatch queue, never a documented use.
pub struct KernelCommandAdapter {
    queue: Mutex<VecDeque<Command>>,
    client: CommandAuthorityServiceClient<Channel>,
    /// R3.1: the real OIDC **service** subject this adapter presents on every `Ack`/`Expire`/
    /// `Fail` RPC it makes (`crates/av-command/src/service.rs`'s module doc, "R3.1: a service
    /// principal, verified like a human token"; `docs/aiplane-plan.md` round 2's declared
    /// gap). A compact-serialization JWS, supplied as a plain value at construction -- **the
    /// caller of [`Self::new`] is responsible for reading it from a file the deployment
    /// provisions** (question 199: never the process environment, and never a value this
    /// crate hardcodes or mints itself; this crate has no `test-support` feature enabled in
    /// its own `[dependencies]`, so it cannot mint one even by accident -- only its own
    /// `[dev-dependencies]`-gated test build can). See the module doc's "Token lifetime" (below)
    /// for what this adapter does and does not do about the token expiring mid-run.
    service_token: String,
    /// The runtime [`ExternalCommandSource::report`]'s own `block_on` dispatches onto -- see
    /// the module doc's "bridging a synchronous trait method" section. Captured once, at
    /// construction (ordinarily from an async context that already has one, e.g. inside a
    /// `#[tokio::test]`), rather than re-resolved with `Handle::current()` inside `report`
    /// itself, so this type's own construction site is where a caller confronts the "which
    /// thread will `poll`/`report` actually run on" question, not a call buried three frames
    /// deep inside a kernel run.
    rt: tokio::runtime::Handle,
}

impl KernelCommandAdapter {
    /// `client` is this adapter's own private handle -- `CommandAuthorityServiceClient` is
    /// cheap to `Clone` (a `tonic::client::Grpc` over a shared `Channel`), so [`Self::report`]
    /// clones it per call rather than holding a lock across an `.await` point. `service_token`
    /// (R3.1) is presented, verbatim, on every `Ack`/`Expire`/`Fail` this adapter makes -- see
    /// [`Self::service_token`]'s own doc for who must supply it and from where. `rt` is the
    /// [`tokio::runtime::Handle`] [`Self::report`]'s own `block_on` calls will use --
    /// **the caller must ensure `poll`/`report` are never invoked from a thread that is
    /// itself currently being polled as an async task on this same runtime** (the module
    /// doc's own constraint); calling `av_kernel::drm::execute` directly from a plain,
    /// synchronous context -- never from inside an `async fn` body the runtime is polling,
    /// and never from inside an already-running `block_on` frame -- satisfies it (this
    /// crate's own `tests/command_dispatch_e2e.rs` convention: a plain `#[test]`, `rt.
    /// block_on(...)` only for this adapter's own setup RPCs, `execute()` called with no
    /// `block_on` wrapper at all).
    pub fn new(client: CommandAuthorityServiceClient<Channel>, service_token: String, rt: tokio::runtime::Handle) -> Self {
        Self { queue: Mutex::new(VecDeque::new()), client, service_token, rt }
    }
}

impl DispatchSink for KernelCommandAdapter {
    fn dispatch(&self, command: &Command) {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).push_back(command.clone());
    }
}

impl ExternalCommandSource for KernelCommandAdapter {
    /// Drains the queue [`DispatchSink::dispatch`] filled, in dispatch order (a `VecDeque`,
    /// FIFO) -- `av_kernel::drm::command_source`'s own module doc names this order
    /// authoritative for idempotency-duplicate detection and `report` order within one poll
    /// batch, so a FIFO drain (not, say, draining into a re-sorted `Vec`) is what preserves
    /// "dispatched in the order this service actually dispatched them."
    fn poll(&self, _now_tai_ns: i64) -> Vec<Command> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).drain(..).collect()
    }

    /// See the module doc's "D2's state mapping" and "bridging a synchronous trait method"
    /// sections for the full contract every branch below implements.
    fn report(&self, outcome: CommandOutcome) {
        match outcome {
            // The `Dispatch` RPC itself already recorded AUTHORIZED -> DISPATCHED before this
            // command ever reached the kernel's own queue -- nothing further to report here.
            CommandOutcome::Dispatched { .. } => {}
            CommandOutcome::Acked { id, level, epoch_tai_ns } => {
                let mut client = self.client.clone();
                let reason = format!("kernel reported {level:?} at kernel epoch {epoch_tai_ns} tai_ns");
                let service_token = self.service_token.clone();
                let result = self.rt.block_on(async move {
                    client
                        .ack(AckRequest { command_id: id.clone(), ack_level: level as i32, principal: KERNEL_PRINCIPAL.to_string(), reason, service_token })
                        .await
                        .map(|_| ())
                        .map_err(|status| (id, status))
                });
                if let Err((id, status)) = result {
                    eprintln!("av-run command adapter: Ack({id:?}, {level:?}) failed: {status}");
                }
            }
            CommandOutcome::Expired { id, deadline_tai_ns, kernel_epoch_tai_ns } => {
                let mut client = self.client.clone();
                let reason = format!("kernel: deadline_tai_ns={deadline_tai_ns} had already passed at kernel_epoch_tai_ns={kernel_epoch_tai_ns} (the earliest epoch the kernel would otherwise have dispatched this command) -- never dispatched");
                let service_token = self.service_token.clone();
                let result = self.rt.block_on(async move {
                    client
                        .expire(ExpireRequest { command_id: id.clone(), reason, principal: KERNEL_PRINCIPAL.to_string(), service_token })
                        .await
                        .map(|_| ())
                        .map_err(|status| (id, status))
                });
                if let Err((id, status)) = result {
                    eprintln!("av-run command adapter: Expire({id:?}) failed: {status}");
                }
            }
            CommandOutcome::DuplicateIdempotencyKey { id, idempotency_key } => {
                let mut client = self.client.clone();
                let reason = format!("kernel: refused as a duplicate idempotency_key {idempotency_key:?} within one poll batch");
                let service_token = self.service_token.clone();
                let result = self.rt.block_on(async move {
                    client
                        .fail(FailRequest { command_id: id.clone(), reason, principal: KERNEL_PRINCIPAL.to_string(), service_token })
                        .await
                        .map(|_| ())
                        .map_err(|status| (id, status))
                });
                if let Err((id, status)) = result {
                    eprintln!("av-run command adapter: Fail({id:?}, duplicate key) failed: {status}");
                }
            }
            CommandOutcome::NotDispatchedRunEnded { id, not_before_tai_ns, run_end_tai_ns } => {
                let mut client = self.client.clone();
                let reason = format!("kernel: not_before_tai_ns={not_before_tai_ns} fell at or after this run's own run_end_tai_ns={run_end_tai_ns} -- never dispatched, run ended first");
                let service_token = self.service_token.clone();
                let result = self.rt.block_on(async move {
                    client
                        .fail(FailRequest { command_id: id.clone(), reason, principal: KERNEL_PRINCIPAL.to_string(), service_token })
                        .await
                        .map(|_| ())
                        .map_err(|status| (id, status))
                });
                if let Err((id, status)) = result {
                    eprintln!("av-run command adapter: Fail({id:?}, run ended) failed: {status}");
                }
            }
            CommandOutcome::Refused { id, reason } => {
                let mut client = self.client.clone();
                let reason_text = format!("kernel: {reason}");
                let service_token = self.service_token.clone();
                let result = self.rt.block_on(async move {
                    client
                        .fail(FailRequest { command_id: id.clone(), reason: reason_text, principal: KERNEL_PRINCIPAL.to_string(), service_token })
                        .await
                        .map(|_| ())
                        .map_err(|status| (id, status))
                });
                if let Err((id, status)) = result {
                    eprintln!("av-run command adapter: Fail({id:?}, refused) failed: {status}");
                }
            }
        }
    }
}
