//! `BoardService`: the `altavista.v1.LockstepService` the kernel dials, which is
//! `av_lockstep_shim::service::ShimService` plus the Bind-time board check.
//!
//! # The Bind-time check
//!
//! When the kernel binds a `BINDING_KIND_BOARD` instance it sets two entries in
//! `LockstepBindRequest.parameters` (the next task; the names are `av_edge::board`'s
//! [`BIND_PARAM_EDGE_NODE_ID`] and [`BIND_PARAM_PORT_DEVICE`]): the edge node and the
//! canonical port device it believes it is binding. This service compares them with its own
//! configuration, the device in canonical form ([`PortDevice::canonical`]), so
//! `udp://HOST:5000` and `udp://host:5000` agree.
//!
//! - **Both present and equal:** both entries are removed, then the request is forwarded to
//!   the board. The guest's BIND bytes are what the container path sends today (the
//!   instance's other parameters, untouched); the board-specific entries never reach it.
//! - **Present and different, or only one of the two present, or the device not parseable:**
//!   refused with a `LockstepBindResponse { lockstep_capable: false, refusal_reason }`
//!   naming both the requested and the configured values, and **nothing is forwarded to the
//!   board**. This is the refusal shape the kernel's container client already treats as a
//!   typed `ContainerRefused` (`av-kernel`'s `materialize_container`), unlike a gRPC error,
//!   which it reports as a failed Bind.
//! - **Both absent:** forwarded unchanged with a warning line on stderr (the kernel side
//!   that sets them is the next task).
//!
//! Step, Reset and Shutdown are forwarded as the shim does. A successful Shutdown also
//! signals the process to exit (see [`BoardService::shutdown_signal`]): one link per
//! process, and the guest has left its loop.
//!
//! # The board I/O log (question 242 (a))
//!
//! Every exchange that crosses the link (Bind as forwarded, Step, Reset, Shutdown) is
//! recorded as a signed, hash-chained `BoardIoRecord` by [`crate::iolog::BoardIoLog`], and
//! the record is on disk (written and `fsync`ed) **before the reply is returned to the
//! kernel**: the service performs the exchange, appends, and only then returns the board's
//! response (or the error status of a failed exchange, which is logged too, with its
//! `error`). If the append fails the kernel gets `DATA_LOSS` instead of the response and
//! the log is poisoned, so no later exchange reaches the board. A Bind refused by the
//! board-parameter check never crosses the link and is not logged. The initial HELLO
//! handshake is link setup, not part of a run, and is not logged.
use std::collections::BTreeMap;
use std::sync::Arc;

use av_edge::board::{parse_port_device, PortDevice, BIND_PARAM_EDGE_NODE_ID, BIND_PARAM_PORT_DEVICE};
use av_edge::board_log::{BoardIoKind, BoardIoRecord};
use av_lockstep_shim::pb::lockstep_service_server::LockstepService;
use av_lockstep_shim::pb::{LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownRequest, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse};
use av_lockstep_shim::service::ShimService;
use av_lockstep_shim::PeerLink;
use crate::iolog::BoardIoLog;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tonic::{Request, Response, Status};

/// What the Bind-time check decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindCheck {
    /// Both parameters present and equal; they have been removed from the map.
    Matched,
    /// Neither parameter present; the map is untouched.
    Absent,
    /// Refuse with this reason; the map is untouched.
    Refused(String),
}

