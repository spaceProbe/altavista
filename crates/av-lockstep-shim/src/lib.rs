//! M23.1 (`docs/open-questions.md` question 153, `docs/sil-plan.md` M23): a Rust process
//! that speaks `altavista.v1.LockstepService` (server side) to the kernel and
//! "lockstep-local v1" -- a length-prefixed frame protocol documented byte-exactly in
//! `services/cfs/README.md` -- over a Unix socket to whatever flight software is bound.
//!
//! ## Why this exists (question 153's decision)
//!
//! cFS is C; a gRPC implementation inside it would be heavy and hard to keep
//! deterministic. This shim runs beside the flight software in the same container,
//! fronting it: the kernel's `BINDING_KIND_CONTAINER` executor speaks ordinary
//! `LockstepService` gRPC to this process exactly as it already does to
//! `services/lockstep-ref` (`crates/av-lockstep`'s client, unmodified), and this process
//! translates every call into a `lockstep-local v1` frame exchange with the flight
//! software over a Unix socket. The same shim fronts the Renode bridge in M24, so nothing
//! in the local protocol (`framing.rs`, `peer_link.rs`) assumes cFS or a container --
//! `PeerLink` is generic over any `AsyncRead + AsyncWrite` stream, and every required test
//! in this crate drives it over an in-process `tokio::net::UnixStream::pair()`, never a
//! real filesystem socket or a real flight-software process.
//!
//! ## Module map
//!
//! - [`framing`] -- the frame header, `HELLO`/`ERROR` payload layouts, and the
//!   `encode_frame`/`read_frame`/`write_frame` primitives. This is "lockstep-local v1"
//!   itself; `services/cfs/README.md` documents it byte-exactly, and this module is that
//!   document's one implementation.
//! - [`peer_link`] -- `PeerLink<S>`: the stateful `bind`/`step`/`reset`/`shutdown` exchange
//!   built on `framing`, plus every protocol-error check this shim owns (one outstanding
//!   step, sequence echoes, `reached_tai_ns`, the handshake).
//! - [`service`] -- `ShimService`: the generated `altavista.v1.LockstepService` server
//!   trait implementation that the kernel actually dials, thin glue from `tonic::Request`/
//!   `tonic::Status` onto `PeerLink`.
//!
//! ## What is not implemented in this batch
//!
//! The brief calls for "TLS via the same OpenSSL stack as av-grpc" on the kernel-facing
//! side. `av-grpc`'s own OpenSSL integration (`av_grpc::tls`) is a **client**-side
//! connector (`hyper_openssl::client::legacy::HttpsConnector`, plugged into
//! `Endpoint::connect_with_connector`) -- nothing in this workspace's dependency tree
//! (`hyper-openssl` 0.10.2's own source has no server/acceptor module; confirmed by
//! inspecting the vendored `.crate` directly) provides an OpenSSL-backed **server**
//! acceptor for a `tonic`/`hyper` service without adding a new, network-fetched dependency
//! (`tokio-openssl` or equivalent) this offline environment cannot vet or build against.
//! `av_dynamics_service` (this workspace's other Rust-hosted gRPC server) resolves the
//! identical gap by binding plaintext on loopback and having a service-owned nginx front
//! terminate mTLS in front of it; this shim's own `--tls-cert`/`--tls-key`/`--tls-ca`
//! equivalent is therefore an explicitly open gap for a later batch, not a claim of a
//! working mTLS listener -- see this crate's README "Honesty" section. Every required
//! test in this batch uses plaintext loopback, which the task brief explicitly allows.
#![allow(clippy::result_large_err)]

pub mod pb {
    //! Generated `altavista.v1.lockstep_service_server` plumbing only -- every message
    //! type (`LockstepBindRequest`, `PortMessage`, ...) is `av_cdm::pb`'s own generated
    //! type, re-exported here for convenience so a caller never has to depend on both
    //! crates just to name one type.
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));

    pub use av_cdm::pb::{LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownRequest, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse, Port, PortMessage};
}

pub mod framing;
pub mod peer_link;
pub mod service;

pub use peer_link::{PeerLink, ProtocolError};
