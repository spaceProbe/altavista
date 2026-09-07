//! `ShimService`: the `altavista.v1.LockstepService` server the kernel actually dials --
//! thin glue from `tonic::Request`/`tonic::Status` onto [`crate::peer_link::PeerLink`].
//! Every protocol check (one outstanding step, sequence echoes, `reached_tai_ns`, the
//! handshake) already happened in `PeerLink`; this module's only job is turning a
//! [`ProtocolError`] into a `tonic::Status` the kernel's `av_lockstep::LockstepClient` can
//! observe as an RPC failure, per `lockstep.proto`'s own doc comment: "a response whose
//! sequence does not match is a protocol error and the run stops."
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tonic::{Request, Response, Status};

use crate::pb::lockstep_service_server::LockstepService;
use crate::pb::{LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownRequest, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse};
use crate::peer_link::{PeerLink, ProtocolError};

/// `Status::aborted` for every [`ProtocolError`] variant: none of them is a request the
/// kernel could usefully retry unmodified (a stale sequence, a wedged step, a peer that
/// cannot parse this shim's frames) -- `aborted` is gRPC's own code for "the operation was
/// aborted, typically due to a concurrency issue" which is the closest fit `tonic::Status`
/// offers without inventing an out-of-band signalling channel `lockstep.proto` does not
/// have. The full detail (which value mismatched what) is preserved in the status message,
/// not discarded -- `{err}`'s `Display` impl is every `ProtocolError` variant's own
/// `#[error(...)]` text, verbatim.
fn to_status(err: ProtocolError) -> Status {
    Status::aborted(err.to_string())
}

/// Generic over the peer stream type so the exact same service logic serves a real
/// `tokio::net::UnixStream` (`src/bin/av-lockstep-shim.rs`) and an in-memory
/// `tokio::net::UnixStream::pair()` half (this crate's own tests) identically.
pub struct ShimService<S> {
    peer: Arc<PeerLink<S>>,
}

impl<S> ShimService<S> {
    pub fn new(peer: Arc<PeerLink<S>>) -> Self {
        Self { peer }
    }
}

#[tonic::async_trait]
impl<S> LockstepService for ShimService<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn bind(&self, request: Request<LockstepBindRequest>) -> Result<Response<LockstepBindResponse>, Status> {
        let response = self.peer.bind(request.into_inner()).await.map_err(to_status)?;
        Ok(Response::new(response))
    }

    async fn step(&self, request: Request<LockstepStepRequest>) -> Result<Response<LockstepStepResponse>, Status> {
        let response = self.peer.step(request.into_inner()).await.map_err(to_status)?;
        Ok(Response::new(response))
    }

    async fn reset(&self, request: Request<LockstepResetRequest>) -> Result<Response<LockstepResetResponse>, Status> {
        let response = self.peer.reset(request.into_inner()).await.map_err(to_status)?;
        Ok(Response::new(response))
    }

    async fn shutdown(&self, request: Request<LockstepShutdownRequest>) -> Result<Response<LockstepShutdownResponse>, Status> {
        let response = self.peer.shutdown(request.into_inner()).await.map_err(to_status)?;
        Ok(Response::new(response))
    }
}