/// Compare the board parameters in `parameters` with this service's configuration, and strip
/// them on a match. Pure: no I/O.
pub fn check_bind(own_edge_node_id: &str, own_device: &PortDevice, parameters: &mut BTreeMap<String, String>) -> BindCheck {
    let want_node = parameters.get(BIND_PARAM_EDGE_NODE_ID).cloned();
    let want_device = parameters.get(BIND_PARAM_PORT_DEVICE).cloned();
    let own_canonical = own_device.canonical();
    let describe = |node: &Option<String>, device: &Option<String>| format!("{BIND_PARAM_EDGE_NODE_ID}={:?}, {BIND_PARAM_PORT_DEVICE}={:?}", node.as_deref().unwrap_or("<absent>"), device.as_deref().unwrap_or("<absent>"));
    let refusal = |why: String| {
        BindCheck::Refused(format!(
            "board link mismatch ({why}): the kernel bound {}, but this av-edge-board serves {BIND_PARAM_EDGE_NODE_ID}={own_edge_node_id:?}, {BIND_PARAM_PORT_DEVICE}={own_canonical:?}",
            describe(&want_node, &want_device)
        ))
    };
    match (&want_node, &want_device) {
        (None, None) => BindCheck::Absent,
        (Some(_), None) | (None, Some(_)) => refusal("only one of the two board parameters is present".to_string()),
        (Some(node), Some(device)) => {
            let device_canonical = match parse_port_device(device) {
                Ok(d) => d.canonical(),
                Err(e) => return refusal(format!("the requested port device does not parse: {e}")),
            };
            let mut diffs = Vec::new();
            if node != own_edge_node_id {
                diffs.push("edge node id differs");
            }
            if device_canonical != own_canonical {
                diffs.push("port device differs");
            }
            if !diffs.is_empty() {
                return refusal(diffs.join(", "));
            }
            parameters.remove(BIND_PARAM_EDGE_NODE_ID);
            parameters.remove(BIND_PARAM_PORT_DEVICE);
            BindCheck::Matched
        }
    }
}

/// The kernel-facing service for one board link.
pub struct BoardService<S> {
    inner: ShimService<S>,
    edge_node_id: String,
    device: PortDevice,
    shutdown: Arc<Notify>,
    log: Arc<BoardIoLog>,
}

impl<S> BoardService<S> {
    /// `log` is the board I/O log every exchange is recorded in before its reply returns.
    pub fn new(peer: Arc<PeerLink<S>>, edge_node_id: String, device: PortDevice, log: Arc<BoardIoLog>) -> Self {
        Self { inner: ShimService::new(peer), edge_node_id, device, shutdown: Arc::new(Notify::new()), log }
    }

    /// Notified (once, with a stored permit) after a Shutdown RPC succeeded.
    pub fn shutdown_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.shutdown)
    }
}

/// The error text a failed exchange is logged with.
fn error_text(status: &Status) -> String {
    format!("{:?}: {}", status.code(), status.message())
}

