//! Question 242 (c): a `tonic` client for `altavista.v1.BoardEdgeService`
//! (`proto/altavista/v1/board.proto`), the board's edge-node operations that are not lockstep
//! traffic. Today that is `PowerCycle`: the kernel asks the board's edge service (`av-edge-board`),
//! at the address it already dials for the board's `LockstepService`, to run the edge node's own
//! power control channel.
//!
//! Same shape and the same transport rules as [`crate::LockstepClient`] /
//! [`crate::BlockingLockstepClient`] (this crate owns the kernel's gRPC clients): plaintext h2c is
//! for loopback only, mTLS goes through `av_grpc::tls` (the system OpenSSL, never `ring`); the
//! generated client comes from `av-grpc`'s own build, which compiles every proto under
//! `proto/altavista/v1/`. Bare transport only: what a response means (a refusal, a failure) is the
//! caller's to turn into a typed error.
use std::path::Path;

pub use av_grpc::pb::{BoardPowerCycleFailure, BoardPowerCycleOutcome, BoardPowerCycleRefusal, PowerCycleRequest, PowerCycleResponse};
use av_grpc::pb::board_edge_service_client::BoardEdgeServiceClient as GeneratedClient;
use av_grpc::tls::MtlsConfig;
use tonic::transport::{Channel, Endpoint};
use tonic::Status;

use crate::ConnectError;

/// The bare async RPC transport for `BoardEdgeService`.
#[derive(Debug, Clone)]
pub struct BoardEdgeClient {
    inner: GeneratedClient<Channel>,
}

impl BoardEdgeClient {
    /// Plain HTTP/2 (h2c), **loopback only**. `address` is `host:port`.
    pub async fn connect_plaintext(address: &str) -> Result<Self, ConnectError> {
        let uri = format!("http://{address}");
        let ep = Endpoint::from_shared(uri.clone()).map_err(|source| ConnectError::InvalidEndpoint { address: uri.clone(), source })?;
        let channel = ep.connect().await.map_err(|source| ConnectError::Connect { address: uri, source })?;
        Ok(Self { inner: GeneratedClient::new(channel) })
    }

    /// mTLS through the system OpenSSL; `endpoint` is a full `https://host:port` URI.
    pub async fn connect_mtls(endpoint: &str, cfg: MtlsConfig<'_>) -> Result<Self, ConnectError> {
        let channel = av_grpc::tls::connect(endpoint, cfg).await.map_err(|source| ConnectError::Mtls { endpoint: endpoint.to_string(), source })?;
        Ok(Self { inner: GeneratedClient::new(channel) })
    }

    pub async fn power_cycle(&mut self, request: PowerCycleRequest) -> Result<PowerCycleResponse, Status> {
        self.inner.power_cycle(request).await.map(tonic::Response::into_inner)
    }
}

/// A [`BoardEdgeClient`] plus the current-thread runtime it is driven through, for the kernel's
/// synchronous code (as [`crate::BlockingLockstepClient`]).
pub struct BlockingBoardEdgeClient {
    runtime: tokio::runtime::Runtime,
    client: BoardEdgeClient,
}

impl BlockingBoardEdgeClient {
    pub fn connect_plaintext(address: &str) -> Result<Self, ConnectError> {
        let runtime = crate::new_runtime()?;
        let client = runtime.block_on(BoardEdgeClient::connect_plaintext(address))?;
        Ok(Self { runtime, client })
    }

    pub fn connect_mtls(endpoint: &str, ca_file: &Path, client_cert: Option<&Path>, client_key: Option<&Path>) -> Result<Self, ConnectError> {
        let runtime = crate::new_runtime()?;
        let cfg = MtlsConfig { ca_file, client_cert, client_key };
        let client = runtime.block_on(BoardEdgeClient::connect_mtls(endpoint, cfg))?;
        Ok(Self { runtime, client })
    }

    pub fn power_cycle(&mut self, request: PowerCycleRequest) -> Result<PowerCycleResponse, Status> {
        let client = &mut self.client;
        self.runtime.block_on(client.power_cycle(request))
    }
}
