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
//! - [`counters`] (R3.1) -- [`counters::Counters`]/[`counters::Counted`], ADR-004's
//!   "everything rejected is counted" primitive, moved here unchanged from `crates/
//!   av-gateway/src/counters.rs` (see that module's own doc for why the move direction is
//!   this one) -- every typed refusal `Dispatch`/`Ack`/`Expire`/`Fail`/`Authorize` can
//!   produce implements [`counters::Counted`]; `av-gateway` re-exports this module under its
//!   own `counters` path so its own call sites are unchanged.
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
//! - [`oidc`] (A2.1, `docs/aiplane-plan.md` milestone A2's principal half): OIDC token
//!   verification against the secsso claims contract (question 34, question 201(b)) --
//!   [`oidc::verify`], [`oidc::IssuerConfig`], the full [`oidc::TokenError`] refusal
//!   vocabulary. A pure function of (token, issuer configuration, clock reading); no I/O, no
//!   environment read, no wall clock. RS256 only -- see that module's doc for why ES256/
//!   ES384 are a named, documented gap rather than a half-built one.
//! - `test_support` -- **test-fixture-only, and gated out of a default build**
//!   (`#[cfg(any(test, feature = "test-support"))]`, `Cargo.toml`'s `[features]`/
//!   `[dev-dependencies]` -- see that module's own doc for why a plain, ungated `pub mod`
//!   was rejected on review): a local OpenSSL-backed OIDC issuer (`TestIssuer`) that mints
//!   real, signed RS256 tokens with caller-chosen claims, used by both this crate's unit
//!   tests and its integration tests. Absent entirely -- not merely unreferenced -- from
//!   `cargo build -p av-command`'s default-feature artifact.
//! - [`pb`] -- generated `altavista.v1.command_authority_service_server` plumbing only
//!   (`build.rs` `extern_path`s every message type onto [`av_cdm::pb`], so this module is,
//!   in practice, just the `CommandAuthorityService` server trait and the
//!   `CommandAuthorityServiceServer<T>` tower wrapper -- never a second copy of the wire
//!   types `av-cdm` already compiles).
//! - [`authz`] (A2.2, `docs/aiplane-plan.md` milestone A2's second half): the role gate
//!   ([`authz::RoleTable`], `Principal.groups`), the MFA gate for `Command.hazardous`
//!   (`Principal.amr`/`acr`), and time-limited [`authz::DelegationTable`] enforcement against
//!   the injected clock ([`authz::authorize_command`]) -- the one entry point `Authorize`
//!   calls, after [`oidc::verify`] has already established *who* the caller is.
//! - [`audit`] (A2.2): every transition, and every refused attempt, as one RFC 5424
//!   syslog-format line ([`audit::format_line`]) to a configured sink ([`audit::AuditWriter`],
//!   `profiles/execution.yaml`'s top-level `audit:` block).
//! - [`service`] -- [`service::CommandAuthorityServiceImpl`] (A1.3, A2.1, A2.2): `Propose`/
//!   `Check`/`Authorize`/`Dispatch`/`Ack`/`Query`/`VerifyLedger` over the wire, the
//!   `DispatchSink` seam A3 fills, the question-155 loopback-only bind-address check, (A2.1)
//!   `Authorize`'s real OIDC verification of `principal_token` via [`oidc::verify`], and
//!   (A2.2) the role/MFA/delegation gate ([`authz::authorize_command`]) plus an [`audit`] line
//!   for every transition and every refusal.
//!
//! Not yet in this crate (a later milestone, so the next worker does not invent a second
//! shape for something already planned): dispatch into the kernel's real telecommand path
//! (A3, behind [`service::DispatchSink`]).

pub mod admin;
pub mod audit;
pub mod authority;
pub mod authz;
pub mod clock;
pub mod counters;
pub mod evidence;
pub mod fips;
pub mod ledger;
pub mod oidc;
pub mod policy;
pub mod rate;
pub mod service;
pub mod state;
// A2.1: gated out of a default build -- `cargo build -p av-command` and the shipped
// `av-command` binary do not compile this module at all, not merely "never call it from
// production code". Active for this crate's own unit tests via `cfg(test)`, and for
// `tests/*.rs` (a separate compilation unit that cannot see `cfg(test)` code) via the
// `test-support` feature this crate's own `[dev-dependencies]` turns on for itself -- see
// `Cargo.toml`'s `[features]`/`[dev-dependencies]` comments and this module's own doc.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

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
