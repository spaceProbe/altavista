//! M13.2 (`docs/open-questions.md` question 107): a `tonic` client for
//! `altavista.v1.LockstepService` -- `Bind`/`Step`/`Reset`/`Shutdown` against an
//! already-running `BINDING_KIND_CONTAINER` process at a known address (container image
//! lifecycle -- pulling by digest, starting the process -- is explicitly out of this batch;
//! see this crate's README and `crates/av-kernel/README.md`).
//!
//! ## Why a separate crate, not folded into `av-grpc`
//!
//! `av-grpc`'s own `Cargo.toml`/module doc describe it as "the Rust `tonic` side of
//! `altavista.v1.DynamicsService`" -- a client bound to one specific service, fronted by
//! nginx mTLS (`services/gmat-service/deploy/`). Lockstep is a different shape of problem
//! (a stateful Bind-then-many-Steps-then-Shutdown session, sequence tracking, and a
//! protocol-error contract the caller must enforce itself -- `lockstep.proto`'s own doc
//! comment: "a response whose sequence does not match is a protocol error and the run
//! stops") that deserves its own home rather than blurring `av-grpc`'s stated scope. This
//! crate is instead a thin layer *on top of* `av-grpc`:
//!
//! - **No second `protoc`/`tonic-build` pass.** `av-grpc`'s own `build.rs` already compiles
//!   every `.proto` under `proto/altavista/v1/` (it globs the directory), so
//!   `av_grpc::pb::lockstep_service_client::LockstepServiceClient` and the
//!   `LockstepBindRequest`/`LockstepStepRequest`/... message types already exist; this crate
//!   depends on `av-grpc` as an ordinary library and reuses them directly (`pub use` below).
//!   `av-grpc/build.rs`'s two new `.extern_path` entries (M13.2) additionally make
//!   `LockstepBindRequest.ports`/`LockstepStepRequest.inputs`/`LockstepStepResponse.outputs`
//!   reference `av_cdm::pb::Port`/`PortMessage` directly -- the exact types
//!   `av_dynamics::Inbox`/`Outbox` already speak -- so a message crosses this RPC boundary
//!   with no field-by-field conversion.
//! - **The same "no `ring`" discipline `av-grpc` already proved.** [`connect_mtls`] is a thin
//!   wrapper over `av_grpc::tls::connect` (the OpenSSL-backed connector, never `tonic`'s own
//!   `tls`/`tls-native-roots`/`tls-webpki-roots` features -- see that module's doc comment).
//!   [`connect_plaintext`] needs no TLS stack at all: `tonic`'s `channel` feature alone
//!   already speaks plain HTTP/2 (h2c) over an `http://` URI, so a plaintext loopback
//!   connection touches neither OpenSSL nor `ring` -- **this path is for local-subprocess
//!   tests only** (`crates/av-kernel/tests/drm_container.rs`, `tests/test_lockstep_ref.py`),
//!   never a real cross-host binding, which ADR-003 requires mTLS for.
//! - **Sequence tracking and protocol-error checking live in `av-kernel`, not here.** This
//!   crate's [`LockstepClient`]/[`BlockingLockstepClient`] are a bare RPC transport --
//!   `bind`/`step`/`reset`/`shutdown` send exactly the request given and return exactly the
//!   response (or a `tonic::Status`), nothing more. `crates/av-kernel/src/drm/binding.rs`'s
//!   `ContainerModel` is what actually increments the sequence counter, checks
//!   `response.sequence`/`reached_tai_ns` against what was sent, and turns a mismatch into a
//!   typed `DrmError` -- see that module's doc comment. Keeping that logic out of this crate
//!   means a future non-DRM caller (a standalone lockstep smoke-test tool, say) gets the same
//!   honest, unopinionated transport this crate provides today.
//!
//! ## The sync/async boundary
//!
//! `av_dynamics::DynamicsModel`'s methods are `&self`, synchronous (the kernel drives every
//! model, GMAT-backed or native, from ordinary non-async code). [`BlockingLockstepClient`]
//! is what lets `ContainerModel` (an ordinary synchronous `DynamicsModel` impl) call this
//! crate's async RPCs: it owns a single-threaded `tokio::runtime::Runtime` and blocks on it
//! for every call, exactly the way a synchronous FFI call blocks -- there is no concurrency
//! inside one bound instance's own Bind/Step/.../Shutdown sequence to give up by blocking
//! (`lockstep.proto`'s own doc comment: "one outstanding [Step]").

