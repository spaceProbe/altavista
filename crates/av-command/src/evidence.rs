//! What `/admin/api/evidence` and `/admin/api/evidence/verify` report (ADR-004 question 63),
//! built from [`crate::ledger::Ledger`] (the durable log *is* the evidence for this crate --
//! ADR-004: "for the engine and command services the durable log itself is the ledger,
//! chained per partition") and [`crate::fips`]. `src/admin.rs` owns the HTTP transport (the
//! hand-rolled `tokio::net::TcpListener` server); this module owns what goes in the body,
//! mirroring the split `av-dynamics-service` draws between `evidence.rs` (the log) and
//! `admin.rs` (the server) -- adapted here because, in this crate, the log itself lives in
//! [`crate::ledger`], not in this module, so this module's job is composing the *report*
//! (ledger summary + FIPS posture + crate identity) rather than owning a log of its own.
//!
//! `BTreeMap<&str, serde_json::Value>` for every JSON object built here (ADR-004's
//! determinism rule: "`BTreeMap` on output paths, sort explicitly"), matching
//! `av-dynamics-service/src/admin.rs`'s own convention -- the JSON key order is stable and
//! independent of Rust's (unspecified) struct-field iteration order.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::counters::Counters;
use crate::fips;
use crate::ledger::Ledger;

/// Everything the two admin routes need. Built once at server startup and shared (via
/// `Arc`) across every accepted connection.
pub struct AdminState {
    pub ledger: Arc<Ledger>,
    /// One id per server process -- not a credential, just a correlation tag (matches
    /// `av-dynamics-service::admin::AdminState::run_id`'s role). This crate never generates
    /// this randomly on its own path; the caller (the eventual `av-command` service binary)
    /// supplies it, exactly like every other id in this crate (see `crate::ledger`'s module
    /// doc on why: no test and no service path generates an id at random).
    pub run_id: String,
    /// This crate's own `CARGO_PKG_VERSION`.
    pub version: String,
    /// R3.1: ADR-004's "everything rejected is counted" primitive, shared with
    /// [`crate::service::CommandAuthorityServiceImpl`] (the same `Arc`, constructed once by
    /// `src/bin/av-command.rs`) -- exposed here so the counts are observable evidence, not
    /// merely in-memory state invisible outside this process.
    pub counters: Arc<Counters>,
}

