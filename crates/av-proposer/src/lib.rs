//! A4b (`docs/aiplane-plan.md` milestone A4; round 2's decision 11): the first model
//! sidecar, a deterministic, rule-based station-keeping proposer -- proposes a burn when a
//! scored radius drifts past a declared threshold ([`rule`]), talking to `av-gateway` over a
//! real network socket (never MCP-over-stdio, which a separate container has no access to;
//! [`gateway_client`]), and implementing spoore's `ModelService` contract for real
//! ([`model_service`]) so a golden scorecard or a later, learned sidecar under the same
//! contract can stand next to it.
//!
//! ## Module map
//!
//! - [`command_id`] (D4): [`command_id::compute_command_id`]/[`command_id::
//!   compute_idempotency_key`], SHA-256 through `openssl`, mirroring `crates/av-gateway/src/
//!   query_id.rs`'s canonical-preimage convention byte for byte.
//! - [`rule`] (D3): [`rule::evaluate`], the station-keeping rule -- see that module's own doc
//!   for the exact formula and its units.
//! - [`model_service`] (D2): [`model_service::ModelServiceImpl`], spoore's `ModelService`
//!   contract over a real `spoore_models::KalmanFilter`, built exactly as `crates/av-track/
//!   src/config.rs` builds one.
//! - [`gateway_client`]: [`gateway_client::GatewayClient`], this crate's ONLY seam into
//!   `av-gateway` -- `DataGatewayService.Query` and `ModelProposeService.ProposeCommand`, and
//!   structurally nothing else (this crate generates no `CommandAuthorityService` client at
//!   all; see `build.rs`'s own doc and `tests/no_command_authority_client.rs`).
//! - [`proposer`]: [`proposer::run`], one complete propose-run (D5) -- query, evaluate,
//!   propose-or-report, never more than once.
//! - [`gateway_pb`]/[`model_service_pb`]: generated wire plumbing only, see each module's own
//!   doc for exactly what `build.rs` compiles into them and why.
//!
//! ## Determinism (D8)
//!
//! No wall clock anywhere in this crate's own code (every epoch this crate reads is either
//! spoore's own event-time `Epoch`, carried on the wire, or absent entirely -- this crate
//! holds no `av_command::clock::Clock` of its own; the evidence-recording epoch is
//! `av-gateway`'s job, on its own side of the network propose path). No random id (D4's
//! command id/idempotency key are pure SHA-256 functions of the proposal's own inputs). No
//! `HashMap` (`GatewayQueryResponse.scores` is already a `BTreeMap`, D1/av-cdm's own
//! `.btree_map(["."])`; this crate introduces no map of its own).

pub mod command_id;
pub mod gateway_client;
pub mod model_service;
pub mod proposer;
pub mod rule;

pub mod gateway_pb;
pub mod model_service_pb;
