//! `av-edge-board`: the board's edge service. Opens the board's link, performs the
//! lockstep-local v1 handshake with the flight software (HELLO once at startup, never retried),
//! then serves `altavista.v1.LockstepService` on loopback for the kernel. See the crate README.
//!
//! ```text
//! av-edge-board --port-device <spec> --edge-node-id <id> [--grpc-addr 127.0.0.1:<port>]
//!               [--handshake-timeout-ms <n>] [--udp-local <addr:port>]
//!               --io-log <path> --signing-key <pem> --signing-cert <pem>
//!               [--power-control cmd:/abs/path] [--power-timeout-ms <n>]
//!               [--power-cycle-reboots-peer [--rehandshake-interval-ms <n>] [--rehandshake-timeout-ms <n>]]
//! ```
//!
//! `--power-control` is this edge node's power control channel (question 242 (c)): `cmd:<absolute
//! path>` runs that executable on this node when the kernel asks for a power cycle over
//! `altavista.v1.BoardEdgeService`, served on the same gRPC address as `LockstepService`; absent
//! means no channel (every request is refused, nothing run). `gpio://...` is reserved and refused
//! at startup. See `power.rs`.
//!
//! `--power-cycle-reboots-peer` declares that the power channel reboots the flight software (a real
//! board always would; the existing stand-ins' power cycles do not): after a power cycle that
//! performed, the service re-handshakes, re-sending HELLO every `--rehandshake-interval-ms` until
//! the peer answers or `--rehandshake-timeout-ms` passes. See `relink.rs`.
//!
//! Every stage is logged on stderr. One link per process; the process exits after a
//! successful Shutdown RPC, or on SIGINT/SIGTERM.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use av_edge::board::{parse_port_device, parse_power_control, BoardLink, BoardLinkError, PortDeviceSpecError, PowerControl, PowerControlSpecError};
use av_edge::board_log::{BoardIoLogWriter, BoardLogError, LogSigner};
use av_edge_board::iolog::BoardIoLog;
use av_edge_board::timed::{LinkTimes, TimedStream};
use av_edge_board::link::{open_link, LinkError, LinkOptions};
use av_edge_board::power::{BoardEdge, BoardEdgeServiceServer, DEFAULT_POWER_TIMEOUT_MS, POWER_TIMEOUT_BOUNDS_MS};
use av_edge_board::relink::{PeerCell, PeerRelink, RelinkConfig, RelinkSetup, SharedStream};
use av_edge_board::service::BoardService;
use av_lockstep_shim::pb::lockstep_service_server::LockstepServiceServer;
use av_lockstep_shim::{PeerLink, ProtocolError};
use tokio::signal::unix::{signal, SignalKind};