/// The `GET /admin/api/evidence` body: this crate's version, every partition's chain head
/// and record count, the FIPS posture `crate::fips::detect` observed, and (R3.1) every
/// refusal this process has counted so far, sorted by code.
pub fn evidence_body(state: &AdminState) -> std::io::Result<Value> {
    let partitions = state.ledger.partitions()?;
    let posture = fips::detect();

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
    m.insert("fips", serde_json::to_value(&posture).expect("FipsPosture always serializes"));
    m.insert("partitions", Value::Array(partition_values));
    // R3.1: `Counters::snapshot` is already a sorted `BTreeMap<&'static str, u64>` (ADR-004's
    // determinism rule) -- serializes directly to a sorted JSON object, no re-sorting needed.
    m.insert("refusals", serde_json::to_value(state.counters.snapshot()).expect("Counters::snapshot always serializes"));
    m.insert("run_id", Value::String(state.run_id.clone()));
    m.insert("version", Value::String(state.version.clone()));
    Ok(serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes"))
}

/// The `GET /admin/api/evidence/verify` body: `crate::ledger::Ledger::verify` run over
/// every partition the ledger currently has a file for, plus an overall `ok` that is the
/// logical AND of every partition's own `ok`.
pub fn verify_body(state: &AdminState) -> std::io::Result<Value> {
    let partitions = state.ledger.partitions()?;
    let mut all_ok = true;
    let mut results = Vec::with_capacity(partitions.len());
    for p in &partitions {
        let result = state.ledger.verify(&p.partition)?;
        all_ok &= result.ok;
        results.push(serde_json::to_value(SerializableChainVerification::from(&result)).expect("ChainVerification always serializes"));
    }

    let mut m: BTreeMap<&str, Value> = BTreeMap::new();
    m.insert("ok", Value::Bool(all_ok));
    m.insert("partitions", Value::Array(results));
    Ok(serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes"))
}

/// `av_cdm::pb::ChainVerification` is a `prost::Message`, not `serde::Serialize` -- this is
/// a small serializable mirror of its four fields, field order matching
/// `altavista.v1.ChainVerification` (`envelope.proto`).
#[derive(Debug, serde::Serialize)]
struct SerializableChainVerification {
    producer_id: String,
    ok: bool,
    checked: u64,
    broken_at_sequence: u64,
    detail: String,
}

impl From<&av_cdm::pb::ChainVerification> for SerializableChainVerification {
    fn from(v: &av_cdm::pb::ChainVerification) -> Self {
        Self { producer_id: v.producer_id.clone(), ok: v.ok, checked: v.checked, broken_at_sequence: v.broken_at_sequence, detail: v.detail.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::ledger::CommandMeta;
    use av_cdm::pb::{AckLevel, CommandState, CommandTransition};

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
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), t, None, None, &clock).unwrap();
        ledger
    }

    #[test]
    fn evidence_body_reports_version_run_id_and_the_ledgers_partitions() {
        let dir = std::env::temp_dir().join(format!("av-command-evidence-test-body-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = ledger_with_one_partition(&dir);
        let state = AdminState { ledger: Arc::new(ledger), run_id: "run-42".to_string(), version: "0.1.0".to_string(), counters: Arc::new(Counters::new()) };

        let body = evidence_body(&state).unwrap();
        assert_eq!(body["version"], "0.1.0");
        assert_eq!(body["run_id"], "run-42");
        let partitions = body["partitions"].as_array().unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0]["partition"], "sat-1");
        assert_eq!(partitions[0]["records"], 1);
        assert!(body["fips"]["openssl_version"].as_str().unwrap().starts_with("OpenSSL"));
        assert_eq!(body["refusals"], serde_json::json!({}), "no refusal has been counted yet");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R3.1: the counters this process has already recorded show up in the evidence body,
    /// sorted by code -- observable evidence, not merely in-memory state.
    #[test]
    fn evidence_body_reports_a_refusal_this_process_already_counted() {
        let dir = std::env::temp_dir().join(format!("av-command-evidence-test-refusals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = ledger_with_one_partition(&dir);
        let counters = Arc::new(Counters::new());
        counters.record(&crate::oidc::TokenError::MissingSubject);
        counters.record(&crate::oidc::TokenError::MissingSubject);
        counters.record(&crate::authz::ServiceAuthzError::ServiceRoleNotGranted { groups: vec![], rpc: crate::authz::ServiceRpc::Dispatch });
        let state = AdminState { ledger: Arc::new(ledger), run_id: "run-refusals".to_string(), version: "0.1.0".to_string(), counters };

        let body = evidence_body(&state).unwrap();
        assert_eq!(body["refusals"]["token_missing_subject"], 2);
        assert_eq!(body["refusals"]["service_role_not_granted"], 1);
        assert_eq!(body["refusals"].as_object().unwrap().len(), 2, "only the two codes actually recorded appear, sorted by code: {:?}", body["refusals"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_body_reports_ok_true_for_a_clean_ledger() {
        let dir = std::env::temp_dir().join(format!("av-command-evidence-test-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = ledger_with_one_partition(&dir);
        let state = AdminState { ledger: Arc::new(ledger), run_id: "run-1".to_string(), version: "0.1.0".to_string(), counters: Arc::new(Counters::new()) };

        let body = verify_body(&state).unwrap();
        assert_eq!(body["ok"], true);
        let partitions = body["partitions"].as_array().unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0]["producer_id"], "sat-1");
        assert_eq!(partitions[0]["ok"], true);
        assert_eq!(partitions[0]["checked"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
