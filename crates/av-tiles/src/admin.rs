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
//! # Round 5, item B: authenticated, on the SAME posture as `av-gateway`'s admin surface
//!
//! Question 229's open ruling, verbatim: "`/admin/api/counters` on `av-tiles` takes the same
//! authentication `av-gateway`'s admin surface has (one posture, question 215's)." Round 4
//! recorded this as defect 7: this route had no access control at all, while `av-gateway`'s
//! `GET /admin/api/evidence/bundle` was gated under the lead's R5.1 ruling
//! (`crates/av-gateway/src/admin.rs`'s own module doc, "R5.1/question 208(b)"). [`AdminAuthContext`]
//! is that same gate, restated for this crate's one admin route: `Authorization: Bearer
//! <token>` parsed from the request headers, run through [`av_command::oidc::verify`] (the
//! SAME verifier [`crate::core::handle`] already uses on the main port -- never a second
//! copy), then a role check against [`av_command::authz::RoleTable`] (the SAME reuse
//! `crates/av-gateway/src/auth.rs`'s own module doc describes for a crate whose surface
//! names `av_command::authz::ServiceRpc` has no way to express) for the one surface this
//! route protects ([`SURFACE`]). An absent or unverifiable token is `401`; a verified token
//! whose `groups` claim names no role granting [`SURFACE`] is `403` -- invariant F's own
//! UNAUTHENTICATED/PERMISSION_DENIED split, restated as HTTP, identical to
//! `crates/av-gateway/src/admin.rs::auth_refusal_status_line`. Every refusal is recorded in
//! the SAME [`Counters`] instance `bin/av-tiles.rs` already shares between this route and the
//! main port -- under codes namespaced `tiles_admin_auth_*`, deliberately distinct from the
//! main port's own `tiles_auth_*` codes (`crate::refusal::TileRefusal`), because the two are
//! different gates over the same process and conflating their codes would make either count
//! unreadable on its own.
//!
//! # Still a SEPARATE bind
//!
//! Mirrors `av-command`'s own `--admin-bind` (`crates/av-command/src/bin/av-command.rs`):
//! an operational/observability surface, not a caller-facing data path, so it carries none of
//! the label-enforcement machinery [`crate::core::handle`] enforces on the main port (there is
//! no product/tile data behind this route at all, only this process's own refusal counts) --
//! and because it is a SEPARATE bind (never the main port), an operator who wants it
//! firewalled off from the main tile-serving port can still do so at the network layer,
//! exactly the shape `av-command --admin-bind` already gives operators today. What changed in
//! round 5 is authentication, not the bind topology.

use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use av_cdm::pb::Principal;
use av_command::authz::RoleTable;
use av_command::oidc::{verify, IssuerConfig, TokenError};

use crate::counters::{Counted, Counters};

/// The one surface [`AdminAuthContext`] gates -- see this module's own doc, "Round 5, item
/// B". A role (an OIDC `groups` entry) grants this route exactly when it appears as a key in
/// the `admin_roles` table [`AdminAuthContext::new`] is built with, mapped to a list
/// containing this string (or [`av_command::authz::WILDCARD`]) -- [`RoleTable::granting_role`]'s
/// own deny-by-default guarantee, unchanged.
pub const SURFACE: &str = "admin_counters";

/// Everything [`AdminAuthContext::authenticate`] needs to verify a caller and check its role
/// -- the issuer configuration [`av_command::oidc::verify`] checks tokens against (REUSED
/// from this deployment's own [`crate::config::TilesConfig::issuer_config`], never a second,
/// independently-loaded issuer for this one route) and the human role table this deployment
/// configured for the admin surface specifically.
pub struct AdminAuthContext {
    issuer_config: Arc<IssuerConfig>,
    admin_roles: Arc<RoleTable>,
}

impl AdminAuthContext {
    pub fn new(issuer_config: Arc<IssuerConfig>, admin_roles: Arc<RoleTable>) -> Self {
        Self { issuer_config, admin_roles }
    }

