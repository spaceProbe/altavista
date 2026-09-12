//! `GET /admin/api/evidence` and `GET /admin/api/evidence/verify` (ADR-004 question 63),
//! **localhost only** -- mirrors `crates/av-dynamics-service/src/admin.rs`'s own hand-rolled
//! shape exactly: a plain `tokio::net::TcpListener`, manual `GET`-only request-line parsing,
//! no `axum`/`hyper` direct dependency (this crate does not even have `tonic` in its tree,
//! so there is even less reason here than there than to pin a heavier HTTP stack for two
//! fixed routes).
//!
//! # Routes
//!
//! - `GET /admin/api/evidence` -- [`crate::evidence::evidence_body`].
//! - `GET /admin/api/evidence/verify` -- [`crate::evidence::verify_body`].
//!
//! Every other path/method gets `404`/`405`.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::evidence::{self, AdminState};

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
            "/admin/api/evidence" => match evidence::evidence_body(&state) {
                Ok(body) => json_response("200 OK", &body),
                Err(e) => json_response("500 Internal Server Error", &serde_json::json!({"error": e.to_string()})),
            },
            "/admin/api/evidence/verify" => match evidence::verify_body(&state) {
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

/// Binds `addr` and serves the two admin routes forever (one `tokio::spawn`ed task per
/// connection; each connection is short-lived -- one request, `Connection: close`). `addr`
/// must be a loopback address -- callers are responsible for never passing a non-loopback
/// one (ADR-004).
pub async fn serve(addr: std::net::SocketAddr, state: Arc<AdminState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                eprintln!("av-command admin: connection error: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::ledger::Ledger;
    use av_cdm::pb::{AckLevel, CommandState, CommandTransition};
    use tokio::io::AsyncReadExt as _;

    /// Binds on `127.0.0.1:0` (an OS-assigned ephemeral port, never a fixed one), spawns the
    /// server on that listener, and returns the address a test client should connect to.
    async fn spawn_test_server(state: Arc<AdminState>) -> std::net::SocketAddr {
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
        let status_line = head.lines().next().unwrap_or("").to_string();
        (status_line, body)
    }

    fn test_state(dir: &std::path::Path) -> Arc<AdminState> {
        let ledger = Ledger::open(dir).unwrap();
        let clock = TestClock::new(5_000);
        let t = CommandTransition {
            state: CommandState::Proposed as i32,
            tai_ns: 5_000,
            principal: "model-x".to_string(),
            reason: "reason".to_string(),
            ack_level: AckLevel::Unspecified as i32,
            delegation_id: String::new(),
        };
        ledger.append("sat-admin-test", "cmd-1", "burn", t, None, &clock).unwrap();
        Arc::new(AdminState { ledger: Arc::new(ledger), run_id: "run-admin-test".to_string(), version: "0.1.0".to_string() })
    }

    #[tokio::test]
    async fn evidence_route_returns_200_with_the_expected_json_body() {
        let dir = std::env::temp_dir().join(format!("av-command-admin-test-evidence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = test_state(&dir);
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/admin/api/evidence").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        let json: Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(json["version"], "0.1.0");
        assert_eq!(json["run_id"], "run-admin-test");
        let partitions = json["partitions"].as_array().unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0]["partition"], "sat-admin-test");
        assert_eq!(partitions[0]["records"], 1);
        assert!(json["fips"]["openssl_version"].as_str().unwrap().starts_with("OpenSSL"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verify_route_returns_200_with_ok_true_for_a_clean_ledger() {
        let dir = std::env::temp_dir().join(format!("av-command-admin-test-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = test_state(&dir);
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/admin/api/evidence/verify").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        let json: Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(json["ok"], true);
        let partitions = json["partitions"].as_array().unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0]["producer_id"], "sat-admin-test");
        assert_eq!(partitions[0]["ok"], true);
        assert_eq!(partitions[0]["checked"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_path_is_404_and_non_get_is_405() {
        let dir = std::env::temp_dir().join(format!("av-command-admin-test-404-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = test_state(&dir);
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/nope").await;
        assert_eq!(status, "HTTP/1.1 404 Not Found");
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["path"], "/nope");

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"POST /admin/api/evidence HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("HTTP/1.1 405"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
