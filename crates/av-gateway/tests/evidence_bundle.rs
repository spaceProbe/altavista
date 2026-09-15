//! R3.6/A6, Part 3: "the evidence bundle from both services collects in one call" --
//! against two REAL, running admin HTTP servers (the real, unmodified
//! `av_command::admin::serve` over a real ledger, and this crate's own new
//! `av_gateway::admin::serve` over a real evidence ledger), never a hand-assembled literal
//! standing in for either side.

use std::collections::BTreeMap;
use std::sync::Arc;

use av_cdm::pb::{AckLevel, CommandState, CommandTransition};
use av_command::authz::{RoleTable, WILDCARD};
use av_command::clock::TestClock;
use av_command::counters::Counters;
use av_command::evidence::AdminState;
use av_command::ledger::{CommandMeta, Ledger};
use av_command::oidc::IssuerConfig;
use av_command::test_support::{valid_claims, TestIssuer};
use av_gateway::auth::{AuthContext, GroupClearanceMap};
use av_gateway::evidence_bundle::BundleState;
use av_gateway::labels::ClearanceLadder;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const ISSUER: &str = "https://sso.test.example/";
const AUDIENCE: &str = "av-gateway";

/// R5.1: mints a token, against `issuer`, whose group holds a WILDCARD-granting role -- every
/// test in this file exercises the bundle's own CONTENT (D6/A6's own acceptance evidence), not
/// the auth gate itself (`crates/av-gateway/src/admin.rs`'s own module tests cover that), so
/// one maximally-permissive token per spawned server is the right fixture here.
fn mint_admin_token(issuer: &TestIssuer) -> String {
    let mut claims = valid_claims(ISSUER, AUDIENCE, "admin-it", 1_700_000_000, 3_600);
    claims["groups"] = serde_json::json!(["test-admin"]);
    issuer.mint(&claims)
}

fn tmp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("av-gateway-evidence-bundle-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A real, running `av-command` admin server (the SAME `av_command::admin::serve` the real
/// `av-command` binary serves) over a real ledger with one real record -- returns its address.
async fn spawn_real_command_admin(name: &str) -> std::net::SocketAddr {
    let dir = tmp_dir(&format!("{name}-command-ledger"));
    let ledger = Ledger::open(&dir).expect("open ledger");
    let clock = TestClock::new(5_000);
    let t = CommandTransition {
        state: CommandState::Proposed as i32,
        tai_ns: 5_000,
        principal: "model-x".to_string(),
        reason: "reason".to_string(),
        ack_level: AckLevel::Unspecified as i32,
        delegation_id: String::new(),
    };
    ledger.append(CommandMeta::new("sat-bundle-it", "cmd-1", "mode", ""), t, None, None, None, &clock).expect("append");
    let state = Arc::new(AdminState { ledger: Arc::new(ledger), run_id: "run-command-real".to_string(), version: "0.1.0".to_string(), counters: Arc::new(Counters::new()) });

    // Bind-then-drop-then-rebind to discover a free ephemeral port first -- the same
    // convention `crates/av-command/tests/grpc_service.rs::TestServer::spawn_over_with_
    // service_roles` already documents and uses for this exact "hand an address, not a
    // listener, to a `serve` that binds internally" shape; the window between drop and
    // rebind is negligible for a single local test process on loopback.
    let addr: std::net::SocketAddr = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port").local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = av_command::admin::serve(addr, state).await;
    });
    addr
}

/// This crate's own new admin server, over a real evidence ledger with one real record,
/// configured to reach `command_admin_addr` (or `None`) for the `av_command` half of the
/// bundle -- returns its address and the [`TestIssuer`] its own [`AuthContext`] verifies
/// against (R5.1: [`mint_admin_token`] mints a real, accepted bearer token from it).
async fn spawn_real_gateway_admin(name: &str, command_admin_addr: Option<std::net::SocketAddr>) -> (std::net::SocketAddr, TestIssuer) {
    let dir = tmp_dir(&format!("{name}-gateway-evidence-ledger"));
    let ledger = Ledger::open(&dir).expect("open evidence ledger");
    let clock = TestClock::new(6_000);
    let t = CommandTransition {
        state: CommandState::Proposed as i32,
        tai_ns: 6_000,
        principal: "model-y".to_string(),
        reason: "evidence topic record".to_string(),
        ack_level: AckLevel::Unspecified as i32,
        delegation_id: String::new(),
    };
    ledger.append(CommandMeta::new("gateway-evidence:cmd-2", "cmd-2", "proposal-evidence", ""), t, None, None, None, &clock).expect("append");

    let issuer = TestIssuer::new();
    let issuer_config = Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap());
    let human_roles = Arc::new(RoleTable::from_config(&BTreeMap::from([("test-admin".to_string(), vec![WILDCARD.to_string()])])));
    let ladder = Arc::new(ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]));
    let auth = Arc::new(AuthContext::new(issuer_config, human_roles, Arc::new(RoleTable::default()), Arc::new(GroupClearanceMap::default()), ladder, Arc::new(TestClock::new(1_700_000_000_000_000_000))));

    let state = Arc::new(BundleState { evidence_ledger: Arc::new(ledger), counters: Arc::new(Counters::new()), run_id: "run-gateway-real".to_string(), version: "0.1.0".to_string(), command_admin_addr, auth });

    let addr: std::net::SocketAddr = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port").local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = av_gateway::admin::serve(addr, state).await;
    });
    (addr, issuer)
}