    /// Verify -> role check, the identical two-step `crates/av-gateway/src/auth.rs::
    /// AuthContext::authenticate_admin_bundle` runs for its own one human-only surface.
    /// `now_tai_ns` is an injected clock reading (rule: clocks injected, never slept on --
    /// `crate::core::handle`'s own identical rule, restated here for this route).
    pub fn authenticate(&self, token: &str, now_tai_ns: i64, counters: &Counters) -> Result<Principal, AdminAuthRefusal> {
        if token.is_empty() {
            let refusal = AdminAuthRefusal::MissingToken;
            counters.record(&refusal);
            return Err(refusal);
        }
        let principal = match verify(token, &self.issuer_config, now_tai_ns) {
            Ok(p) => p,
            Err(source) => {
                // The underlying TokenError's own specific `token_*` code, recorded
                // ALONGSIDE the generic one below -- mirrors `crates/av-gateway/src/
                // auth.rs::AuthContext::verify_token`'s identical double-count.
                counters.record(&source);
                let refusal = AdminAuthRefusal::TokenInvalid { source };
                counters.record(&refusal);
                return Err(refusal);
            }
        };
        if self.admin_roles.granting_role(&principal.groups, SURFACE).is_none() {
            let refusal = AdminAuthRefusal::RoleNotGranted { groups: principal.groups.clone() };
            counters.record(&refusal);
            return Err(refusal);
        }
        Ok(principal)
    }
}

/// Every way [`AdminAuthContext::authenticate`] can refuse a caller -- distinct, greppable,
/// [`Counted`] codes (`tiles_admin_auth_*`), classifiable by the CODE, never by message prose
/// (mirrors `crate::refusal::TileRefusal`'s own discipline for the main port).
#[derive(Debug, Error)]
pub enum AdminAuthRefusal {
    /// No `Authorization: Bearer <token>` header at all (or an empty one) -- never treated as
    /// allow.
    #[error("no Authorization: Bearer <token> header presented for the admin surface")]
    MissingToken,
    /// The presented token failed [`av_command::oidc::verify`].
    #[error("bearer token failed verification: {source}")]
    TokenInvalid { #[source] source: TokenError },
    /// The verified token's `groups` name no role granting [`SURFACE`].
    #[error("no role in groups {groups:?} grants the admin_counters surface (deny-by-default)")]
    RoleNotGranted { groups: Vec<String> },
}

impl Counted for AdminAuthRefusal {
    fn code(&self) -> &'static str {
        match self {
            AdminAuthRefusal::MissingToken => "tiles_admin_auth_missing_token",
            AdminAuthRefusal::TokenInvalid { .. } => "tiles_admin_auth_token_invalid",
            AdminAuthRefusal::RoleNotGranted { .. } => "tiles_admin_auth_role_not_granted",
        }
    }
}

/// `401` ("who are you") for a missing/invalid token, `403` ("you may not") for a verified
/// token naming no granting role -- invariant F, restated as HTTP; identical split to
/// `crates/av-gateway/src/admin.rs::auth_refusal_status_line`.
fn admin_auth_status(refusal: &AdminAuthRefusal) -> u16 {
    match refusal {
        AdminAuthRefusal::MissingToken | AdminAuthRefusal::TokenInvalid { .. } => 401,
        AdminAuthRefusal::RoleNotGranted { .. } => 403,
    }
}

/// Everything one connection to this route needs -- bundled (mirrors `crates/av-gateway/src/
/// admin.rs::BundleState`) so `serve`/`handle_connection` take one `Arc` to clone per
/// connection rather than three.
pub struct AdminState {
    pub counters: Arc<Counters>,
    pub auth: Arc<AdminAuthContext>,
    /// Read fresh for every request (never cached at bind time) -- `av_command::clock::
    /// SystemClock` for a real binary, an injected `TestClock` for a test (mirrors
    /// `crate::server::serve`'s identical rule for the main port).
    pub clock: Arc<dyn av_command::clock::Clock>,
}

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
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        _ => "500 Internal Server Error",
    }
}

