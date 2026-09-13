//! The proposer's ONLY seam into `av-gateway`: [`GatewayClient`] holds exactly the two
//! generated clients `crate::gateway_pb` provides -- `DataGatewayServiceClient` (read the
//! run's scores) and `ModelProposeServiceClient` (A4b's network propose path) -- and nothing
//! else. This crate's acceptance evidence item 1 ("this crate generates no client for
//! `CommandAuthorityService` at all") is proven structurally by `build.rs`'s own design (see
//! that file's module doc) and re-proven by `crates/av-proposer/tests/
//! no_command_authority_client.rs`, which greps this crate's own `$OUT_DIR` for the string
//! `"CommandAuthorityService"` and asserts zero matches -- this module is simply the one
//! place that dials the two clients that DO exist.

use tonic::transport::Channel;
use tonic::Status;

use av_cdm::pb::{GatewayQueryRequest, GatewayQueryResponse, ProposeCommandRequest, ProposeCommandResponse};

use crate::gateway_pb::data_gateway_service_client::DataGatewayServiceClient;
use crate::gateway_pb::model_propose_service_client::ModelProposeServiceClient;

/// Every way connecting to `av-gateway` can fail.
#[derive(Debug)]
pub struct GatewayConnectError(pub String);

impl std::fmt::Display for GatewayConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "connecting to av-gateway: {}", self.0)
    }
}

/// A connected session against one `av-gateway` endpoint's `DataGatewayService` and
/// `ModelProposeService`, sharing one underlying `Channel` (tonic multiplexes both service's
/// calls over the same HTTP/2 connection -- one endpoint, one dial).
pub struct GatewayClient {
    data_gateway: DataGatewayServiceClient<Channel>,
    model_propose: ModelProposeServiceClient<Channel>,
}

impl GatewayClient {
    /// Dials `endpoint` (e.g. `"http://127.0.0.1:50170"`) over the same plaintext-on-loopback
    /// tonic stack every other service in this workspace uses (ADR-004: no `tls*` feature
    /// anywhere in this crate's `tonic` dependency).
    pub async fn connect(endpoint: String) -> Result<Self, GatewayConnectError> {
        let channel = tonic::transport::Endpoint::from_shared(endpoint)
            .map_err(|e| GatewayConnectError(e.to_string()))?
            .connect()
            .await
            .map_err(|e| GatewayConnectError(e.to_string()))?;
        Ok(Self::from_channel(channel))
    }

    /// Builds directly from an already-connected [`Channel`] -- this crate's own test suite's
    /// way of dialing a real, in-process `av-gateway` over a real loopback socket, mirroring
    /// `crates/av-gateway/src/propose_only.rs::ProposeOnlyAuthority::from_channel`.
    pub fn from_channel(channel: Channel) -> Self {
        Self { data_gateway: DataGatewayServiceClient::new(channel.clone()), model_propose: ModelProposeServiceClient::new(channel) }
    }

    /// `DataGatewayService.Query` -- the read-only half (D1's own guarantee: nothing reached
    /// through this method can ever create a `Command`).
    pub async fn query(&mut self, request: GatewayQueryRequest) -> Result<GatewayQueryResponse, Status> {
        Ok(self.data_gateway.query(request).await?.into_inner())
    }

    /// `ModelProposeService.ProposeCommand` -- A4b's network propose path. The ONLY method on
    /// this type that can create a `Command`, and it can only ever create one at
    /// `COMMAND_STATE_PROPOSED` (the real state machine, reached through `crate::propose_flow::
    /// propose_command` on the `av-gateway` side, enforces that -- this client cannot smuggle
    /// past it any more than the MCP tool can).
    pub async fn propose_command(&mut self, request: ProposeCommandRequest) -> Result<ProposeCommandResponse, Status> {
        Ok(self.model_propose.propose_command(request).await?.into_inner())
    }
}