// `tonic::Status` (this crate's own `Err` type on every RPC method below, matching the
// generated `LockstepServiceClient`'s own signatures verbatim) is ~176 bytes -- clippy's
// `result_large_err` would want it boxed. Not done: boxing here would just be extra
// indirection on every call site in `av-kernel` for a type this crate does not define and
// cannot shrink (it is `tonic`'s own type, returned unmodified) -- the same tradeoff
// `av-grpc`'s own generated `pb` module (also emitting bare `Result<_, tonic::Status>`
// methods) already accepts, just made explicit here since this crate is hand-written rather
// than `#[allow(clippy::all)]`-generated code.
#![allow(clippy::result_large_err)]

use std::path::Path;

/// Docker image lifecycle for a `BINDING_KIND_CONTAINER` instance whose `ContainerBinding`
/// declares `image`/`image_digest` -- see this module's own doc comment for the full account
/// (question 118, M15.3).
pub mod docker;

pub use av_grpc::pb::{
    LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownRequest, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse,
};
use av_grpc::pb::lockstep_service_client::LockstepServiceClient as GeneratedClient;
use av_grpc::tls::{MtlsConfig, MtlsConnectError};
use tonic::transport::{Channel, Endpoint};
use tonic::Status;

/// Everything that can go wrong establishing a [`LockstepClient`]/[`BlockingLockstepClient`]
/// connection (as distinct from an RPC failing once connected -- that is a plain
/// `tonic::Status`, returned directly by every method below).
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("{address:?} is not a valid plaintext endpoint URI: {source}")]
    InvalidEndpoint {
        address: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("connecting (plaintext) to {address}: {source}")]
    Connect {
        address: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("connecting (mTLS) to {endpoint}: {source}")]
    Mtls {
        endpoint: String,
        #[source]
        source: MtlsConnectError,
    },
    /// The blocking wrapper's own `tokio::runtime::Runtime::new()` failed (OS resource
    /// exhaustion -- not expected in practice, but not `unwrap`-away-able either).
    #[error("building the blocking client's tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
}

/// The bare async RPC transport -- see the module doc comment for what this crate does and
/// does not own. `Clone`-able the same way `tonic::transport::Channel` is (cheap, shares the
/// underlying connection), since `GeneratedClient` requires `&mut self` per call only to
/// serialize `tonic`'s own internal `Grpc::ready()` bookkeeping, not because the channel
/// itself is exclusive.
#[derive(Debug, Clone)]
pub struct LockstepClient {
    inner: GeneratedClient<Channel>,
}

impl LockstepClient {
    /// Connect over plain HTTP/2 (h2c), no TLS at all -- **local-subprocess tests only** (see
    /// the module doc comment). `address` is `host:port` (e.g. `"127.0.0.1:50070"`); this
    /// function builds the `http://` URI itself so a caller never has to remember the scheme.
    pub async fn connect_plaintext(address: &str) -> Result<Self, ConnectError> {
        let uri = format!("http://{address}");
        let ep = Endpoint::from_shared(uri.clone()).map_err(|source| ConnectError::InvalidEndpoint { address: uri.clone(), source })?;
        let channel = ep.connect().await.map_err(|source| ConnectError::Connect { address: uri, source })?;
        Ok(Self { inner: GeneratedClient::new(channel) })
    }

