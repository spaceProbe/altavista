//! H5b-1 (`docs/heavy-plan.md` H5, round 3): a minimal, OPTIONAL admin surface -- exactly one
//! route, `GET /admin/api/counters` -- so an operator (or a test) can read this gateway's own
//! refusal counters from OUTSIDE the process, mirroring `crates/av-command/src/admin.rs`'s
//! own hand-rolled shape (plain `tokio::net::TcpListener`, manual `GET`-only request-line
//! parsing, no `axum`/`hyper` direct dependency -- that module's own doc: "even less reason
//! here than there to pin a heavier HTTP stack for two fixed routes"; this one fixed route
//! has even less).
//!
//! # Why this exists now, when H4's own round-2 status said "no admin surface"
//!
//! `docs/heavy-plan.md`'s round-2 status recorded, deliberately: "`av-tiles` takes
//! `127.0.0.1:50073`, no admin surface, hence no `+100` counterpart" -- correct for what that
//! round needed. This round's own brief asks for an end-to-end test to assert "the gateway's
//! refusal counter for it incremented -- read the counter from the gateway itself, do not
//! merely assume it", against a REAL `av-tiles` subprocess. `crate::counters::Counters` is
//! in-process state with no way for an external test (or a real operator) to read it short of
//! a second listener -- there is no other route on the main port to piggyback it onto (this
//! crate's own crate doc: exactly two routes, neither under `/admin`), so the round-2 decision
//! is revisited here for a concrete, present need rather than sitting on the "no admin
//! surface" line for years. It follows the `+100` convention this workspace's other services
//! already establish ([`DEFAULT_ADMIN_BIND`]'s own doc comment).
//!
//! # No new JSON dependency in production
//!
//! `Cargo.toml`'s own "Deliberately NOT a dependency" note: this crate's wire format is
//! `prost`-encoded protobuf in, raw tile bytes out -- `serde_json` is a DEV-only dependency,
//! for building test claims. This module therefore hand-rolls its one small, fixed JSON
//! shape (a flat string-to-integer counters map, `Counted::code()`'s own values as keys,
//! which are always plain `snake_case` identifiers -- escaped defensively anyway, never
//! assumed safe) rather than pulling `serde_json` into this crate's production dependency
//! set for one endpoint.
//!
//! # Unauthenticated, on purpose, on a SEPARATE bind
//!
//! Mirrors `av-command`'s own `--admin-bind` (`crates/av-command/src/bin/av-command.rs`):
//! operational/observability surface, not a caller-facing data path, so it carries none of
//! the OIDC/label machinery [`crate::core::handle`] enforces on the main port -- and because
//! it is a SEPARATE bind (never the main port), an operator who wants it firewalled off from
//! the main tile-serving port can do so at the network layer, exactly the shape
//! `av-command --admin-bind` already gives operators today.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::counters::Counters;

/// Question 208(c)/219(a)-style `+100` convention: [`crate::server`]'s own `DEFAULT_BIND` is
/// `127.0.0.1:50073` (`crates/av-tiles/src/bin/av-tiles.rs`); this is that port `+100`, the
/// identical offset `crates/av-command`'s own grpc/admin bind pair already uses. Pinned by
/// this module's own `default_admin_bind_is_the_main_default_bind_plus_100` test -- not yet a
/// row in `docs/architecture.md`'s owned port map (that file is shared across tracks;
/// recorded as an open item for the lead rather than edited unilaterally, the identical
/// posture `docs/heavy-plan.md`'s own round-2 status already took for this crate's main
/// bind's row).
pub const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:50173";

fn hex_escape_u32(c: u32) -> String {
    format!("\\u{c:04x}")
}

/// Hand-rolled JSON escaping for a counter's own `code()` string -- see this module's own doc,
/// "No new JSON dependency in production".
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&hex_escape_u32(c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// `{"counters":{"<code>":<count>,...}}`, sorted (`Counters::snapshot`'s own `BTreeMap`
/// iteration order -- ADR-004's determinism rule).
fn counters_json(counters: &Counters) -> String {
    let snapshot = counters.snapshot();
    let mut body = String::from("{\"counters\":{");
    for (i, (code, count)) in snapshot.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        body.push('"');
        body.push_str(&json_escape(code));
        body.push_str("\":");
        body.push_str(&count.to_string());
    }
    body.push_str("}}");
    body
}

fn status_line(status: u16) -> &'static str {
    match status {
        200 => "200 OK",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        _ => "500 Internal Server Error",
    }
}

fn response_bytes(status: u16, body: &str) -> Vec<u8> {
    let head = format!("HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status_line(status), body.len());
    let mut out = head.into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Reads the request line and discards every header up to and including the blank line --
/// this surface never reads a request body and never reads back a request header (unlike
/// `crate::server`'s own main-port `read_request_line_and_headers`, which must read back
/// `Authorization`/`Range`/`If-None-Match` -- this endpoint takes no input beyond the path
/// itself). `None` iff the peer closed before sending a request line at all.
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

async fn handle_connection(stream: TcpStream, counters: Arc<Counters>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let Some((method, path)) = read_request_line_and_drain_headers(&mut reader).await? else {
        return Ok(());
    };

    let response = if method != "GET" {
        response_bytes(405, "{\"error\":\"method not allowed\"}")
    } else {
        match path.as_str() {
            "/admin/api/counters" => response_bytes(200, &counters_json(&counters)),
            _ => response_bytes(404, "{\"error\":\"not found\"}"),
        }
    };

    let mut stream = reader.into_inner();
    stream.write_all(&response).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Binds `addr` and serves `GET /admin/api/counters` forever, one `tokio::spawn`ed task per
/// connection -- mirrors [`crate::server::serve`]'s own identical shape (and, through it,
/// `crates/av-command/src/admin.rs::serve`'s).
pub async fn serve(addr: std::net::SocketAddr, counters: Arc<Counters>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let counters = counters.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, counters).await {
                eprintln!("av-tiles (admin): connection error: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt as _;

    async fn get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf).to_string();
        let split_at = text.find("\r\n\r\n").unwrap();
        let head = &text[..split_at];
        let body = text[split_at + 4..].to_string();
        let status: u16 = head.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body)
    }

    async fn spawn(counters: Arc<Counters>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let _ = handle_connection(stream, counters.clone()).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn counters_route_returns_every_recorded_code_and_count_sorted() {
        struct Refusal(&'static str);
        impl crate::counters::Counted for Refusal {
            fn code(&self) -> &'static str {
                self.0
            }
        }
        let counters = Arc::new(Counters::new());
        counters.record(&Refusal("zeta_code"));
        counters.record(&Refusal("alpha_code"));
        counters.record(&Refusal("alpha_code"));

        let addr = spawn(counters).await;
        let (status, body) = get(addr, "/admin/api/counters").await;
        assert_eq!(status, 200);
        assert_eq!(body, "{\"counters\":{\"alpha_code\":2,\"zeta_code\":1}}", "sorted by code, ADR-004's determinism rule");
    }

    #[tokio::test]
    async fn an_unknown_path_is_404_and_a_non_get_method_is_405() {
        let addr = spawn(Arc::new(Counters::new())).await;
        let (status, _) = get(addr, "/nope").await;
        assert_eq!(status, 404);

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"POST /admin/api/counters HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 405"));
    }

    #[test]
    fn default_admin_bind_is_the_main_default_bind_plus_100() {
        assert_eq!(DEFAULT_ADMIN_BIND, "127.0.0.1:50173");
    }
}