/// Connects to `addr`, retrying (cooperatively yielding between attempts, never sleeping --
/// question 199/this track's own "no test sleeps" rule) up to a bounded number of times: the
/// admin server this test just `tokio::spawn`ed re-binds `addr` asynchronously (the same
/// bind-then-drop-then-rebind convention `spawn_real_command_admin`/`spawn_real_gateway_admin`
/// use), so the very first connect attempt right after spawning can race ahead of that bind.
async fn connect_retrying(addr: std::net::SocketAddr) -> TcpStream {
    for _ in 0..200 {
        if let Ok(stream) = TcpStream::connect(addr).await {
            return stream;
        }
        tokio::task::yield_now().await;
    }
    panic!("could not connect to {addr} after 200 cooperative retries");
}

async fn get_raw(addr: std::net::SocketAddr, path: &str, token: &str) -> String {
    let mut stream = connect_retrying(addr).await;
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8(buf).unwrap()
}

fn body_of(raw: &str) -> String {
    raw.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or("").to_string()
}

/// **The A6/Part 3 acceptance test.** Both services really running, both admin HTTP surfaces
/// really serving: one call to `av-gateway`'s own `/admin/api/evidence/bundle` returns a real,
/// deterministic document naming both real sections.
#[tokio::test]
async fn one_call_collects_both_services_real_evidence() {
    let command_addr = spawn_real_command_admin("both-real").await;
    let (gateway_addr, issuer) = spawn_real_gateway_admin("both-real", Some(command_addr)).await;
    let token = mint_admin_token(&issuer);

    let raw = get_raw(gateway_addr, "/admin/api/evidence/bundle", &token).await;
    assert!(raw.starts_with("HTTP/1.1 200 OK"), "{raw}");
    let body = body_of(&raw);
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");

    // The av_gateway side: this process's OWN real evidence.
    assert_eq!(json["av_gateway"]["run_id"], "run-gateway-real");
    let gw_partitions = json["av_gateway"]["evidence_ledger_partitions"].as_array().unwrap();
    assert_eq!(gw_partitions.len(), 1);
    assert_eq!(gw_partitions[0]["partition"], "gateway-evidence:cmd-2");
    assert_eq!(gw_partitions[0]["records"], 1);

    // The av_command side: a REAL fetch of the REAL av-command admin server's real evidence,
    // not a placeholder -- named reachable, and carrying that service's own real partition.
    assert_eq!(json["av_command"]["reachable"], true);
    assert_eq!(json["av_command"]["evidence"]["run_id"], "run-command-real");
    let cmd_partitions = json["av_command"]["evidence"]["partitions"].as_array().unwrap();
    assert_eq!(cmd_partitions.len(), 1);
    assert_eq!(cmd_partitions[0]["partition"], "sat-bundle-it");
    assert!(json["av_command"]["evidence"]["fips"]["openssl_version"].as_str().unwrap().starts_with("OpenSSL"), "the real av-command FIPS posture must be present, not omitted");

    // ============================================================================
    // Determinism (ADR-004): calling the identical bundle route again, with no state
    // change in between, must reproduce byte-identical JSON text -- BTreeMap end to end,
    // never HashMap iteration order.
    // ============================================================================
    let raw2 = get_raw(gateway_addr, "/admin/api/evidence/bundle", &token).await;
    assert_eq!(body_of(&raw2), body, "the bundle must be byte-identical run to run with no state change in between");

    // ============================================================================
    // Never a token, a private key, or any raw credential (this task's own rule) --
    // checked against the REAL response bytes of two REAL running services, not by
    // inspecting the source alone.
    // ============================================================================
    assert!(!body.contains("PRIVATE KEY"), "{body}");
    assert!(!body.contains("BEGIN "), "a PEM block must never appear in an evidence bundle: {body}");
    assert!(!body.to_lowercase().contains("principal_token"), "{body}");
    assert!(!body.to_lowercase().contains("service_token"), "{body}");
    // R5.1/invariant G: the bearer token this very request was authenticated with must never
    // appear anywhere in the response body either.
    assert!(!body.contains(&token), "the real caller_token must never appear in the evidence bundle body: {body}");
}

