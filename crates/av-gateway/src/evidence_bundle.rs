//! R3.6/A6, Part 3: "the evidence bundle from both services collects in one call"
//! (`docs/aiplane-plan.md` milestone A6). Today `av-command` serves its own evidence at
//! `GET /admin/api/evidence` (`crates/av-command/src/evidence.rs`) and this crate has its own
//! evidence ledger partition ([`crate::evidence::EvidenceRecorder`]) and its own [`Counters`]
//! -- but nothing collects both into one document. [`bundle_body`] is that one call: a real,
//! deterministic, `BTreeMap`-keyed JSON document built from THIS process's own real ledger and
//! counters, plus a real HTTP fetch of the configured `av-command` service's own
//! `/admin/api/evidence` -- never a hand-assembled literal standing in for either half.
//!
//! ## Why this lives here, not in `av-command`
//!
//! `av-command` is the lower crate in this workspace's dependency graph (`av-gateway` already
//! depends on it; the reverse is impossible without a cycle -- `crates/av-command/src/
//! counters.rs`'s own module doc restates this same rule for why `Counters` itself lives
//! there). Collecting "both services" therefore has to happen from the crate that can see
//! both -- this one -- reaching OUT to the other over its already-public, already-loopback-
//! only HTTP surface, never by teaching the foundational crate about its own consumer.
//!
//! ## Never a token, a private key, or any raw credential
//!
//! Every value this module puts into the bundle comes from [`crate::counters::Counters::
//! snapshot`] (refusal codes and counts -- `&'static str` keys this crate itself defined, no
//! caller-supplied string), [`av_command::ledger::Ledger::partitions`] (partition names, hex
//! chain heads, record counts -- no record body, no key material), and the identical shape
//! `av-command`'s own `/admin/api/evidence` already returns (which that crate's own control
//! matrix and test suite already establish carries no secret -- FIPS posture, ledger
//! partition summaries, run id, version, refusal counts). Neither this module nor
//! `av-command`'s own evidence route ever serializes a `principal_token`/`service_token`, a
//! signing key, or any `TestIssuer`-minted material -- `crates/av-gateway/tests/
//! evidence_bundle.rs`'s own test asserts this directly against the real, running services'
//! real response bytes, not by inspection of the source alone.
//!
//! ## Determinism (ADR-004)
//!
//! `BTreeMap<&str, Value>` end to end, exactly like `crates/av-command/src/evidence.rs`'s own
//! convention -- the JSON key order is stable and independent of Rust's own (unspecified)
//! struct-field iteration order, and [`Counters::snapshot`]/[`Ledger::partitions`] are
//! themselves already sorted. No wall clock, no random id: the only "new" value this module
//! introduces beyond what each side already reports is the fetch outcome itself
//! (`"reachable"`/`"error"`), which is a fact about the call just made, not a generated one.
//!
//! ## What "could not collect" looks like
//!
//! `av-command`'s side of the bundle is `{"reachable": false, "error": "<detail>"}` whenever
//! no `--command-admin-bind` was configured, the connection is refused, the response is not
//! `200 OK`, or the body does not parse as JSON -- a NAMED entry, never an omitted key. This is
//! this track's own recurring defect shape (a failure that leaves no trace) turned inside out:
//! the one thing an evidence bundle must never do is drop a section silently.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use av_command::ledger::Ledger;

use crate::auth::AuthContext;
use crate::counters::Counters;

/// Everything [`bundle_body`] needs: this process's OWN real evidence (ledger + counters),
/// plus where to reach the `av-command` service's own admin surface (`None` when this
/// deployment never configured one -- a real, honest state, not an error condition by
/// itself, though the resulting bundle still names it unreachable rather than omitting the
/// section).
pub struct BundleState {
    pub evidence_ledger: Arc<Ledger>,
    pub counters: Arc<Counters>,
    pub run_id: String,
    pub version: String,
    /// The `av-command` service's own `/admin/api/evidence` listener, already validated
    /// loopback-only by [`av_command::service::resolve_loopback_bind_address`] at CLI-parse
    /// time (question 155: this crate never dials an arbitrary caller-supplied address here
    /// either).
    pub command_admin_addr: Option<SocketAddr>,
    /// R5.1/question 208(b): `crate::admin::handle_connection` authenticates every caller of
    /// `GET /admin/api/evidence/bundle` through this, before [`bundle_body`] ever runs -- see
    /// `crate::admin`'s own module doc for the full contract (AU 3.3.9).
    pub auth: Arc<AuthContext>,
}