/// Question 208(c): `docs/architecture.md` section 4, "Default ports", is the one owned port map, and
/// `tests/test_port_map.py` checks this constant against its row. 50081 is the next free port after
/// `av-lockstep-shim`'s 50080; no admin surface.
const DEFAULT_GRPC_ADDR: &str = "127.0.0.1:50081";
const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 60_000;
/// Between re-sent HELLOs. A guest answers a HELLO in well under 100 ms (measured for the cFS
/// container, see the crate README), so one second is far longer than the answer, and short enough
/// that a guest which comes up at an arbitrary moment waits at most a second for its HELLO.
const DEFAULT_REHANDSHAKE_INTERVAL_MS: u64 = 1_000;
const REHANDSHAKE_INTERVAL_BOUNDS_MS: std::ops::RangeInclusive<u64> = 100..=60_000;
/// From the first re-sent HELLO to giving up: a container restart or a board boot, with margin.
const DEFAULT_REHANDSHAKE_TIMEOUT_MS: u64 = 60_000;
const REHANDSHAKE_TIMEOUT_BOUNDS_MS: std::ops::RangeInclusive<u64> = 200..=600_000;
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
    #[error("--power-control: {0}")]
    PowerControl(#[from] PowerControlSpecError),
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
    #[error("{flag} {path}: {source}")]
    ReadIdentity { flag: &'static str, path: String, source: std::io::Error },
    #[error("the board I/O log: {0}")]
    IoLog(#[from] BoardLogError),
}

/// Removes the I/O log at drop if nothing was ever written to it, so a startup that fails
/// after the file was created (a handshake timeout while the board is being brought up, say)
/// does not leave an empty file that would make the next start refuse "log already exists".
/// A log with any record in it is never touched.
struct EmptyLogCleanup(PathBuf);

impl Drop for EmptyLogCleanup {
    fn drop(&mut self) {
        if std::fs::metadata(&self.0).map(|m| m.len() == 0).unwrap_or(false) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

struct Args {
    port_device: String,
    edge_node_id: String,
    grpc_addr: String,
    handshake_timeout_ms: u64,
    udp_local: Option<SocketAddr>,
    io_log: PathBuf,
    signing_key: PathBuf,
    signing_cert: PathBuf,
    power_control: String,
    power_timeout_ms: u64,
    power_cycle_reboots_peer: bool,
    rehandshake_interval_ms: u64,
    rehandshake_timeout_ms: u64,
}

const USAGE: &str = "usage: av-edge-board --port-device <spec> --edge-node-id <id> [--grpc-addr 127.0.0.1:PORT] [--handshake-timeout-ms N] [--udp-local ADDR:PORT] --io-log PATH --signing-key PEM --signing-cert PEM [--power-control cmd:/ABS/PATH] [--power-timeout-ms N] [--power-cycle-reboots-peer [--rehandshake-interval-ms N] [--rehandshake-timeout-ms N]]\n  <spec> is /dev/<name>@<baud> (serial, 8N1) or udp://<host>:<port>\n  --io-log/--signing-key/--signing-cert are required: the board's I/O is logged durably as signed, hash-chained records, and a run without that log is refused (the log must not exist yet; the key and certificate are the edge node's P-384 identity)\n  --power-control is this edge node's power control channel, run here when the kernel asks for a power cycle: cmd:<absolute path> (no shell; invoked as `<path> power-cycle --edge-node-id ID --instance NAME --fault-id ID --tai-ns N`), or absent for none (gpio://... is reserved); --power-timeout-ms bounds one run (default 30000, 10..=600000)\n  --power-cycle-reboots-peer declares that the power channel reboots the flight software (needs --power-control): after a power cycle that performed, HELLO is re-sent every --rehandshake-interval-ms (default 1000, 100..=60000, not above the timeout) until the peer answers or --rehandshake-timeout-ms (default 60000, 200..=600000) passes";

fn parse_args() -> Result<Args, StartupError> {
    let mut port_device = None;
    let mut edge_node_id = None;
    let mut grpc_addr = DEFAULT_GRPC_ADDR.to_string();
    let mut handshake_timeout_ms = DEFAULT_HANDSHAKE_TIMEOUT_MS;
    let mut udp_local = None;
    let mut io_log = None;
    let mut signing_key = None;
    let mut signing_cert = None;
    let mut power_control = String::new();
    let mut power_timeout_ms = DEFAULT_POWER_TIMEOUT_MS;
    let mut power_cycle_reboots_peer = false;
    let mut rehandshake_interval_ms = None;
    let mut rehandshake_timeout_ms = None;
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
            "--io-log" => io_log = Some(PathBuf::from(val()?)),
            "--signing-key" => signing_key = Some(PathBuf::from(val()?)),
            "--signing-cert" => signing_cert = Some(PathBuf::from(val()?)),
            "--power-control" => power_control = val()?,
            "--power-timeout-ms" => {
                let v = val()?;
                power_timeout_ms = v.parse().map_err(|e| StartupError::Usage(format!("--power-timeout-ms {v:?} is not a number of milliseconds: {e}")))?;
                if !POWER_TIMEOUT_BOUNDS_MS.contains(&power_timeout_ms) {
                    return Err(StartupError::Usage(format!("--power-timeout-ms {power_timeout_ms} is outside {}..={}", POWER_TIMEOUT_BOUNDS_MS.start(), POWER_TIMEOUT_BOUNDS_MS.end())));
                }
            }
            "--power-cycle-reboots-peer" => power_cycle_reboots_peer = true,
            "--rehandshake-interval-ms" => {
                let v = val()?;
                let n: u64 = v.parse().map_err(|e| StartupError::Usage(format!("--rehandshake-interval-ms {v:?} is not a number of milliseconds: {e}")))?;
                if !REHANDSHAKE_INTERVAL_BOUNDS_MS.contains(&n) {
                    return Err(StartupError::Usage(format!("--rehandshake-interval-ms {n} is outside {}..={}", REHANDSHAKE_INTERVAL_BOUNDS_MS.start(), REHANDSHAKE_INTERVAL_BOUNDS_MS.end())));
                }
                rehandshake_interval_ms = Some(n);
            }
            "--rehandshake-timeout-ms" => {
                let v = val()?;
                let n: u64 = v.parse().map_err(|e| StartupError::Usage(format!("--rehandshake-timeout-ms {v:?} is not a number of milliseconds: {e}")))?;
                if !REHANDSHAKE_TIMEOUT_BOUNDS_MS.contains(&n) {
                    return Err(StartupError::Usage(format!("--rehandshake-timeout-ms {n} is outside {}..={}", REHANDSHAKE_TIMEOUT_BOUNDS_MS.start(), REHANDSHAKE_TIMEOUT_BOUNDS_MS.end())));
                }
                rehandshake_timeout_ms = Some(n);
            }
            "--help" | "-h" => return Err(StartupError::Usage(USAGE.to_string())),
            other => return Err(StartupError::Usage(format!("unrecognized argument: {other}\n{USAGE}"))),
        }
    }
    if !power_cycle_reboots_peer && (rehandshake_interval_ms.is_some() || rehandshake_timeout_ms.is_some()) {
        return Err(StartupError::Usage(format!("--rehandshake-interval-ms / --rehandshake-timeout-ms only apply with --power-cycle-reboots-peer\n{USAGE}")));
    }
    if power_cycle_reboots_peer && power_control.is_empty() {
        return Err(StartupError::Usage(format!("--power-cycle-reboots-peer declares what the power channel does, so it needs --power-control\n{USAGE}")));
    }
    let rehandshake_interval_ms = rehandshake_interval_ms.unwrap_or(DEFAULT_REHANDSHAKE_INTERVAL_MS);
    let rehandshake_timeout_ms = rehandshake_timeout_ms.unwrap_or(DEFAULT_REHANDSHAKE_TIMEOUT_MS);
    if power_cycle_reboots_peer && rehandshake_interval_ms > rehandshake_timeout_ms {
        return Err(StartupError::Usage(format!("--rehandshake-interval-ms {rehandshake_interval_ms} is longer than --rehandshake-timeout-ms {rehandshake_timeout_ms}: not even one re-sent HELLO would get its interval\n{USAGE}")));
    }
    Ok(Args {
        port_device: port_device.ok_or_else(|| StartupError::Usage(format!("--port-device is required\n{USAGE}")))?,
        edge_node_id: edge_node_id.ok_or_else(|| StartupError::Usage(format!("--edge-node-id is required\n{USAGE}")))?,
        grpc_addr,
        handshake_timeout_ms,
        udp_local,
        io_log: io_log.ok_or_else(|| StartupError::Usage(format!("--io-log is required: a board run without a durable, signed I/O log is refused\n{USAGE}")))?,
        signing_key: signing_key.ok_or_else(|| StartupError::Usage(format!("--signing-key is required: the board's I/O log is signed\n{USAGE}")))?,
        signing_cert: signing_cert.ok_or_else(|| StartupError::Usage(format!("--signing-cert is required: the board's I/O log names its signer by certificate\n{USAGE}")))?,
        power_control,
        power_timeout_ms,
        power_cycle_reboots_peer,
        rehandshake_interval_ms,
        rehandshake_timeout_ms,
    })
}

async fn run() -> Result<(), StartupError> {
    let args = parse_args()?;
    let device = parse_port_device(&args.port_device)?;
    let link = BoardLink::single(&args.edge_node_id, device.clone())?;
    let power_control: PowerControl = parse_power_control(&args.power_control)?;
    let addr: SocketAddr = args.grpc_addr.parse().map_err(|e: std::net::AddrParseError| StartupError::BadGrpcAddr { addr: args.grpc_addr.clone(), detail: e.to_string() })?;
    if !addr.ip().is_loopback() {
        return Err(StartupError::NonLoopback(addr));
    }
    eprintln!("av-edge-board: edge node {:?}, port device {} (link config hash {})", link.edge_node_id(), device, link.config_hash_hex());
    if power_control.is_none() {
        eprintln!("av-edge-board: no power control channel (--power-control absent): every PowerCycle request will be refused and nothing run");
    } else {
        eprintln!("av-edge-board: power control channel {power_control} (timeout {} ms)", args.power_timeout_ms);
        if args.power_cycle_reboots_peer {
            eprintln!(
                "av-edge-board: the power channel is declared to reboot the peer: after a power cycle that performed, HELLO is re-sent every {} ms until the peer answers or {} ms pass",
                args.rehandshake_interval_ms, args.rehandshake_timeout_ms
            );
        }
    }

    // The board's I/O log (question 242 (a)): signer first, then the log file, both before the
    // board is touched, so a missing key or an existing log refuses the run up front.
    let read = |flag: &'static str, path: &PathBuf| std::fs::read(path).map_err(|source| StartupError::ReadIdentity { flag, path: path.display().to_string(), source });
    let signer = LogSigner::from_pem(&read("--signing-key", &args.signing_key)?, &read("--signing-cert", &args.signing_cert)?)?;
    eprintln!("av-edge-board: signing the I/O log as certificate {}", signer.cert_sha256());
    let writer = BoardIoLogWriter::create(&args.io_log, link.edge_node_id(), &link.config_hash_hex(), signer)?;
    let _empty_log_cleanup = EmptyLogCleanup(args.io_log.clone());
    eprintln!("av-edge-board: board I/O log created at {}", args.io_log.display());
    let times = Arc::new(LinkTimes::default());
    let log = Arc::new(BoardIoLog::new(writer, Arc::clone(&times)));

    let stream = open_link(&device, &LinkOptions { udp_local: args.udp_local }).await?;
    let udp_stats = stream.udp_stats();
    // One shared handle on the link, so a power cycle's re-handshake can build a fresh `PeerLink`
    // on it (relink.rs). The first handshake uses it too, so there is one code path.
    let stream = SharedStream::new(TimedStream::new(stream, times));
    eprintln!("av-edge-board: link open, performing the lockstep-local v1 handshake (timeout {} ms)", args.handshake_timeout_ms);
    let hello_sent_at = std::time::Instant::now();
    let peer = tokio::time::timeout(Duration::from_millis(args.handshake_timeout_ms), PeerLink::handshake(stream.clone()))
        .await
        .map_err(|_| StartupError::HandshakeTimeout { after_ms: args.handshake_timeout_ms })??;
    eprintln!("av-edge-board: handshake complete ({} ms from HELLO sent to the guest's answer read)", hello_sent_at.elapsed().as_millis());

    let cell = if args.power_cycle_reboots_peer {
        let discard = |s: &SharedStream<TimedStream<av_edge_board::link::BoardStream>>| s.with(|timed| timed.inner_mut().discard_pending());
        let setup = RelinkSetup {
            stream: stream.clone(),
            discard: Box::new(discard),
            config: RelinkConfig { interval: Duration::from_millis(args.rehandshake_interval_ms), timeout: Duration::from_millis(args.rehandshake_timeout_ms) },
        };
        Arc::new(PeerCell::with_relink(Arc::new(peer), setup))
    } else {
        Arc::new(PeerCell::new(Arc::new(peer)))
    };
    let service = BoardService::with_cell(Arc::clone(&cell), link.edge_node_id().to_string(), device, Arc::clone(&log));
    let shutdown_after_rpc = service.shutdown_signal();
    let mut board_edge = BoardEdge::new(link.edge_node_id().to_string(), power_control, Duration::from_millis(args.power_timeout_ms), Arc::clone(&log));
    if args.power_cycle_reboots_peer {
        board_edge = board_edge.with_relink(Arc::clone(&cell) as Arc<dyn PeerRelink>);
    }
    eprintln!("av-edge-board: BoardEdgeService (PowerCycle) served on the same address");
    eprintln!("av-edge-board: LockstepService listening on {addr}");

    let mut sigterm = signal(SignalKind::terminate()).map_err(|e| StartupError::Serve(e.to_string()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(LockstepServiceServer::new(service))
            .add_service(BoardEdgeServiceServer::new(board_edge))
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
    eprintln!("av-edge-board: board I/O log {}: {} records, chain head {}", args.io_log.display(), log.records_written(), log.chain_head_hex());
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