/// The `av_command` side, unreachable (nothing bound at all), is a NAMED entry -- never an
/// omitted key -- and the `av_gateway` side is still fully present. This is this track's own
/// recurring defect shape (a failure that leaves no trace) turned inside out: the one thing
/// an evidence bundle must never do is drop a section silently.
#[tokio::test]
async fn an_unreachable_av_command_is_named_not_omitted() {
    // An address nothing listens on: bind, read it back, then drop the listener.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = listener.local_addr().unwrap();
    drop(listener);

    let (gateway_addr, issuer) = spawn_real_gateway_admin("unreachable", Some(dead_addr)).await;
    let raw = get_raw(gateway_addr, "/admin/api/evidence/bundle", &mint_admin_token(&issuer)).await;
    assert!(raw.starts_with("HTTP/1.1 200 OK"), "an unreachable av-command side must not fail the whole bundle call: {raw}");
    let body = body_of(&raw);
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");

    assert_eq!(json["av_command"]["reachable"], false);
    assert!(json["av_command"]["error"].as_str().unwrap().contains(&dead_addr.to_string()), "{}", json["av_command"]["error"]);
    // The av_gateway side is unaffected -- still fully populated.
    assert_eq!(json["av_gateway"]["run_id"], "run-gateway-real");
    assert_eq!(json["av_gateway"]["evidence_ledger_partitions"].as_array().unwrap().len(), 1);
}

/// As above, but `av-command`'s admin address was never configured at all
/// (`command_admin_addr: None`) -- the same named-not-omitted contract for the "never
/// deployed with one" case, not only the "deployed but down right now" case.
#[tokio::test]
async fn an_unconfigured_command_admin_address_is_named_not_omitted() {
    let (gateway_addr, issuer) = spawn_real_gateway_admin("unconfigured", None).await;
    let raw = get_raw(gateway_addr, "/admin/api/evidence/bundle", &mint_admin_token(&issuer)).await;
    let body = body_of(&raw);
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");

    assert_eq!(json["av_command"]["reachable"], false);
    assert!(json["av_command"]["error"].as_str().unwrap().contains("no --command-admin-bind"), "{}", json["av_command"]["error"]);
    assert_eq!(json["av_gateway"]["run_id"], "run-gateway-real");
}

/// Determinism holds across independently-built states too, not only across two calls to
/// the same running server: two fresh sets of real ledgers/servers built with identical
/// inputs (same records, same clock readings) produce byte-identical bundles.
#[tokio::test]
async fn two_independently_built_but_identical_deployments_produce_byte_identical_bundles() {
    let command_addr_a = spawn_real_command_admin("determinism-a").await;
    let (gateway_addr_a, issuer_a) = spawn_real_gateway_admin("determinism-a", Some(command_addr_a)).await;
    let command_addr_b = spawn_real_command_admin("determinism-b").await;
    let (gateway_addr_b, issuer_b) = spawn_real_gateway_admin("determinism-b", Some(command_addr_b)).await;

    let body_a = body_of(&get_raw(gateway_addr_a, "/admin/api/evidence/bundle", &mint_admin_token(&issuer_a)).await);
    let body_b = body_of(&get_raw(gateway_addr_b, "/admin/api/evidence/bundle", &mint_admin_token(&issuer_b)).await);

    // run_id differs by construction ("run-command-real"/"run-gateway-real" are identical
    // strings in both -- the only difference between deployments here is which ephemeral
    // port each pair of servers happened to bind, which never appears inside either body).
    assert_eq!(body_a, body_b, "two independently-built deployments with identical real inputs must produce byte-identical evidence bundles");
    let _: BTreeMap<String, serde_json::Value> = serde_json::from_str(&body_a).expect("sanity: the body is a real JSON object");
}
