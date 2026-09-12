//! E3a: the ingest as a library (`docs/edge-plan.md` milestone E3, first half; ADR-004's
//! security boundary and evidence rules).
//!
//! Milestone E3 has two halves. **This crate is the first half only.**
//!
//! - **E3a (this crate): the ingest as a library.** The durable, file-backed,
//!   per-partition chained log that *is* the ledger ([`log`]); the accept/reject pipeline
//!   wiring `av_edge::chain::ChainVerifier` and `av_edge::identity` verification to
//!   appends ([`ingest`]); the counters; crash recovery; determinism; the evidence
//!   surface as data ([`evidence`]); the control matrix
//!   (`docs/compliance/av-ingest/control-matrix.md`).
//! - **E3b (NOT this crate, a later round): the wire.** The gRPC service, the plugin
//!   manifest handshake, and the mTLS front. Deferred because this repository has an
//!   OpenSSL TLS *client* connector (`crates/av-grpc`) and no server acceptor, and open
//!   question 155 ruled "no new crypto-adjacent crate" for exactly that gap, with an
//!   nginx front as the server side (`crates/av-dynamics-service`'s own precedent) --
//!   that is the lead's decision to confirm, not this round's. Accordingly, **this crate
//!   depends on no `tonic`, `hyper`, `axum`, or TLS crate, opens no socket, and defines no
//!   gRPC service anywhere in it.** Every test in `tests/` exercises this crate's own
//!   public API directly (in-process, against a temp directory), never a wire -- each
//!   test module's own doc comment says so, so "every rejection kind counted through the
//!   wire" (the plan's E3 prose) is understood this round as "through the ingest's public
//!   API", with the wire itself explicitly left to E3b.
//!
//! # Modules
//!
//! - [`log`] -- [`log::PartitionLog`]: one durable, append-only, hash-chained file per
//!   partition (`shard_key`), with crash recovery and independent `verify()`. See that
//!   module's doc for the exact record framing.
//! - [`ingest`] -- [`ingest::Ingest`]: the pipeline. Identity (if a certificate was
//!   presented) -> `SHARD_MISMATCH` -> `av_edge::chain::ChainVerifier` -> append only if
//!   accepted -> the verdict. See that module's doc for exactly where `SHARD_MISMATCH`
//!   sits relative to `av_edge::chain`'s own documented check order, and why.
//! - [`evidence`] -- [`evidence::evidence`]/[`evidence::verify_all`]: the evidence surface
//!   as plain data (a `serde_json::Value` and a map of `av_cdm::pb::ChainVerification`),
//!   for E3b to mount on `GET /admin/api/evidence`/`.../verify`.

pub mod evidence;
pub mod ingest;
pub mod log;

pub use av_edge::pb;
