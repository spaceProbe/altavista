//! Boots the M23.1 lockstep shim: listens for exactly one flight-software peer connection
//! on a Unix socket, performs the lockstep-local v1 handshake, then hosts
//! `altavista.v1.LockstepService` (plaintext, loopback -- see the crate's `lib.rs` module
//! doc comment's "What is not implemented in this batch" section for the TLS gap) for the
//! kernel to dial exactly the way it already dials `services/lockstep-ref`.
//!
//! One peer connection per process lifetime is a deliberate simplification, not an
//! oversight: a real deployment starts a fresh shim (and a fresh bound flight-software
//! process) per container/run, matching `Bind`'s own one-shot-per-process contract
//! (`lockstep.proto`'s `Reset` exists precisely so a *bound* process can be power-cycled
//! without a fresh `Bind`) -- there is no scenario in this batch's scope where one shim
//! process needs to serve a second, independent peer after the first disconnects.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use av_lockstep_shim::pb::lockstep_service_server::LockstepServiceServer;
use av_lockstep_shim::service::ShimService;
use av_lockstep_shim::PeerLink;
use tokio::net::UnixListener;

struct Args {
    grpc_addr: String,
    socket_path: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut grpc_addr = "127.0.0.1:50080".to_string();
    let mut socket_path = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--grpc-addr" => grpc_addr = val()?,
            "--socket-path" => socket_path = Some(PathBuf::from(val()?)),
            "--help" | "-h" => return Err("usage: av-lockstep-shim --socket-path PATH [--grpc-addr HOST:PORT]".to_string()),
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    let socket_path = socket_path.ok_or_else(|| "--socket-path is required".to_string())?;
    Ok(Args { grpc_addr, socket_path })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-lockstep-shim: {e}");
            std::process::exit(1);
        }
    };

    // A stale socket file from a previous, uncleanly-terminated run must not make `bind`
    // fail with "address in use" -- remove it first. Only ever a plain `remove_file`
    // (never `remove_dir_all` or a glob): this is exactly the one path the caller named,
    // nothing else.
    if args.socket_path.exists() {
        std::fs::remove_file(&args.socket_path).map_err(|e| format!("removing stale socket {}: {e}", args.socket_path.display()))?;
    }
    let listener = UnixListener::bind(&args.socket_path).map_err(|e| format!("binding Unix socket {}: {e}", args.socket_path.display()))?;
    eprintln!("av-lockstep-shim: listening for the flight-software peer on {}", args.socket_path.display());

    let (stream, _addr) = listener.accept().await.map_err(|e| format!("accepting the flight-software peer connection: {e}"))?;
    eprintln!("av-lockstep-shim: peer connected, performing the lockstep-local v1 handshake");
    let peer = PeerLink::handshake(stream).await.map_err(|e| format!("lockstep-local v1 handshake failed: {e}"))?;
    eprintln!("av-lockstep-shim: handshake complete");

    let servicer = ShimService::new(Arc::new(peer));

    // Never a non-loopback address (ADR-004, matching av-dynamics-service's own rule) --
    // the kernel's `BINDING_KIND_CONTAINER` executor always publishes/connects a bound
    // container process over loopback (services/lockstep-ref/README.md).
    let addr: SocketAddr = args.grpc_addr.parse().map_err(|e| format!("{:?} is not a valid HOST:PORT: {e}", args.grpc_addr))?;
    eprintln!("av-lockstep-shim: LockstepService listening on {addr}");

    tonic::transport::Server::builder().add_service(LockstepServiceServer::new(servicer)).serve(addr).await?;
    Ok(())
}
