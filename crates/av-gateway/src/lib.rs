//! A4a (`docs/aiplane-plan.md` milestone A4; ADR-004's "AI plane" section): the read-only,
//! label-aware gateway over run products, exposed as gRPC ([`gateway`]) and as a hand-rolled
//! MCP/JSON-RPC-2.0-over-stdio server ([`mcp`]) with a deny-by-default tool allow-list, plus
//! a `propose_command` tool that can create `PROPOSED` and nothing else ([`propose_only`]).
//!
//! ## Module map
//!
//! - [`counters`] -- **moved to `av_command::counters` (R3.1)**, unchanged in behaviour, and
//!   re-exported here under the identical `crate::counters::{Counted, Counters}` path so no
//!   call site in this crate changed: `av-command` gained its own refusal surface this round
//!   (service-principal verification on `Dispatch`/`Ack`/`Expire`/`Fail`) and needed the
//!   identical shared counting primitive; the move goes to the lower crate in the dependency
//!   graph (`av-gateway` already depends on `av-command`, never the reverse) rather than
//!   duplicating the type. Still ADR-004's "everything rejected is counted" rule,
//!   mechanically enforced by having exactly one counting type rather than ad hoc counters
//!   per module, now shared by both crates instead of owned by this one alone.
//! - [`labels`] -- [`labels::ClearanceLadder`] (D2): an explicit, deployment-configured,
//!   ordered clearance ladder (rank = index), copied from `crates/av-edge/src/policy.rs`'s
//!   `ProducerPolicy` convention verbatim -- never a hardcoded enum or a numeric level. A
//!   marking absent from the ladder is always refused, on either side of the comparison
//!   (the caller's claimed clearance or the product's own label), never defaulted to a
//!   rank; mislabeling is checked before over-clearance.
//! - [`catalogue`] -- [`catalogue::RunCatalogue`] (D1): the gateway's OWN configured
//!   catalogue of run products, keyed by [`pb::RunIdentity::run_id`] -- a caller names a run
//!   by identity, never a path, and this module is the only place identity is resolved
//!   against real bytes.
//! - [`query_id`] -- [`query_id::compute_query_id`] (D5): a deterministic id derived from a
//!   query's own canonical content, mirroring `crates/av-command/src/policy.rs`'s
//!   `compute_decision_id` preimage convention byte for byte.
//! - [`gateway`] -- [`gateway::GatewayCore`], the label-aware query resolution D1/D2 both
//!   drive (shared by the gRPC service impl and the MCP `query` tool, so there is exactly
//!   one place the ordered typed-refusal chain lives), and
//!   [`gateway::DataGatewayServiceImpl`], the generated `DataGatewayService` server trait
//!   implementation over it.
//! - [`evidence`] -- [`evidence::EvidenceRecorder`] (D6): "an evidence topic" realized as a
//!   record on the existing, unmodified `av_command::ledger::Ledger`, under its own
//!   documented partition convention -- never a broker (no Kafka/Redpanda crate exists in
//!   this tree or is added here).
//! - [`propose_only`] -- [`propose_only::ProposeOnlyAuthority`] (D4): a type whose only
//!   public method is `propose`; the generated `CommandAuthorityServiceClient` (`check`,
//!   `authorize`, `dispatch`, `ack`, `expire`, `fail`) is a private field, unreachable from
//!   the MCP or gRPC surface this crate exposes.
//! - [`propose_flow`] -- R3.2/A4b: the ONE shared implementation `propose_command` runs
//!   through, called by both the MCP `propose_command` tool ([`mcp`], stdio) and the new
//!   `ModelProposeService.ProposeCommand` rpc ([`propose_flow::ModelProposeServiceImpl`],
//!   gRPC -- served by `av-gateway` on the same port as `DataGatewayService`, additive on
//!   `authority.proto`) -- a containerised proposer with no stdio channel to this process
//!   needs a network propose path, and this is it. See that module's own doc.
//! - [`mcp`] -- [`mcp::McpServer`] (D3): hand-rolled JSON-RPC 2.0 over injected
//!   `AsyncBufRead`/`AsyncWrite` streams (never the process's real stdin/stdout in a test --
//!   question 199), `initialize`/`tools/list`/`tools/call`, a deny-by-default allow-list
//!   derived from one source ([`mcp::GatewayTool`]) so `tools/list` and the dispatch table
//!   can never disagree.
//! - [`pb`] -- generated `altavista.v1` plumbing only (`build.rs` `extern_path`s every
//!   message type onto [`av_cdm::pb`]; see that module's own doc).
//!
//! ## Determinism (D8)
//!
//! No `HashMap` on any output path (every map in this crate's own types is a `BTreeMap`;
//! every `map<..>` field in the generated `altavista.v1` types is `.btree_map(["."])`'d by
//! `av-cdm`/this crate's own `build.rs`), no wall clock (every epoch is a parameter,
//! `av_command::clock::Clock`, never `SystemTime::now()` read directly in this crate), no
//! random id anywhere (D5's query id and every refusal are pure functions of their inputs).