#[tonic::async_trait]
impl<S> LockstepService for BoardService<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn bind(&self, request: Request<LockstepBindRequest>) -> Result<Response<LockstepBindResponse>, Status> {
        let mut req = request.into_inner();
        match check_bind(&self.edge_node_id, &self.device, &mut req.parameters) {
            BindCheck::Refused(reason) => {
                eprintln!("av-edge-board: Bind refused, nothing forwarded to the board: {reason}");
                return Ok(Response::new(LockstepBindResponse { lockstep_capable: false, binding_hash: String::new(), version: String::new(), refusal_reason: reason }));
            }
            BindCheck::Matched => eprintln!("av-edge-board: Bind board parameters match this link; stripped them and forwarding the BIND to the board"),
            BindCheck::Absent => eprintln!("av-edge-board: WARNING: Bind carries no {BIND_PARAM_EDGE_NODE_ID}/{BIND_PARAM_PORT_DEVICE}; forwarding unchecked"),
        }
        let exchange = self.log.begin().await?;
        self.log.set_run(&req.run_id, &req.instance);
        let mut draft = BoardIoRecord { kind: BoardIoKind::Bind as i32, run_id: req.run_id.clone(), instance: req.instance.clone(), bind_request: Some(req.clone()), ..Default::default() };
        let result = self.inner.bind(Request::new(req)).await;
        match &result {
            Ok(r) => draft.bind_response = Some(r.get_ref().clone()),
            Err(status) => draft.error = error_text(status),
        }
        // The record is durable before the reply is returned.
        self.log.record(&exchange, draft).await?;
        result
    }

    async fn step(&self, request: Request<LockstepStepRequest>) -> Result<Response<LockstepStepResponse>, Status> {
        let exchange = self.log.begin().await?;
        let req = request.into_inner();
        let mut draft = BoardIoRecord { kind: BoardIoKind::Step as i32, lockstep_sequence: req.sequence, until_tai_ns: req.until_tai_ns, inputs: req.inputs.clone(), ..Default::default() };
        let result = self.inner.step(Request::new(req)).await;
        match &result {
            Ok(r) => {
                draft.outputs = r.get_ref().outputs.clone();
                draft.named_outputs = r.get_ref().named_outputs.clone();
            }
            Err(status) => draft.error = error_text(status),
        }
        // The record is durable before the reply is returned.
        self.log.record(&exchange, draft).await?;
        result
    }

    async fn reset(&self, request: Request<LockstepResetRequest>) -> Result<Response<LockstepResetResponse>, Status> {
        let exchange = self.log.begin().await?;
        let req = request.into_inner();
        let mut draft = BoardIoRecord { kind: BoardIoKind::Reset as i32, lockstep_sequence: req.sequence, reset_tai_ns: req.tai_ns, reset_reason: req.reason.clone(), ..Default::default() };
        let result = self.inner.reset(Request::new(req)).await;
        if let Err(status) = &result {
            draft.error = error_text(status);
        }
        self.log.record(&exchange, draft).await?;
        result
    }

    async fn shutdown(&self, request: Request<LockstepShutdownRequest>) -> Result<Response<LockstepShutdownResponse>, Status> {
        let exchange = self.log.begin().await?;
        let req = request.into_inner();
        let mut draft = BoardIoRecord { kind: BoardIoKind::Shutdown as i32, run_id: req.run_id.clone(), ..Default::default() };
        let result = self.inner.shutdown(Request::new(req)).await;
        if let Err(status) = &result {
            draft.error = error_text(status);
        }
        let logged = self.log.record(&exchange, draft).await;
        if result.is_ok() {
            // The guest has left its loop whether or not the record could be written; the
            // process exits once the reply (or the log failure) has been sent.
            eprintln!("av-edge-board: Shutdown acknowledged by the board; exiting once the reply is sent");
            self.shutdown.notify_one();
        }
        logged?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn own() -> PortDevice {
        parse_port_device("udp://board.lab:5000").unwrap()
    }

    #[test]
    fn matching_parameters_are_stripped_and_the_rest_kept() {
        let mut p = params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-7"), (BIND_PARAM_PORT_DEVICE, "udp://BOARD.lab:5000"), ("gain", "2.5")]);
        assert_eq!(check_bind("edge-7", &own(), &mut p), BindCheck::Matched);
        assert_eq!(p, params(&[("gain", "2.5")]));
    }

    #[test]
    fn absent_parameters_are_forwarded_untouched() {
        let mut p = params(&[("gain", "2.5")]);
        assert_eq!(check_bind("edge-7", &own(), &mut p), BindCheck::Absent);
        assert_eq!(p, params(&[("gain", "2.5")]));
    }

    #[test]
    fn every_mismatch_is_refused_naming_both_values_and_leaves_the_map_alone() {
        let cases = [
            params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-8"), (BIND_PARAM_PORT_DEVICE, "udp://board.lab:5000")]),
            params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-7"), (BIND_PARAM_PORT_DEVICE, "udp://board.lab:5001")]),
            params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-7"), (BIND_PARAM_PORT_DEVICE, "/dev/ttyUSB0@115200")]),
            params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-7"), (BIND_PARAM_PORT_DEVICE, "not a device")]),
            params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-7")]),
            params(&[(BIND_PARAM_PORT_DEVICE, "udp://board.lab:5000")]),
        ];
        for original in cases {
            let mut p = original.clone();
            let BindCheck::Refused(reason) = check_bind("edge-7", &own(), &mut p) else { panic!("{original:?} must be refused") };
            assert_eq!(p, original, "a refusal must not touch the parameters");
            assert!(reason.contains("udp://board.lab:5000") && reason.contains("\"edge-7\""), "the reason names this service's values: {reason}");
            for v in original.values() {
                assert!(reason.contains(v.as_str()), "the reason names the requested value {v:?}: {reason}");
            }
        }
    }
}
