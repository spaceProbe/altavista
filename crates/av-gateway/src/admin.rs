//! `GET /admin/api/evidence/bundle` (R3.6/A6, Part 3), **localhost only** -- the same
//! hand-rolled `tokio::net::TcpListener` shape `crates/av-command/src/admin.rs` uses (manual
//! `GET`-only request-line parsing, no `axum`/`hyper`), reused here rather than re-invented:
//! this crate already has no reason to pull a heavier HTTP stack in for one fixed route than
//! `av-command` did for two.
//!
//! # Routes
//!
//! - `GET /admin/api/evidence/bundle` -- [`crate::evidence_bundle::bundle_body`].
//!
//! Every other path/method gets `404`/`405`.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::evidence_bundle::{bundle_body, BundleState};

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

/// Reads exactly the request line and discards headers up to and including the blank line --
/// mirrors `crates/av-command/src/admin.rs`'s identical helper (every route here is `GET`-only
/// too, so no request body is ever read).
async fn read_request_line_and_drain_headers(stream: &mut BufReader<TcpStream>) -> std::io::Result<Option<(String, String)>> {
    let mut request_line = String::new();
    if stream.read_line(&mut request_line).await? == 0 {
        return Ok(None);
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

async fn handle_connection(stream: TcpStream, state: Arc<BundleState>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let Some((method, path)) = read_request_line_and_drain_headers(&mut reader).await? else {
        return Ok(());
    };

    let response = if method != "GET" {
        json_response("405 Method Not Allowed", &serde_json::json!({"error": "method not allowed", "method": method}))
    } else {
        match path.as_str() {
            "/admin/api/evidence/bundle" => match bundle_body(&state).await {
                Ok(body) => json_response("200 OK", &body),
                Err(e) => json_response("500 Internal Server Error", &serde_json::json!({"error": e.to_string()})),
            },
            other => json_response("404 Not Found", &serde_json::json!({"error": "not found", "path": other})),
        }
    };

    let mut stream = reader.into_inner();
    stream.write_all(&response).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Binds `addr` and serves the one admin route forever. `addr` must be a loopback address --
/// callers are responsible for never passing a non-loopback one (ADR-004; question 155 --
/// mirrors `crates/av-command/src/admin.rs::serve`'s identical contract and doc note).
pub async fn serve(addr: std::net::SocketAddr, state: Arc<BundleState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                eprintln!("av-gateway admin: connection error: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counters::Counters;
    use av_command::ledger::Ledger;
    use tokio::io::AsyncReadExt as _;

    async fn spawn_test_server(state: Arc<BundleState>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, state).await;
                });
            }
        });
        addr
    }

    async fn get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8(buf).unwrap();
        let mut parts = text.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or("").to_string();
        let body = parts.next().unwrap_or("").to_string();
        (head.lines().next().unwrap_or("").to_string(), body)
    }

    fn test_state(dir: &std::path::Path) -> Arc<BundleState> {
        let ledger = Ledger::open(dir).unwrap();
        Arc::new(BundleState { evidence_ledger: Arc::new(ledger), counters: Arc::new(Counters::new()), run_id: "run-admin-test".to_string(), version: "0.1.0".to_string(), command_admin_addr: None })
    }

    #[tokio::test]
    async fn bundle_route_returns_200_with_the_expected_shape() {
        let dir = std::env::temp_dir().join(format!("av-gateway-admin-test-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = test_state(&dir);
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/admin/api/evidence/bundle").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        let json: Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(json["av_gateway"]["run_id"], "run-admin-test");
        assert_eq!(json["av_command"]["reachable"], false);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_path_is_404_and_non_get_is_405() {
        let dir = std::env::temp_dir().join(format!("av-gateway-admin-test-404-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = test_state(&dir);
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/nope").await;
        assert_eq!(status, "HTTP/1.1 404 Not Found");
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["path"], "/nope");

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"POST /admin/api/evidence/bundle HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("HTTP/1.1 405"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