pub mod catalogue;
/// R3.1: this module moved to `av_command::counters` unchanged in behaviour -- see that
/// module's own doc for the full reasoning. Re-exported under this crate's own, pre-existing
/// `counters` path so every call site here (`crate::counters::{Counted, Counters}`) keeps
/// resolving without change.
pub mod counters {
    pub use av_command::counters::{Counted, Counters};
}
pub mod evidence;
pub mod gateway;
pub mod labels;
pub mod mcp;
pub mod propose_flow;
pub mod propose_only;
pub mod query_id;
pub mod unknown_route_counter;

pub mod pb {
    //! Generated `altavista.v1` plumbing, compiled by `build.rs`: the `DataGatewayService`
    //! server/client (this crate's own new service) and a second, independently-generated
    //! `CommandAuthorityService` client (used only by [`crate::propose_only`]) -- every
    //! message type is `extern_path`'d onto [`av_cdm::pb`] (see `build.rs`'s own doc
    //! comment), so this module never produces a second copy of a wire type `av-cdm`
    //! already compiles. Mirrors `crates/av-command/src/lib.rs`'s identical `pb` module and
    //! its inner-attribute spelling (generated code, not this crate's own style, so this
    //! crate's own rule against lint-suppressing attributes on hand-written items does not
    //! reach this module).
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}

/// D7: this crate reuses `av_command::service::resolve_loopback_bind_address`/
/// `BindAddressError` directly (a regular, non-dev dependency already) rather than a second
/// copy of that logic -- see `crates/av-gateway/src/bin/av-gateway.rs`'s own module doc for
/// where both this crate's gRPC and MCP-adjacent listeners are bound through it. This test
/// proves the reused function is actually wired and reachable from this crate (not merely
/// present in a dependency this crate happens to also pull in), and pins question 155's own
/// acceptance line at this crate's own boundary too.
#[cfg(test)]
mod bind_address_reuse {
    use av_command::service::{resolve_loopback_bind_address, BindAddressError};

    #[test]
    fn a_non_loopback_bind_address_is_refused_naming_question_155() {
        let err = resolve_loopback_bind_address("0.0.0.0:50170").unwrap_err();
        assert!(matches!(err, BindAddressError::NotLoopback { .. }), "{err:?}");
        assert!(err.to_string().contains("question 155"), "{err}");
    }

    #[test]
    fn a_bare_localhost_with_no_port_is_a_typed_missing_port_never_a_panic() {
        let err = resolve_loopback_bind_address("localhost").unwrap_err();
        assert!(matches!(err, BindAddressError::MissingPort { .. }), "{err:?}");
    }

    #[test]
    fn a_loopback_address_resolves() {
        assert!(resolve_loopback_bind_address("127.0.0.1:50170").is_ok());
    }
}
