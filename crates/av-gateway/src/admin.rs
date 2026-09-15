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
//!
//! # R5.1/question 208(b): `Authorization: Bearer <token>`, verified through [`crate::auth`]
//!
//! Loopback-only binding was this route's entire access boundary before this round (AU 3.3.9's
//! own Gap row) -- "any local process can read the full bundle." [`read_request_line_and_
//! headers`] now also captures the `Authorization` header (case-insensitive name, a `Bearer `
//! prefix -- everything after it is the compact-serialization JWS), and [`handle_connection`]
//! runs it through [`crate::auth::AuthContext::authenticate_admin_bundle`] ([`crate::auth::
//! Surface::AdminBundle`], the human role table) BEFORE ever calling [`crate::evidence_bundle::
//! bundle_body`] -- an absent/invalid token is `401`, a verified token naming no granting role
//! is `403`, exactly [`crate::auth::AuthRefusal`]'s own UNAUTHENTICATED/PERMISSION_DENIED split
//! (invariant F), restated as HTTP status instead of a `tonic::Code`.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::auth::AuthRefusal;
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

/// R5.1: extracts the bearer token from a header line whose name is `Authorization`
/// (case-insensitive, matching HTTP's own header-name convention) and whose value starts with
/// `Bearer ` (case-insensitive) -- returns the remainder, trimmed. `None` for any other header
/// line (including a present-but-differently-shaped `Authorization` value, e.g. `Basic ...`),
/// never a panic on a header this route does not expect.
fn parse_bearer_token(header_line: &str) -> Option<String> {
    let (name, value) = header_line.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("authorization") {
        return None;
    }
    let value = value.trim();
    let rest = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?;
    Some(rest.trim().to_string())
}

/// Reads the request line and every header up to and including the blank line, extracting the
/// bearer token from an `Authorization` header if one is present (empty string otherwise) --
/// mirrors `crates/av-command/src/admin.rs`'s identical request-line helper, extended for R5.1
/// (every route here is still `GET`-only, so no request body is ever read).
async fn read_request_line_and_headers(stream: &mut BufReader<TcpStream>) -> std::io::Result<Option<(String, String, String)>> {
    let mut request_line = String::new();
    if stream.read_line(&mut request_line).await? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut bearer_token = String::new();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await? == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some(token) = parse_bearer_token(line.trim_end()) {
            bearer_token = token;
        }
    }
    Ok(Some((method, path, bearer_token)))
}

