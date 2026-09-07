//! M6.2: a Rust-hosted `tonic` server for `altavista.v1.DynamicsService`, over
//! `gmat-sys`/`av-dynamics` (ADR-002 depth 2, `"gmat-ffi"`).
//!
//! ADR-003's 2026-09-02 amendment decided this: `grpcio` (what
//! `services/gmat-service` is built on) bundles BoringSSL and cannot terminate TLS on the
//! host's FIPS OpenSSL, so a cross-host `DynamicsService` call needs a rule-compliant
//! transport somewhere -- this crate is that somewhere. `services/gmat-service` stays as a
//! **design-time** tool (solvers, authoring, golden generation); this crate is what a
//! deployed profile actually runs. See `docs/adr/003-substrate-and-deployment.md`'s
//! amendment and this crate's README for the full picture.
//!
//! ## Module map
//!
//! - [`pb`] -- generated `altavista.v1.dynamics_service_server` plumbing only (`build.rs`
//!   `extern_path`s every message type onto `av_cdm::pb`, so this crate never generates a
//!   second, independent copy of `ModelInfo`/`PropagateRequest`/etc.).
//! - [`config`] -- the one fixed model identity/settings this server hosts.
//! - [`worker`] -- the GMAT single-thread contract: one dedicated OS thread owns every
//!   `gmat-sys` handle for the life of the process; [`worker::WorkerHandle::run`] is the
//!   only way an async RPC handler reaches it.
//! - [`propagate`] -- `Propagate`'s sampling loop (plain and STM-augmented/covariance).
//! - [`evidence`] -- the JSONL evidence log, SHA-256 via the `openssl` crate, hash-chained
//!   (`prev_hash`/`hash`, `"GENESIS"` convention) with a [`evidence::EvidenceLog::verify`].
//! - [`fips`] -- FIPS posture *detection* (never assertion) of the linked OpenSSL, for
//!   [`admin`]'s `/admin/api/evidence`.
//! - [`admin`] -- the localhost-only `/admin/api/evidence` HTTP endpoint (ADR-004 question 63).
//! - [`service`] -- the `DynamicsService` trait implementation ([`service::DynamicsServiceImpl`]).
//!
//! **Plaintext on localhost only** (ADR-004/ADR-003 amendment): this crate never links any
//! TLS stack (`tonic`'s `server` feature, not `tls`/`tls-native-roots`/`tls-webpki-roots` --
//! see `Cargo.toml`'s dependency comment). A cross-host call is terminated by a
//! service-owned nginx front (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`),
//! the same pattern `services/gmat-service` itself uses.

pub mod pb {
    //! Generated `altavista.v1.dynamics_service_server` plumbing, compiled by `build.rs`.
    //! Every message type is `extern_path`'d onto [`av_cdm::pb`] (see `build.rs`'s own doc
    //! comment), so this module is, in practice, just the `DynamicsService` server trait
    //! and the `DynamicsServiceServer<T>` tower service wrapper -- never a second copy of
    //! the wire types `av-cdm`/`av-dynamics`/`gmat-sys` already share.
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}

pub mod admin;
pub mod config;
pub mod evidence;
pub mod fips;
pub mod propagate;
pub mod service;
pub mod worker;

pub use evidence::EvidenceLog;
pub use service::DynamicsServiceImpl;
pub use worker::WorkerHandle;
