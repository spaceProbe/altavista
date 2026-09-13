//! `/admin/api/evidence` (ADR-004 question 63: "exposes `/admin/api/evidence` for
//! `secdeploy evidence` to collect"), **localhost only** -- the mTLS front door for a
//! cross-host hop is nginx's job (question 155/202), not this endpoint's, exactly like
//! this crate's plaintext gRPC port ([`crate::server`]).
//!
//! A hand-rolled `GET`-only HTTP/1.1 server over a plain `tokio::net::TcpListener`, not
//! `axum`/`hyper` directly -- modelled **directly** on
//! `crates/av-dynamics-service/src/admin.rs`, down to the route shapes and the
//! `BTreeMap<&str, serde_json::Value>` response-building convention (ADR-004's
//! determinism rule); see that module's own doc comment for the full reasoning this
//! module borrows without repeating.
//!
//! # Routes
//!
//! - `GET /admin/api/evidence` -- exactly [`crate::evidence::evidence`]'s own JSON.
//! - `GET /admin/api/evidence/verify` -- exactly [`crate::evidence::verify_all`]'s own
//!   map, as JSON (`producer_id`/`ok`/`checked`/`broken_at_sequence`/`detail` per
//!   partition, keyed by `shard_key`).
//!
//! Every other path/method gets `404`/`405`.
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::ingest::Ingest;

/// Everything one admin connection needs: the same `Ingest` the gRPC service itself
/// mutates, behind the same kind of lock `crate::service::EdgeIngestService` uses (see
/// that module's own doc comment on why a plain `std::sync::Mutex` is the right choice
/// here -- nothing below holds it across an `.await`).
pub struct AdminState {
    pub ingest: Arc<Mutex<Ingest>>,
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
    let ingest = state.ingest.lock().unwrap_or_else(|p| p.into_inner());
    crate::evidence::evidence(&ingest)
}

fn verify_body(state: &AdminState) -> Value {
    let ingest = state.ingest.lock().unwrap_or_else(|p| p.into_inner());
    let verifications = crate::evidence::verify_all(&ingest);
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    for (shard_key, v) in verifications {
        let mut entry: BTreeMap<&str, Value> = BTreeMap::new();
        entry.insert("producer_id", Value::String(v.producer_id));
        entry.insert("ok", Value::Bool(v.ok));
        entry.insert("checked", Value::from(v.checked));
        entry.insert("broken_at_sequence", Value::from(v.broken_at_sequence));
        entry.insert("detail", Value::String(v.detail));
        m.insert(shard_key, serde_json::to_value(entry).expect("BTreeMap<&str, Value> always serializes"));
    }
    serde_json::to_value(m).expect("BTreeMap<String, Value> always serializes")
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
            "/admin/api/evidence/verify" => json_response("200 OK", &verify_body(&state)),
            other => json_response("404 Not Found", &serde_json::json!({"error": "not found", "path": other})),
        }
    };

    let mut stream = reader.into_inner();
    stream.write_all(&response).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Binds `addr` and serves `/admin/api/evidence`/`.../verify` forever (one `tokio::spawn`
/// per connection; each connection is short-lived -- one request, `Connection: close`).
/// `addr` must be a loopback address (ADR-004/question 155) -- callers (a real binary's
/// `main`) are responsible for only ever passing one this crate's own [`crate::server::
/// bind_loopback`] would also accept.
pub async fn serve(addr: SocketAddr, state: Arc<AdminState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_on(listener, state).await
}

/// Same as [`serve`], but over an already-bound `listener` -- what a test (`tests/
/// wire_evidence.rs`) uses so it can read the OS-assigned ephemeral port back from
/// `listener.local_addr()` before this function ever takes ownership of it, rather than
/// guessing a port, dropping a probe listener, and hoping nothing else claims it in
/// between (`bind`-then-immediately-`drop`-then-rebind-the-same-number would be exactly
/// that kind of race).
pub async fn serve_on(listener: TcpListener, state: Arc<AdminState>) -> std::io::Result<()> {
    loop {
        let (stream, _peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                eprintln!("av-ingest admin: connection error: {e}");
            }
        });
    }
}