/// Hand-rolled `{"error":"<escaped message>"}` -- this module's own doc, "No new JSON
/// dependency in production", applies identically to an auth refusal's message as it does to
/// the counters body.
fn error_json(message: &str) -> String {
    format!("{{\"error\":\"{}\"}}", json_escape(message))
}

fn response_bytes(status: u16, body: &str) -> Vec<u8> {
    let head = format!("HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status_line(status), body.len());
    let mut out = head.into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Round 5, item B: extracts the bearer token from a header line whose name is
/// `Authorization` (case-insensitive) and whose value starts with `Bearer ` (case-
/// insensitive) -- returns the remainder, trimmed. `None` for any other header line
/// (including a present-but-differently-shaped `Authorization` value, e.g. `Basic ...`),
/// never a panic on a header this route does not expect. Byte-for-byte the same rule
/// `crates/av-gateway/src/admin.rs::parse_bearer_token` already establishes; hand-rolled
/// again here rather than shared, the same "each hand-rolled HTTP module owns its own small
/// parsing helpers" precedent this crate's own `crate::server::read_request_line_and_headers`
/// already sets alongside `av-command`'s and `av-gateway`'s admin modules -- none of the four
/// depends on a fifth to share four lines of `str` parsing.
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
/// round 5, item B extends what was `read_request_line_and_drain_headers` (P1-era: discarded
/// every header) to capture the one this route now needs, mirroring `crates/av-gateway/src/
/// admin.rs::read_request_line_and_headers`'s identical shape for its own single-header need.
/// `None` iff the peer closed before sending a request line at all.
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

async fn handle_connection(stream: TcpStream, state: Arc<AdminState>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let Some((method, path, bearer_token)) = read_request_line_and_headers(&mut reader).await? else {
        return Ok(());
    };

    let response = if method != "GET" {
        response_bytes(405, "{\"error\":\"method not allowed\"}")
    } else {
        match path.as_str() {
            "/admin/api/counters" => match state.auth.authenticate(&bearer_token, state.clock.now_tai_ns(), &state.counters) {
                Err(e) => {
                    let status = admin_auth_status(&e);
                    response_bytes(status, &error_json(&e.to_string()))
                }
                Ok(_principal) => response_bytes(200, &counters_json(&state.counters)),
            },
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
pub async fn serve(addr: std::net::SocketAddr, state: Arc<AdminState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state).await {
                eprintln!("av-tiles (admin): connection error: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_command::clock::TestClock;
    use av_command::test_support::{valid_claims, TestIssuer};
    use std::collections::BTreeMap;
    use tokio::io::AsyncReadExt as _;

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-tiles";
    const NOW_UNIX_S: i64 = 1_760_000_000;
    const TTL_S: i64 = 3_600;

    fn now_tai_ns() -> i64 {
        av_cdm::time::Tai::from_utc_nanos(NOW_UNIX_S * 1_000_000_000).as_nanos()
    }

    /// `admin_roles` populates the human role table [`AdminAuthContext`] gates `GET
    /// /admin/api/counters` against ([`SURFACE`]) -- an empty slice denies every token,
    /// exactly [`RoleTable::granting_role`]'s own deny-by-default guarantee (mirrors
    /// `crates/av-gateway/src/admin.rs::tests::test_state`'s identical fixture shape).
    fn admin_state(issuer: &TestIssuer, admin_roles: &[(&str, &[&str])]) -> Arc<AdminState> {
        let issuer_config = Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap());
        let mut roles = BTreeMap::new();
        for (role, surfaces) in admin_roles {
            roles.insert(role.to_string(), surfaces.iter().map(|s| s.to_string()).collect());
        }
        let auth = Arc::new(AdminAuthContext::new(issuer_config, Arc::new(RoleTable::from_config(&roles))));
        Arc::new(AdminState { counters: Arc::new(Counters::new()), auth, clock: Arc::new(TestClock::new(now_tai_ns())) })
    }

    fn mint(issuer: &TestIssuer, groups: &[&str]) -> String {
        let mut claims = valid_claims(ISSUER, AUDIENCE, "admin-1", NOW_UNIX_S, TTL_S);
        claims["groups"] = serde_json::json!(groups);
        issuer.mint(&claims)
    }

    /// `token`, when `Some`, is sent as `Authorization: Bearer <token>` (round 5, item B).
    async fn get(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let auth_header = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth_header}\r\n").as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf).to_string();
        let split_at = text.find("\r\n\r\n").unwrap();
        let head = &text[..split_at];
        let body = text[split_at + 4..].to_string();
        let status: u16 = head.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body)
    }

    async fn spawn(state: Arc<AdminState>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let _ = handle_connection(stream, state.clone()).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_verified_token_with_the_granting_role_gets_the_counters_body_sorted() {
        struct Refusal(&'static str);
        impl crate::counters::Counted for Refusal {
            fn code(&self) -> &'static str {
                self.0
            }
        }
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[("admins", &[SURFACE])]);
        state.counters.record(&Refusal("zeta_code"));
        state.counters.record(&Refusal("alpha_code"));
        state.counters.record(&Refusal("alpha_code"));
        let token = mint(&issuer, &["admins"]);

        let addr = spawn(state).await;
        let (status, body) = get(addr, "/admin/api/counters", Some(&token)).await;
        assert_eq!(status, 200);
        assert_eq!(body, "{\"counters\":{\"alpha_code\":2,\"zeta_code\":1}}", "sorted by code, ADR-004's determinism rule");
    }

    /// **Round 5, item B acceptance evidence: an absent token.** No `Authorization` header at
    /// all -- `401`, and the COUNTER (not merely the status) is what this test asserts moved.
    /// Closes round 4's defect 7 ("no access control at all").
    #[tokio::test]
    async fn counters_route_without_a_token_is_401_and_the_counter_moves() {
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[("admins", &[SURFACE])]);
        let counters_handle = state.counters.clone();
        let addr = spawn(state).await;

        let (status, body) = get(addr, "/admin/api/counters", None).await;
        assert_eq!(status, 401);
        assert!(body.contains("admin surface"), "{body}");
        assert_eq!(counters_handle.get("tiles_admin_auth_missing_token"), 1);
        assert_eq!(counters_handle.get("tiles_admin_auth_token_invalid"), 0);
        assert_eq!(counters_handle.get("tiles_admin_auth_role_not_granted"), 0);
    }

    /// **Round 5, item B acceptance evidence: an invalid token.** Syntactically present but
    /// unverifiable -- `401`, distinct counter from the missing-token case.
    #[tokio::test]
    async fn counters_route_with_an_invalid_token_is_401_and_the_counter_moves() {
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[("admins", &[SURFACE])]);
        let counters_handle = state.counters.clone();
        let addr = spawn(state).await;

        let (status, _body) = get(addr, "/admin/api/counters", Some("not.a.real.token")).await;
        assert_eq!(status, 401);
        assert_eq!(counters_handle.get("tiles_admin_auth_token_invalid"), 1);
        assert_eq!(counters_handle.get("tiles_admin_auth_missing_token"), 0);
    }

    /// Round 5 manager review. The test above presents `"not.a.real.token"`, which
    /// [`av_command::oidc::verify`] rejects on segment-count/base64 grounds BEFORE it ever
    /// reaches signature verification -- so on its own it does not prove this route checks
    /// a signature at all. This one does: a token minted by a SECOND, independent
    /// [`TestIssuer`] (a different RSA-2048 key), structurally perfect in every other
    /// respect -- right `iss`, right `aud`, unexpired against the injected clock, and
    /// carrying the very group that DOES grant [`SURFACE`] -- so the ONLY thing that can
    /// refuse it is the signature failing against the issuer key this deployment was
    /// configured with. A verifier that skipped signature checking would return 200 here
    /// and still return 401 above.
    #[tokio::test]
    async fn a_well_formed_token_signed_by_a_foreign_key_is_401_and_never_reaches_the_role_check() {
        let issuer = TestIssuer::new();
        let foreign = TestIssuer::new();
        let state = admin_state(&issuer, &[("admins", &[SURFACE])]);
        let counters_handle = state.counters.clone();
        let addr = spawn(state).await;

        // Exactly the passing test's claims, including the granting group -- signed by the
        // wrong key and differing in nothing else.
        let token = mint(&foreign, &["admins"]);
        let (status, _body) = get(addr, "/admin/api/counters", Some(&token)).await;

        assert_eq!(status, 401, "a token signed by a key this deployment does not trust must be 401, never 200");
        assert_eq!(counters_handle.get("tiles_admin_auth_token_invalid"), 1);
        // It must be refused as UNAUTHENTICATED and never fall through to the role check --
        // invariant F's split: we never established who this caller is.
        assert_eq!(counters_handle.get("tiles_admin_auth_role_not_granted"), 0);
        assert_eq!(counters_handle.get("tiles_admin_auth_missing_token"), 0);
    }

    /// Round 5 manager review, the same gap's second half: the clock this route verifies
    /// against is the INJECTED one ([`AdminState::clock`], read fresh per request), never a
    /// wall clock. A token correctly signed by the right issuer and carrying the granting
    /// group, whose `exp` is in the past relative to that injected reading, must be 401 --
    /// which also proves the clock reading is genuinely consulted rather than passed and
    /// dropped.
    #[tokio::test]
    async fn a_correctly_signed_but_expired_token_is_401_against_the_injected_clock() {
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[("admins", &[SURFACE])]);
        let counters_handle = state.counters.clone();
        let addr = spawn(state).await;

        // Issued and expired well before NOW_UNIX_S, the reading the injected TestClock
        // returns; in every other respect the passing token's exact shape.
        let mut claims = valid_claims(ISSUER, AUDIENCE, "admin-1", NOW_UNIX_S - 10 * TTL_S, TTL_S);
        claims["groups"] = serde_json::json!(["admins"]);
        let token = issuer.mint(&claims);

        let (status, _body) = get(addr, "/admin/api/counters", Some(&token)).await;
        assert_eq!(status, 401, "an expired token must be 401 even with a valid signature and a granting role");
        assert_eq!(counters_handle.get("tiles_admin_auth_token_invalid"), 1);
        assert_eq!(counters_handle.get("tiles_admin_auth_role_not_granted"), 0);
    }

    /// **Round 5, item B acceptance evidence: a verified token naming no granting role.**
    /// `403`, distinct from the `401` a missing/invalid token gets -- invariant F.
    #[tokio::test]
    async fn counters_route_with_a_token_naming_no_granting_role_is_403_and_the_counter_moves() {
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[]); // empty table -- deny by default
        let counters_handle = state.counters.clone();
        let addr = spawn(state).await;
        let token = mint(&issuer, &["admins"]);

        let (status, _body) = get(addr, "/admin/api/counters", Some(&token)).await;
        assert_eq!(status, 403);
        assert_eq!(counters_handle.get("tiles_admin_auth_role_not_granted"), 1);
    }

    /// A role granting some OTHER surface must not grant this one -- `RoleTable::granting_role`
    /// keys off `SURFACE` specifically, never "any role at all".
    #[tokio::test]
    async fn a_role_granting_a_different_surface_does_not_grant_the_admin_counters_surface() {
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[("operators", &["some_other_surface"])]);
        let addr = spawn(state).await;
        let token = mint(&issuer, &["operators"]);

        let (status, _body) = get(addr, "/admin/api/counters", Some(&token)).await;
        assert_eq!(status, 403);
    }

    #[tokio::test]
    async fn an_unknown_path_is_404_and_a_non_get_method_is_405() {
        let issuer = TestIssuer::new();
        let state = admin_state(&issuer, &[("admins", &[SURFACE])]);
        let addr = spawn(state).await;
        let (status, _) = get(addr, "/nope", None).await;
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
