//! The command authority library (`docs/aiplane-plan.md` milestone A1; ADR-004's "Command
//! authority" section; `docs/open-questions.md` question 201). This crate is a library
//! first: the state machine, the durable ledger, the injected clock, Rego policy evaluation
//! at `CHECKED` and the evidence/admin surface all stand on their own, with no gRPC service
//! wired up yet (A1.3, a separate task).
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
//!
//! Not yet in this crate (later milestones, so the next worker does not invent a second
//! shape for something already planned): the gRPC service surface (`propose`/`check`/
//! `authorize`/`dispatch`/`ack`/`query` over the wire, A1.3); `Principal`, `Delegation`, role
//! bindings, MFA (A2); dispatch into the kernel's real telecommand path (A3).

pub mod admin;
pub mod authority;
pub mod clock;
pub mod evidence;
pub mod fips;
pub mod ledger;
pub mod policy;
pub mod rate;
pub mod state;
