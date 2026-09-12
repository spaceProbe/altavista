//! The command authority library and service (`docs/aiplane-plan.md` milestone A1; ADR-004's
//! "Command authority" section; `docs/open-questions.md` question 201). The state machine,
//! the durable ledger, the injected clock and Rego policy evaluation at `CHECKED` all stand
//! on their own as a library (A1.1/A1.2); [`service`] (A1.3) is the `CommandAuthorityService`
//! gRPC surface over the same OpenSSL-backed tonic stack `av-grpc`/`av-dynamics-service` use.
//!
//! ## Module map
//!
//! - [`clock`] -- the injected TAI-nanosecond clock (`Clock`, `SystemClock`, `TestClock`).
//!   Nothing else in this crate reads the wall clock.
//! - [`state`] -- the `CommandState` machine: one transition function per edge
//!   (`propose`, `check`, `authorize`, `dispatch`, `ack`, `reject`, `expire`, `fail`), a
//!   typed [`state::CommandError`] for every illegal edge, and question 53's propose-only
//!   rule enforced in code.
//! - [`ledger`] -- the durable, file-backed, hash-chained, per-partition command ledger
//!   (ADR-004: "the durable log itself is the ledger, chained per partition").
//! - [`policy`] -- Rego policy evaluation at `CHECKED` (A1.2, question 201(a)): the
//!   [`policy::PolicyBundle`] loader and its content hash, [`policy::evaluate`], and the
//!   `profiles/execution.yaml` `authority:` block loader.
//! - [`rate`] -- [`rate::RateSource`], the trait behind `PolicyInputRate.counts_by_class`,
//!   and its two implementors: the ledger-backed one and a deterministic test fixture.
//! - [`authority`] -- the check edge (A1.2): evaluates policy over a `PROPOSED` `Command` and
//!   drives `state::check`/`state::reject` plus the matching `Ledger::append`.
//! - [`fips`] -- FIPS posture *detection* (never assertion) of the linked OpenSSL, copied
//!   from `crates/av-dynamics-service/src/fips.rs` with attribution (see that module's doc).
//! - [`evidence`] -- what `/admin/api/evidence`(`/verify`) report, built from [`ledger`] and
//!   [`fips`].
//! - [`admin`] -- the localhost-only `/admin/api/evidence*` HTTP endpoint (hand-rolled
//!   `tokio::net::TcpListener`, no `axum`/`hyper`).
//! - [`pb`] -- generated `altavista.v1.command_authority_service_server` plumbing only
//!   (`build.rs` `extern_path`s every message type onto [`av_cdm::pb`], so this module is,
//!   in practice, just the `CommandAuthorityService` server trait and the
//!   `CommandAuthorityServiceServer<T>` tower wrapper -- never a second copy of the wire
//!   types `av-cdm` already compiles).
//! - [`service`] -- [`service::CommandAuthorityServiceImpl`] (A1.3): `Propose`/`Check`/
//!   `Authorize`/`Dispatch`/`Ack`/`Query`/`VerifyLedger` over the wire, the `DispatchSink`
//!   seam A3 fills, and the question-155 loopback-only bind-address check.
//!
//! Not yet in this crate (later milestones, so the next worker does not invent a second
//! shape for something already planned): `Principal`, `Delegation`, role bindings, MFA (A2);
//! dispatch into the kernel's real telecommand path (A3, behind [`service::DispatchSink`]).

pub mod admin;
pub mod authority;
pub mod clock;
pub mod evidence;
pub mod fips;
pub mod ledger;
pub mod policy;
pub mod rate;
pub mod service;
pub mod state;

pub mod pb {
    //! Generated `altavista.v1.command_authority_service_server` plumbing, compiled by
    //! `build.rs`. Every message type is `extern_path`'d onto [`av_cdm::pb`] (see
    //! `build.rs`'s own doc comment), so this module is, in practice, just the
    //! `CommandAuthorityService` server trait and the `CommandAuthorityServiceServer<T>`
    //! tower service wrapper -- never a second copy of the wire types `av-cdm` already
    //! compiles. The inner attribute right below (outer-attribute spelling deliberately
    //! avoided in this sentence, so it does not itself trip a textual scan for one) matches
    //! `crates/av-dynamics-service/src/lib.rs`'s and `crates/av-grpc/src/lib.rs`'s identical
    //! module (generated code, not this crate's own style) -- this crate's own rule against
    //! lint-suppressing attributes on hand-written items does not reach this one
    //! generated-code module, whose inner-attribute spelling is deliberately distinct from
    //! the outer form that rule's own verification scan looks for.
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}
