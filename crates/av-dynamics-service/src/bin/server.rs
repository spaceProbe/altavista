//! Boots `av-dynamics-service`'s `DynamicsService` gRPC server. The Rust twin of
//! `services/gmat-service/gmat_service/server.py` -- see `crate::worker`'s module doc for
//! how this binary keeps the GMAT single-thread constraint under `tonic`'s multi-threaded
//! async runtime (a dedicated OS thread, not a sized-to-one thread pool the way the Python
//! `grpc.server` does it, since `tonic`'s own executor is `tokio`'s and this crate must not
//! let GMAT work land on it).
//!
//! Security note (ADR-004/ADR-003 amendment): binds plaintext gRPC on `127.0.0.1` only.
//! mTLS with seccert-issued certificates crosses a host boundary through a service-owned
//! nginx front (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`), not this
//! process -- see the crate README.
use std::net::SocketAddr;
use std::sync::Arc;

use av_dynamics_service::admin::AdminState;
use av_dynamics_service::pb::dynamics_service_server::DynamicsServiceServer;
use av_dynamics_service::{DynamicsServiceImpl, EvidenceLog, WorkerHandle};

/// Offset from `config::DEFAULT_PORT` (50062) by +100, mirroring
/// `gmat_service.config.DEFAULT_ADMIN_PORT`'s own +100 offset from ITS `DEFAULT_PORT`
/// (50061 -> 50161) -- both services' admin ports are +100 from their own gRPC port, so the
/// pairing is easy to remember without the two languages needing to agree on one shared
/// number range.
const DEFAULT_ADMIN_PORT: u16 = av_dynamics_service::config::DEFAULT_PORT + 100;

struct Args {
    port: u16,
    admin_port: u16,
    evidence_path: std::path::PathBuf,
    run_id: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut port = av_dynamics_service::config::DEFAULT_PORT;
    let mut admin_port = DEFAULT_ADMIN_PORT;
    let mut evidence_path = default_evidence_path();
    let mut run_id = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--port" => port = val()?.parse::<u16>().map_err(|e| format!("--port: {e}"))?,
            "--admin-port" => admin_port = val()?.parse::<u16>().map_err(|e| format!("--admin-port: {e}"))?,
            "--evidence-path" => evidence_path = std::path::PathBuf::from(val()?),
            "--run-id" => run_id = Some(val()?),
            "--help" | "-h" => {
                return Err("usage: av-dynamics-service [--port PORT] [--admin-port PORT] \
                            [--evidence-path PATH] [--run-id ID]"
                    .to_string())
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok(Args { port, admin_port, evidence_path, run_id })
}

/// `<repo>/crates/av-dynamics-service/evidence.jsonl` -- the same "next to the crate"
/// convention `gmat_service.config.DEFAULT_EVIDENCE_PATH` uses relative to its own package.
fn default_evidence_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evidence.jsonl")
}

fn random_run_id() -> String {
    // 16 bytes from OpenSSL's RNG, hex-encoded -- an opaque per-process run identifier
    // (the same role `uuid.uuid4().hex` plays on the Python side), without adding a `uuid`/
    // `rand` crate: `openssl` is already this crate's dependency for the evidence log's
    // SHA-256 hashing. Not a literal RFC 4122 UUID (no version/variant bits set) -- nothing
    // downstream parses it as one, only compares it for equality (evidence records, test
    // assertions), so that is not a functional gap.
    let mut buf = [0u8; 16];
    openssl::rand::rand_bytes(&mut buf).expect("OpenSSL RNG failed");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-dynamics-service: {e}");
            std::process::exit(1);
        }
    };
    let run_id = args.run_id.unwrap_or_else(random_run_id);

    // Spawn and warm up the GMAT worker thread FIRST -- literally the first gmat-sys call
    // this process ever makes happens there, before the server accepts any request
    // (mirrors gmat_service.server.serve: warm_up() runs before server.start()).
    let worker = WorkerHandle::spawn().map_err(|e| format!("GMAT warm-up failed: {e}"))?;

    let evidence = Arc::new(EvidenceLog::open(&args.evidence_path)?);
    eprintln!(
        "av-dynamics-service: warmed up (run_id={run_id}, evidence={})",
        evidence.path().display()
    );

    // Grabbed before `worker` moves into the servicer below -- `/admin/api/evidence`
    // reports the same settings_hash `Describe`/every evidence record does.
    let settings_hash = (*worker.settings_hash).clone();

    let admin_state = Arc::new(AdminState {
        evidence: evidence.clone(),
        settings_hash,
        run_id: run_id.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    });
    // Never a non-loopback address (ADR-004): same rule as the gRPC port below, and this
    // endpoint is not fronted by nginx/mTLS at all -- `secdeploy evidence` (ADR-004 question
    // 63) is expected to run on the same host.
    let admin_addr: SocketAddr = format!("127.0.0.1:{}", args.admin_port).parse()?;
    eprintln!("av-dynamics-service: admin API on {admin_addr} (GET /admin/api/evidence, /admin/api/evidence/verify)");
    tokio::spawn(async move {
        if let Err(e) = av_dynamics_service::admin::serve(admin_addr, admin_state).await {
            eprintln!("av-dynamics-service: admin server failed: {e}");
        }
    });

    let servicer = DynamicsServiceImpl::new(worker, evidence, run_id.clone());

    // Never a non-loopback address (ADR-004/ADR-003 amendment) -- see the module doc.
    let addr: SocketAddr = format!("127.0.0.1:{}", args.port).parse()?;
    eprintln!("av-dynamics-service: listening on {addr} (run_id={run_id})");

    tonic::transport::Server::builder()
        .add_service(DynamicsServiceServer::new(servicer))
        .serve(addr)
        .await?;
    Ok(())
}
