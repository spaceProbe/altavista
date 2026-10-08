//! `av-edge-board`: the board's edge service. Opens the board's link, performs the
//! lockstep-local v1 handshake with the flight software (HELLO once, never retried), then
//! serves `altavista.v1.LockstepService` on loopback for the kernel. See the crate README.
//!
//! ```text
//! av-edge-board --port-device <spec> --edge-node-id <id> [--grpc-addr 127.0.0.1:<port>]
//!               [--handshake-timeout-ms <n>] [--udp-local <addr:port>]
//! ```
//!
//! Every stage is logged on stderr. One link per process; the process exits after a
//! successful Shutdown RPC, or on SIGINT/SIGTERM.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use av_edge::board::{parse_port_device, BoardLink, BoardLinkError, PortDeviceSpecError};
use av_edge_board::link::{open_link, LinkError, LinkOptions};
use av_edge_board::service::BoardService;
use av_lockstep_shim::pb::lockstep_service_server::LockstepServiceServer;
use av_lockstep_shim::{PeerLink, ProtocolError};
use tokio::signal::unix::{signal, SignalKind};

/// Question 208(c): `docs/architecture.md` section 4, "Default ports", is the one owned port map, and
/// `tests/test_port_map.py` checks this constant against its row. 50081 is the next free port after
/// `av-lockstep-shim`'s 50080; no admin surface.
const DEFAULT_GRPC_ADDR: &str = "127.0.0.1:50081";
const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 60_000;
/// How long after a Shutdown the server is given to drain before the process exits anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("{0}")]
    Usage(String),
    #[error("--port-device: {0}")]
    PortDevice(#[from] PortDeviceSpecError),
    #[error("--edge-node-id: {0}")]
    EdgeNode(#[from] BoardLinkError),
    #[error("--grpc-addr {addr:?} is not a valid HOST:PORT: {detail}")]
    BadGrpcAddr { addr: String, detail: String },
    #[error("--grpc-addr {0} is not a loopback address: this service only ever binds loopback (ADR-004)")]
    NonLoopback(SocketAddr),
    #[error("opening the board link: {0}")]
    Link(#[from] LinkError),
    #[error("the lockstep-local v1 handshake did not complete within {after_ms} ms (HELLO is sent once and never retried; is the board up and its end of the link open before this service starts?)")]
    HandshakeTimeout { after_ms: u64 },
    #[error("lockstep-local v1 handshake failed: {0}")]
    Handshake(#[from] ProtocolError),
    #[error("serving LockstepService: {0}")]
    Serve(String),
}

struct Args {
    port_device: String,
    edge_node_id: String,
    grpc_addr: String,
    handshake_timeout_ms: u64,
    udp_local: Option<SocketAddr>,
}

const USAGE: &str = "usage: av-edge-board --port-device <spec> --edge-node-id <id> [--grpc-addr 127.0.0.1:PORT] [--handshake-timeout-ms N] [--udp-local ADDR:PORT]\n  <spec> is /dev/<name>@<baud> (serial, 8N1) or udp://<host>:<port>";

fn parse_args() -> Result<Args, StartupError> {
    let mut port_device = None;
    let mut edge_node_id = None;
    let mut grpc_addr = DEFAULT_GRPC_ADDR.to_string();
    let mut handshake_timeout_ms = DEFAULT_HANDSHAKE_TIMEOUT_MS;
    let mut udp_local = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| StartupError::Usage(format!("{flag} needs a value\n{USAGE}")));
        match flag.as_str() {
            "--port-device" => port_device = Some(val()?),
            "--edge-node-id" => edge_node_id = Some(val()?),
            "--grpc-addr" => grpc_addr = val()?,
            "--handshake-timeout-ms" => {
                let v = val()?;
                handshake_timeout_ms = v.parse().map_err(|e| StartupError::Usage(format!("--handshake-timeout-ms {v:?} is not a number of milliseconds: {e}")))?;
            }
            "--udp-local" => {
                let v = val()?;
                udp_local = Some(v.parse().map_err(|e| StartupError::Usage(format!("--udp-local {v:?} is not a valid ADDR:PORT: {e}")))?);
            }
            "--help" | "-h" => return Err(StartupError::Usage(USAGE.to_string())),
            other => return Err(StartupError::Usage(format!("unrecognized argument: {other}\n{USAGE}"))),
        }
    }
    Ok(Args {
        port_device: port_device.ok_or_else(|| StartupError::Usage(format!("--port-device is required\n{USAGE}")))?,
        edge_node_id: edge_node_id.ok_or_else(|| StartupError::Usage(format!("--edge-node-id is required\n{USAGE}")))?,
        grpc_addr,
        handshake_timeout_ms,
        udp_local,
    })
}

async fn run() -> Result<(), StartupError> {
    let args = parse_args()?;
    let device = parse_port_device(&args.port_device)?;
    let link = BoardLink::single(&args.edge_node_id, device.clone())?;
    let addr: SocketAddr = args.grpc_addr.parse().map_err(|e: std::net::AddrParseError| StartupError::BadGrpcAddr { addr: args.grpc_addr.clone(), detail: e.to_string() })?;
    if !addr.ip().is_loopback() {
        return Err(StartupError::NonLoopback(addr));
    }
    eprintln!("av-edge-board: edge node {:?}, port device {} (link config hash {})", link.edge_node_id(), device, link.config_hash_hex());

    let stream = open_link(&device, &LinkOptions { udp_local: args.udp_local }).await?;
    let udp_stats = stream.udp_stats();
    eprintln!("av-edge-board: link open, performing the lockstep-local v1 handshake (timeout {} ms)", args.handshake_timeout_ms);
    let peer = tokio::time::timeout(Duration::from_millis(args.handshake_timeout_ms), PeerLink::handshake(stream))
        .await
        .map_err(|_| StartupError::HandshakeTimeout { after_ms: args.handshake_timeout_ms })??;
    eprintln!("av-edge-board: handshake complete");

    let service = BoardService::new(Arc::new(peer), link.edge_node_id().to_string(), device);
    let shutdown_after_rpc = service.shutdown_signal();
    eprintln!("av-edge-board: LockstepService listening on {addr}");

    let mut sigterm = signal(SignalKind::terminate()).map_err(|e| StartupError::Serve(e.to_string()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(LockstepServiceServer::new(service))
            .serve_with_shutdown(addr, async {
                let _ = done_rx.await;
            })
            .await
    });
    tokio::pin!(server);
    tokio::select! {
        r = &mut server => {
            return r.map_err(|e| StartupError::Serve(e.to_string()))?.map_err(|e| StartupError::Serve(e.to_string()));
        }
        _ = shutdown_after_rpc.notified() => eprintln!("av-edge-board: draining after Shutdown"),
        _ = tokio::signal::ctrl_c() => eprintln!("av-edge-board: SIGINT, stopping"),
        _ = sigterm.recv() => eprintln!("av-edge-board: SIGTERM, stopping"),
    }
    let _ = done_tx.send(());
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, &mut server).await;
    if let Some(stats) = udp_stats {
        let (accepted, foreign, malformed, sent) = stats.snapshot();
        eprintln!("av-edge-board: UDP datagrams: {accepted} accepted, {sent} sent, {foreign} dropped from foreign addresses, {malformed} malformed");
    }
    eprintln!("av-edge-board: stopped");
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("av-edge-board: {e}");
        std::process::exit(if matches!(e, StartupError::Usage(_)) { 2 } else { 1 });
    }
}
