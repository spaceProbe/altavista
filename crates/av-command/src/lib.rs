//! The command authority library (`docs/aiplane-plan.md` milestone A1; ADR-004's "Command
//! authority" section; `docs/open-questions.md` question 201). This crate is a library
//! first: the state machine, the durable ledger, the injected clock and the evidence/admin
//! surface all stand on their own, with no gRPC service and no policy evaluator wired up
//! yet (both are later milestones -- A1.2 for Rego policy at `CHECKED`, the rest of A1 for
//! the gRPC service itself).
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
//! - [`fips`] -- FIPS posture *detection* (never assertion) of the linked OpenSSL, copied
//!   from `crates/av-dynamics-service/src/fips.rs` with attribution (see that module's doc).
//! - [`evidence`] -- what `/admin/api/evidence`(`/verify`) report, built from [`ledger`] and
//!   [`fips`].
//! - [`admin`] -- the localhost-only `/admin/api/evidence*` HTTP endpoint (hand-rolled
//!   `tokio::net::TcpListener`, no `axum`/`hyper`).
//!
//! Not yet in this crate (later milestones, so the next worker does not invent a second
//! shape for something already planned): any Rego policy evaluation (A1.2 -- this crate
//! depends on `regorus` already, with its crypto built-ins disabled, but does not call it
//! yet); the gRPC service surface (`propose`/`check`/`authorize`/`dispatch`/`ack`/`query`
//! over the wire, the rest of A1); `Principal`, `Delegation`, role bindings, MFA (A2);
//! dispatch into the kernel's real telecommand path (A3).

pub mod admin;
pub mod clock;
pub mod evidence;
pub mod fips;
pub mod ledger;
pub mod state;

/// Pins the exact `regorus` build this crate's `Cargo.toml` was set up with (setup step of
/// the AI-plane round, before A1.2 exists): `default-features = false`, `features = ["arc",
/// "regex"]`, evaluating a policy end to end with **no `std` feature** -- the feature this
/// crate must never turn on, because `regorus`'s `std` feature pulls `rand` -> `chacha20`, a
/// bundled-crypto crate ADR-004's rule forbids. If this probe ever fails to compile or
/// fails at runtime, that is a signal the `regorus` dependency line in `Cargo.toml` drifted
/// from what was verified at setup -- check `features`/`default-features` there before
/// touching anything else. A1.2 owns real Rego policy evaluation; this probe is not that,
/// and should move into whatever module A1.2 adds rather than be deleted, so the pin
/// survives past this task.
#[cfg(test)]
mod policy_dependency_probe {
    #[test]
    fn regorus_evaluates_a_policy_without_std() {
        let mut engine = regorus::Engine::new();
        engine
            .add_policy(
                "probe.rego".to_string(),
                "package probe\n\nallow if { input.x == 1 }\n".to_string(),
            )
            .expect("policy compiles");
        engine
            .add_data(regorus::Value::from_json_str("{}").expect("data"))
            .expect("data added");
        engine
            .set_input_json("{\"x\": 1}")
            .expect("input parses");
        let v = engine.eval_rule("data.probe.allow".to_string()).expect("rule evaluates");
        assert_eq!(v, regorus::Value::Bool(true));
    }
}
