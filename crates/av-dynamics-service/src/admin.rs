//! `/admin/api/evidence` (ADR-004 question 63: "exposes `/admin/api/evidence` for
//! `secdeploy evidence` to collect"), **localhost only** -- the mTLS front door for a
//! cross-host hop is nginx's job (this crate's README, ADR-003's amendment), not this
//! endpoint's, exactly like this crate's plaintext gRPC port.
//!
//! A hand-rolled `GET`-only HTTP/1.1 server over a plain `tokio::net::TcpListener`, not
//! `axum`/`hyper` directly: those crates are only present in this workspace's dependency
//! tree *transitively* (through `tonic`'s own internals), and depending on them directly
//! here would need pinning their exact versions/features against `tonic`'s own choices for
//! no real benefit -- two routes, `GET`-only, JSON-out, is well within what
//! `tokio::net::TcpListener` (already a feature this crate enables -- `Cargo.toml`'s
//! `tokio` `"net"` feature) plus manual request-line parsing can serve correctly. This
//! mirrors the codebase's existing style of preferring what is already in the dependency
//! tree over adding a crate (`crate::worker`'s hand-written `mpsc`-based job queue instead
//! of a thread-pool crate is the same instinct).
//!
//! # Routes
//!
//! - `GET /admin/api/evidence` -- `version`, `settings_hash`, `run_id`, the evidence log's
//!   `chain_head`/`entries`, and [`crate::fips::FipsPosture`] (detected, not claimed).
//! - `GET /admin/api/evidence/verify` -- runs [`crate::evidence::EvidenceLog::verify`] and
//!   returns its [`crate::evidence::ChainVerification`] as JSON.
//!
//! Every other path/method gets `404`/`405`. Response bodies are built from a
//! `BTreeMap<&str, serde_json::Value>` (ADR-004's determinism rule: "`BTreeMap` on output
//! paths, sort explicitly") so the JSON key order is stable and independent of Rust's
//! (unspecified) struct-field iteration order for any map-shaped payload.
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::evidence::EvidenceLog;
use crate::fips;

/// Everything one `GET /admin/api/evidence` response needs. Built once at server startup
/// and shared (via `Arc`) across every accepted connection -- nothing here is per-request
/// mutable state except what `EvidenceLog` itself already guards internally.
pub struct AdminState {
    pub evidence: Arc<EvidenceLog>,
    pub settings_hash: String,
    pub run_id: String,
    /// This crate's own `CARGO_PKG_VERSION` (`env!("CARGO_PKG_VERSION")` at the call site in
    /// `src/bin/server.rs`) -- what `/admin/api/evidence`'s `"version"` field reports. Not
    /// `config::GMAT_VERSION`: that already appears inside `settings_hash`'s own inputs and
    /// inside `Describe`'s `ModelInfo.version`; `/admin/api/evidence`'s `"version"` is about
    /// *this admin surface/binary*, matching `secdeploy evidence`'s expectation of a
    /// component version, not a physics-model version.
    pub version: String,
}

fn json_response(status_line: &str, body: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(body).expect("admin response body always serializes");
    let mut resp = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    resp.extend_from_slice(&body);
    resp
}

fn evidence_body(state: &AdminState) -> Value {
    let posture = fips::detect();
    let mut m: BTreeMap<&str, Value> = BTreeMap::new();
    m.insert("chain_head", Value::String(state.evidence.chain_head()));
    m.insert("entries", Value::from(state.evidence.entry_count()));
    m.insert("evidence_path", Value::String(state.evidence.path().display().to_string()));
    m.insert("fips", serde_json::to_value(&posture).expect("FipsPosture always serializes"));
    m.insert("run_id", Value::String(state.run_id.clone()));
    m.insert("settings_hash", Value::String(state.settings_hash.clone()));
    m.insert("version", Value::String(state.version.clone()));
    serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes")
}

fn verify_body(state: &AdminState) -> std::io::Result<Value> {
    let result = state.evidence.verify()?;
    Ok(serde_json::to_value(&result).expect("ChainVerification always serializes"))
}

/// Reads exactly the request line (`"GET /path HTTP/1.1"`) and discards headers up to and
/// including the blank line -- this endpoint never reads a request body (every route is
/// `GET`-only), so nothing beyond the header block is consumed.
async fn read_request_line_and_drain_headers(stream: &mut BufReader<TcpStream>) -> std::io::Result<Option<(String, String)>> {
    let mut request_line = String::new();
    if stream.read_line(&mut request_line).await? == 0 {
        return Ok(None); // peer closed before sending anything
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    // Drain header lines through the blank line terminator; header *values* are unused (no
    // route here depends on any request header).
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await? == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
    }
    Ok(Some((method, path)))
}

async fn handle_connection(stream: TcpStream, state: Arc<AdminState>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let Some((method, path)) = read_request_line_and_drain_headers(&mut reader).await? else {
        return Ok(());
    };

    let response = if method != "GET" {
        json_response("405 Method Not Allowed", &serde_json::json!({"error": "method not allowed", "method": method}))
    } else {
        match path.as_str() {
            "/admin/api/evidence" => json_response("200 OK", &evidence_body(&state)),
            "/admin/api/evidence/verify" => match verify_body(&state) {
                Ok(body) => json_response("200 OK", &body),
                Err(e) => json_response("500 Internal Server Error", &serde_json::json!({"error": e.to_string()})),
            },
            other => json_response("404 Not Found", &serde_json::json!({"error": "not found", "path": other})),
        }
    };

    let stream = reader.into_inner();
    let mut stream = stream;
    stream.write_all(&response).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Binds `addr` and serves `/admin/api/evidence` forever (one `tokio::spawn`ed task per
/// connection; each connection is short-lived -- one request, `Connection: close`). `addr`
/// must be a loopback address (ADR-004) -- callers (`src/bin/server.rs`) are responsible
/// for never passing a non-loopback one, matching this crate's gRPC port's own contract.
pub async fn serve(addr: SocketAddr, state: Arc<AdminState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                eprintln!("av-dynamics-service admin: connection error: {e}");
            }
        });
    }
}