/// Maps an [`AuthRefusal`] to this route's own HTTP status line -- UNAUTHENTICATED ("who are
/// you") is `401`, PERMISSION_DENIED ("you may not") is `403` (invariant F, restated as HTTP).
fn auth_refusal_status_line(e: &AuthRefusal) -> &'static str {
    match e {
        AuthRefusal::MissingToken { .. } | AuthRefusal::TokenInvalid { .. } => "401 Unauthorized",
        AuthRefusal::RoleNotGranted { .. } | AuthRefusal::NoClearanceForSubject { .. } | AuthRefusal::ClearanceMismatch { .. } | AuthRefusal::PrincipalMismatch { .. } => "403 Forbidden",
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<BundleState>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let Some((method, path, bearer_token)) = read_request_line_and_headers(&mut reader).await? else {
        return Ok(());
    };

    let response = if method != "GET" {
        json_response("405 Method Not Allowed", &serde_json::json!({"error": "method not allowed", "method": method}))
    } else {
        match path.as_str() {
            "/admin/api/evidence/bundle" => match state.auth.authenticate_admin_bundle(&bearer_token, &state.counters) {
                Err(e) => {
                    let status = auth_refusal_status_line(&e);
                    json_response(status, &serde_json::json!({"error": e.to_string()}))
                }
                Ok(_principal) => match bundle_body(&state).await {
                    Ok(body) => json_response("200 OK", &body),
                    Err(e) => json_response("500 Internal Server Error", &serde_json::json!({"error": e.to_string()})),
                },
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
    use crate::auth::{AuthContext, GroupClearanceMap};
    use crate::counters::Counters;
    use av_command::authz::RoleTable;
    use av_command::clock::TestClock;
    use av_command::ledger::Ledger;
    use av_command::oidc::IssuerConfig;
    use av_command::test_support::{valid_claims, TestIssuer};
    use std::collections::BTreeMap;
    use tokio::io::AsyncReadExt as _;

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-gateway";
    const NOW_UNIX_S: i64 = 1_760_000_000;

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

    /// `token`, when `Some`, is sent as `Authorization: Bearer <token>` (R5.1).
    async fn get(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> (String, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let auth_header = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth_header}\r\n").as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8(buf).unwrap();
        let mut parts = text.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or("").to_string();
        let body = parts.next().unwrap_or("").to_string();
        (head.lines().next().unwrap_or("").to_string(), body)
    }

    /// `admin_roles` populates the human role table [`crate::auth::AuthContext`] gates `GET
    /// /admin/api/evidence/bundle` against ([`crate::auth::Surface::AdminBundle`]) -- an empty
    /// slice denies every token, exactly [`RoleTable::granting_role`]'s own deny-by-default
    /// guarantee.
    fn test_state(dir: &std::path::Path, issuer: &TestIssuer, admin_roles: &[(&str, &[&str])]) -> Arc<BundleState> {
        let ledger = Ledger::open(dir).unwrap();
        let mut roles = BTreeMap::new();
        for (role, surfaces) in admin_roles {
            roles.insert(role.to_string(), surfaces.iter().map(|s| s.to_string()).collect());
        }
        let auth = Arc::new(AuthContext::new(
            Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap()),
            Arc::new(RoleTable::from_config(&roles)),
            Arc::new(RoleTable::default()),
            Arc::new(GroupClearanceMap::default()),
            Arc::new(TestClock::new(NOW_UNIX_S * 1_000_000_000)),
        ));
        Arc::new(BundleState { evidence_ledger: Arc::new(ledger), counters: Arc::new(Counters::new()), run_id: "run-admin-test".to_string(), version: "0.1.0".to_string(), command_admin_addr: None, auth })
    }

    fn mint(issuer: &TestIssuer, groups: &[&str]) -> String {
        let mut claims = valid_claims(ISSUER, AUDIENCE, "admin-1", NOW_UNIX_S, 3_600);
        claims["groups"] = serde_json::json!(groups);
        issuer.mint(&claims)
    }

    #[tokio::test]
    async fn bundle_route_returns_200_with_the_expected_shape_for_an_authenticated_admin() {
        let dir = std::env::temp_dir().join(format!("av-gateway-admin-test-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let issuer = TestIssuer::new();
        let state = test_state(&dir, &issuer, &[("admins", &["admin_bundle"])]);
        let addr = spawn_test_server(state).await;
        let token = mint(&issuer, &["admins"]);

        let (status, body) = get(addr, "/admin/api/evidence/bundle", Some(&token)).await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        let json: Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(json["av_gateway"]["run_id"], "run-admin-test");
        assert_eq!(json["av_command"]["reachable"], false);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **R5.1 acceptance evidence: admin bundle unauthenticated caller.** No `Authorization`
    /// header at all -- `401`, and the COUNTER (not merely the status) is what this test
    /// asserts moved. Closes AU 3.3.9's own Gap row ("any local process can read the full
    /// bundle").
    #[tokio::test]
    async fn bundle_route_without_a_token_is_401_and_the_counter_moves() {
        let dir = std::env::temp_dir().join(format!("av-gateway-admin-test-401-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let issuer = TestIssuer::new();
        let state = test_state(&dir, &issuer, &[("admins", &["admin_bundle"])]);
        let counters_handle = state.counters.clone();
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/admin/api/evidence/bundle", None).await;
        assert_eq!(status, "HTTP/1.1 401 Unauthorized");
        let json: Value = serde_json::from_str(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("caller_token"), "{body}");
        assert_eq!(counters_handle.get("gateway_auth_missing_token"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A verified token whose groups grant no role at all (an empty admin_roles table --
    /// deny by default) is `403`, distinct from the `401` a missing/invalid token gets.
    #[tokio::test]
    async fn bundle_route_with_a_token_naming_no_granting_role_is_403() {
        let dir = std::env::temp_dir().join(format!("av-gateway-admin-test-403-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let issuer = TestIssuer::new();
        let state = test_state(&dir, &issuer, &[]); // empty table -- deny by default
        let counters_handle = state.counters.clone();
        let addr = spawn_test_server(state).await;
        let token = mint(&issuer, &["admins"]);

        let (status, _body) = get(addr, "/admin/api/evidence/bundle", Some(&token)).await;
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        assert_eq!(counters_handle.get("gateway_auth_role_not_granted"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_path_is_404_and_non_get_is_405() {
        let dir = std::env::temp_dir().join(format!("av-gateway-admin-test-404-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let issuer = TestIssuer::new();
        let state = test_state(&dir, &issuer, &[("admins", &["admin_bundle"])]);
        let addr = spawn_test_server(state).await;

        let (status, body) = get(addr, "/nope", None).await;
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