/// A real `GET /admin/api/evidence` against `addr` over a real loopback socket (this is a
/// same-host, kernel-loopback connection between two processes this workspace itself spawned
/// -- not "network at test time", the identical reasoning `crates/av-command/tests/
/// grpc_service.rs`'s own module doc already gives for its loopback sockets), parsed as JSON.
/// Every failure mode -- connection refused, a non-`200` status, a body that does not decode
/// as UTF-8, a body that does not parse as JSON -- is a named `Err(String)`, never a panic and
/// never treated as "empty evidence".
async fn fetch_command_evidence(addr: SocketAddr) -> Result<Value, String> {
    let mut stream = TcpStream::connect(addr).await.map_err(|e| format!("connect to {addr}: {e}"))?;
    stream
        .write_all(b"GET /admin/api/evidence HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .map_err(|e| format!("write request to {addr}: {e}"))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.map_err(|e| format!("read response from {addr}: {e}"))?;
    let text = String::from_utf8(buf).map_err(|e| format!("response from {addr} was not valid UTF-8: {e}"))?;
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");
    let status_line = head.lines().next().unwrap_or("");
    if !status_line.contains("200") {
        return Err(format!("{addr} answered {status_line:?}, not 200 OK"));
    }
    serde_json::from_str(body).map_err(|e| format!("{addr}'s response body did not parse as JSON: {e}"))
}

/// This process's OWN real evidence: its evidence ledger's partitions (the identical
/// `Ledger::partitions` primitive `av-command`'s own evidence route uses, over THIS crate's
/// dedicated evidence-topic ledger -- `crate::evidence`'s module doc), and its own [`Counters::
/// snapshot`] -- both already sorted (`BTreeMap`), never re-sorted here.
fn av_gateway_evidence_body(state: &BundleState) -> std::io::Result<Value> {
    let partitions = state.evidence_ledger.partitions()?;
    let partition_values: Vec<Value> = partitions
        .iter()
        .map(|p| {
            let mut m: BTreeMap<&str, Value> = BTreeMap::new();
            m.insert("chain_head", Value::String(p.chain_head.clone()));
            m.insert("partition", Value::String(p.partition.clone()));
            m.insert("records", Value::from(p.records));
            serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes")
        })
        .collect();

    let mut m: BTreeMap<&str, Value> = BTreeMap::new();
    m.insert("evidence_ledger_partitions", Value::Array(partition_values));
    m.insert("refusals", serde_json::to_value(state.counters.snapshot()).expect("Counters::snapshot always serializes"));
    m.insert("run_id", Value::String(state.run_id.clone()));
    m.insert("version", Value::String(state.version.clone()));
    Ok(serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes"))
}

/// The one call: `av_gateway` (this process's own real evidence, built in-process, never
/// fetched over the wire from itself) plus `av_command` (a real HTTP fetch of the configured
/// `av-command` service's own `/admin/api/evidence`, wrapped in `{"reachable": ..., ...}` so
/// an unreachable or unconfigured `av-command` side is a NAMED entry, never an omitted key).
/// `BTreeMap` end to end -- see the module doc's "Determinism" section.
pub async fn bundle_body(state: &BundleState) -> std::io::Result<Value> {
    let av_gateway_value = av_gateway_evidence_body(state)?;

    let av_command_value = match state.command_admin_addr {
        None => serde_json::json!({"reachable": false, "error": "no --command-admin-bind configured for this av-gateway process"}),
        Some(addr) => match fetch_command_evidence(addr).await {
            Ok(evidence) => serde_json::json!({"reachable": true, "evidence": evidence}),
            Err(detail) => serde_json::json!({"reachable": false, "error": detail}),
        },
    };

    let mut m: BTreeMap<&str, Value> = BTreeMap::new();
    m.insert("av_command", av_command_value);
    m.insert("av_gateway", av_gateway_value);
    Ok(serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::GroupClearanceMap;
    use av_command::authz::RoleTable;
    use av_command::clock::TestClock;
    use av_command::ledger::CommandMeta;
    use av_command::oidc::IssuerConfig;
    use av_command::test_support::TestIssuer;
    use av_cdm::pb::{AckLevel, CommandState, CommandTransition};

    /// A minimal, real [`AuthContext`] for tests in this module that construct a
    /// [`BundleState`] directly (this module's own focus is `bundle_body`'s content, not
    /// authentication -- `crate::admin`'s own test module is where the admin route's auth gate
    /// itself is exercised end to end).
    fn test_auth_context() -> Arc<AuthContext> {
        Arc::new(AuthContext::new(
            Arc::new(IssuerConfig::from_public_key_pem("https://sso.test.example/", "av-gateway", TestIssuer::new().public_key_pem()).unwrap()),
            Arc::new(RoleTable::default()),
            Arc::new(RoleTable::default()),
            Arc::new(GroupClearanceMap::default()),
            Arc::new(TestClock::new(1_000)),
        ))
    }

    fn ledger_with_one_partition(dir: &std::path::Path) -> Ledger {
        let ledger = Ledger::open(dir).unwrap();
        let clock = TestClock::new(1_000);
        let t = CommandTransition {
            state: CommandState::Proposed as i32,
            tai_ns: 1_000,
            principal: "model-x".to_string(),
            reason: "reason".to_string(),
            ack_level: AckLevel::Unspecified as i32,
            delegation_id: String::new(),
        };
        ledger.append(CommandMeta::new("gateway-evidence:cmd-1", "cmd-1", "proposal-evidence", ""), t, None, None, None, &clock).unwrap();
        ledger
    }

    /// Unconfigured `command_admin_addr` (`None`) is a NAMED, unreachable entry -- never an
    /// omitted key -- while this process's own `av_gateway` section is fully populated.
    #[tokio::test]
    async fn bundle_body_names_an_unconfigured_command_admin_address_rather_than_omitting_it() {
        let dir = std::env::temp_dir().join(format!("av-gateway-bundle-test-unconfigured-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = ledger_with_one_partition(&dir);
        let state = BundleState { evidence_ledger: Arc::new(ledger), counters: Arc::new(Counters::new()), run_id: "run-1".to_string(), version: "0.1.0".to_string(), command_admin_addr: None, auth: test_auth_context() };

        let body = bundle_body(&state).await.unwrap();
        assert_eq!(body["av_command"]["reachable"], false);
        assert!(body["av_command"]["error"].as_str().unwrap().contains("no --command-admin-bind"));
        assert_eq!(body["av_gateway"]["run_id"], "run-1");
        let partitions = body["av_gateway"]["evidence_ledger_partitions"].as_array().unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0]["partition"], "gateway-evidence:cmd-1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An address nothing listens on is a NAMED, unreachable entry too (connection refused),
    /// not an omitted key and not a panic.
    #[tokio::test]
    async fn bundle_body_names_a_connection_refused_rather_than_panicking() {
        let dir = std::env::temp_dir().join(format!("av-gateway-bundle-test-refused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = ledger_with_one_partition(&dir);
        // Bind, read the address back, then drop the listener immediately -- an address on
        // loopback that is very likely refusing connections by the time this test dials it,
        // without depending on any globally-reserved "surely nothing listens here" port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let state = BundleState { evidence_ledger: Arc::new(ledger), counters: Arc::new(Counters::new()), run_id: "run-1".to_string(), version: "0.1.0".to_string(), command_admin_addr: Some(addr), auth: test_auth_context() };
        let body = bundle_body(&state).await.unwrap();
        assert_eq!(body["av_command"]["reachable"], false);
        assert!(body["av_command"]["error"].as_str().unwrap().contains(&addr.to_string()), "{}", body["av_command"]["error"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