    /// Connect with mTLS through the system OpenSSL (`av_grpc::tls::connect`, never `ring`).
    /// `endpoint` is a full `https://host:port` URI (the ADR-003 default: mTLS whenever a
    /// binding crosses a host).
    pub async fn connect_mtls(endpoint: &str, cfg: MtlsConfig<'_>) -> Result<Self, ConnectError> {
        let channel = av_grpc::tls::connect(endpoint, cfg).await.map_err(|source| ConnectError::Mtls { endpoint: endpoint.to_string(), source })?;
        Ok(Self { inner: GeneratedClient::new(channel) })
    }

    pub async fn bind(&mut self, request: LockstepBindRequest) -> Result<LockstepBindResponse, Status> {
        self.inner.bind(request).await.map(tonic::Response::into_inner)
    }
    pub async fn step(&mut self, request: LockstepStepRequest) -> Result<LockstepStepResponse, Status> {
        self.inner.step(request).await.map(tonic::Response::into_inner)
    }
    pub async fn reset(&mut self, request: LockstepResetRequest) -> Result<LockstepResetResponse, Status> {
        self.inner.reset(request).await.map(tonic::Response::into_inner)
    }
    pub async fn shutdown(&mut self, request: LockstepShutdownRequest) -> Result<LockstepShutdownResponse, Status> {
        self.inner.shutdown(request).await.map(tonic::Response::into_inner)
    }
}

/// A [`LockstepClient`] plus the single-threaded `tokio` runtime it is driven through --
/// see the module doc comment's "The sync/async boundary" section. This is what
/// `av_kernel::drm::binding::ContainerModel` (a synchronous `DynamicsModel`) actually holds.
pub struct BlockingLockstepClient {
    runtime: tokio::runtime::Runtime,
    client: LockstepClient,
}

impl BlockingLockstepClient {
    pub fn connect_plaintext(address: &str) -> Result<Self, ConnectError> {
        let runtime = new_runtime()?;
        let client = runtime.block_on(LockstepClient::connect_plaintext(address))?;
        Ok(Self { runtime, client })
    }

    /// `ca_file`/`client_cert`/`client_key` are borrowed only for the duration of this call
    /// (the connector reads them once, during the handshake) -- see [`MtlsConfig`].
    pub fn connect_mtls(endpoint: &str, ca_file: &Path, client_cert: Option<&Path>, client_key: Option<&Path>) -> Result<Self, ConnectError> {
        let runtime = new_runtime()?;
        let cfg = MtlsConfig { ca_file, client_cert, client_key };
        let client = runtime.block_on(LockstepClient::connect_mtls(endpoint, cfg))?;
        Ok(Self { runtime, client })
    }

    pub fn bind(&mut self, request: LockstepBindRequest) -> Result<LockstepBindResponse, Status> {
        let client = &mut self.client;
        self.runtime.block_on(client.bind(request))
    }
    pub fn step(&mut self, request: LockstepStepRequest) -> Result<LockstepStepResponse, Status> {
        let client = &mut self.client;
        self.runtime.block_on(client.step(request))
    }
    pub fn reset(&mut self, request: LockstepResetRequest) -> Result<LockstepResetResponse, Status> {
        let client = &mut self.client;
        self.runtime.block_on(client.reset(request))
    }
    pub fn shutdown(&mut self, request: LockstepShutdownRequest) -> Result<LockstepShutdownResponse, Status> {
        let client = &mut self.client;
        self.runtime.block_on(client.shutdown(request))
    }
}

fn new_runtime() -> Result<tokio::runtime::Runtime, ConnectError> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(ConnectError::Runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_plaintext_to_nothing_listening_fails_with_a_typed_connect_error() {
        // Port 1 is a reserved/privileged port essentially never bound in a test sandbox --
        // this just proves the blocking connect path surfaces a real Err rather than hanging
        // or panicking when nothing answers.
        let err = match BlockingLockstepClient::connect_plaintext("127.0.0.1:1") {
            Ok(_) => panic!("nothing should be listening on 127.0.0.1:1 in a test sandbox"),
            Err(e) => e,
        };
        assert!(matches!(err, ConnectError::Connect { .. }), "{err:?}");
    }
}
