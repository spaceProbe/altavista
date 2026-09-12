//! E3: the ingest, both halves (`docs/edge-plan.md` milestone E3; ADR-004's security
//! boundary and evidence rules; question 202, E3b's charter).
//!
//! - **E3a: the ingest as a library.** The durable, file-backed, per-partition chained
//!   log that *is* the ledger ([`log`]); the accept/reject pipeline wiring `av_edge::
//!   chain::ChainVerifier` and `av_edge::identity` verification to appends ([`ingest`]);
//!   the counters; crash recovery; determinism; the evidence surface as data
//!   ([`evidence`]); the control matrix (`docs/compliance/av-ingest/control-matrix.md`).
//! - **E3b: the wire.** A plaintext-on-loopback `tonic` server for `altavista.v1.
//!   EdgeIngest` ([`service`], bind/refusal in [`server`]), the plugin manifest handshake
//!   ([`service`]'s own module doc), the forwarded-client-certificate header contract
//!   ([`forwarded_cert`]), and the hand-rolled `GET`-only `/admin/api/evidence` HTTP
//!   surface ([`admin`], modelled directly on `crates/av-dynamics-service/src/admin.rs`).
//!   Question 155's rule applies verbatim: plaintext gRPC on loopback within one host; a
//!   service-owned nginx mTLS front (not rendered or run by this crate -- see
//!   `docs/edge-plan.md`) is the only sanctioned cross-host path, and no acceptor crate
//!   (`rustls`, `ring`, `tokio-rustls`, or any other TLS/crypto-adjacent crate beyond what
//!   `av-edge`/this crate already use) is added anywhere in this crate's dependency tree.
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
//!   mounted by both [`admin`] (`GET /admin/api/evidence`/`.../verify`) and [`service`]
//!   (`GetEvidence`/`VerifyLedger`).
//! - [`forwarded_cert`] -- the exact gRPC metadata header a service-owned nginx mTLS front
//!   is expected to carry a client's certificate on, and how this crate un-escapes it.
//! - [`service`] -- [`service::EdgeIngestService`]: the `EdgeIngest` `tonic` service trait
//!   implementation. The manifest handshake, announced-producer tracking (scoped to the
//!   serving process instance, not a TCP connection -- that module's doc says why), and the
//!   clock this crate is injected with rather than ever reading live.
//! - [`server`] -- [`server::bind_loopback`]: the one entry point that turns a bind
//!   address into a listening plaintext socket, refusing a non-loopback address with a
//!   typed error before a socket is ever opened.
//! - [`admin`] -- the hand-rolled `GET`-only `/admin/api/evidence`/`.../verify` HTTP
//!   surface, a plain `tokio::net::TcpListener`, never `axum`/`hyper` directly.

pub mod admin;
pub mod evidence;
pub mod forwarded_cert;
pub mod ingest;
pub mod log;
pub mod server;
pub mod service;

pub mod pb {
    //! Generated `altavista.v1.edge_ingest_server` plumbing only (`build.rs`
    //! `extern_path`s every message type onto [`av_cdm::pb`] -- see that file's own doc
    //! comment), following `crates/av-dynamics-service/src/lib.rs`'s identical `pb`
    //! module convention verbatim: every message type this crate touches is named as
    //! `av_edge::pb::X` (== `av_cdm::pb::X`) directly at every call site in this crate
    //! (`src/ingest.rs`, `src/service.rs`, ...), never through this module, so this module
    //! is in practice just the `EdgeIngest` service trait and its `EdgeIngestServer<T>`
    //! tower wrapper -- never a second copy of the wire types `av-edge`/`av-cdm` already
    //! share. (This crate's own `build.rs` compiles every `.proto` under `proto/altavista/
    //! v1/`, not only `edge.proto`, so this module also contains an unused, harmless
    //! `dynamics_service_server`/`lockstep_service_server` -- the same thing
    //! `av-dynamics-service`'s own generated `pb` module does for the services it does not
    //! implement either; nothing in this crate names them.)
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}
