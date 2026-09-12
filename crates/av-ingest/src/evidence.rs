//! The evidence surface, as data (`docs/edge-plan.md` milestone E3; ADR-004 question 63:
//! "exposes `/admin/api/evidence` for `secdeploy evidence` to collect"; "keeps
//! hash-chained audit ledgers with a `verify` endpoint").
//!
//! **No HTTP server this round.** [`evidence`] and [`verify_all`] are plain functions
//! over an [`Ingest`](crate::ingest::Ingest) reference, returning a `serde_json::Value`
//! and a map of `av_cdm::pb::ChainVerification` respectively -- exactly the two things
//! `crates/av-dynamics-service/src/admin.rs`'s `evidence_body`/`verify_body` already do
//! for that crate's own single evidence log, minus the HTTP framing around them. E3b (the
//! wire, deferred -- see this crate's `lib.rs` module doc) mounts these two functions on
//! `GET /admin/api/evidence` and `GET /admin/api/evidence/verify`, following `admin.rs`'s
//! own documented reasoning for a hand-rolled `GET`-only server over a plain
//! `tokio::net::TcpListener` rather than pulling in `axum`/`hyper` directly.
//!
//! Every map in [`evidence`]'s output is built from a `BTreeMap` before being handed to
//! `serde_json::to_value` (ADR-004's determinism rule -- "`BTreeMap` on output paths, sort
//! explicitly", exactly as `admin.rs::evidence_body` does it), so the JSON key order is
//! stable and independent of `HashMap`'s unspecified iteration order for every map-shaped
//! payload here (partitions, producers, and the top level itself).

use std::collections::BTreeMap;

use serde_json::Value;

use av_edge::hash;
use av_edge::pb;

use crate::ingest::Ingest;

fn rejection_counters_to_value(counters: &pb::RejectionCounters, shard_mismatch_count: u64) -> Value {
    let mut m: BTreeMap<&str, Value> = BTreeMap::new();
    m.insert("accepted", Value::from(counters.accepted));
    m.insert("bad_signature_count", Value::from(counters.bad_signature_count));
    m.insert("chain_break_count", Value::from(counters.chain_break_count));
    m.insert("chain_gap_count", Value::from(counters.chain_gap_count));
    m.insert("chain_head", Value::String(hash::hex_encode(&counters.chain_head)));
    m.insert("duplicate_count", Value::from(counters.duplicate_count));
    m.insert("mislabeled_count", Value::from(counters.mislabeled_count));
    m.insert("over_clearance_count", Value::from(counters.over_clearance_count));
    m.insert("shard_mismatch_count", Value::from(shard_mismatch_count));
    m.insert("stale_count", Value::from(counters.stale_count));
    m.insert("unsigned_count", Value::from(counters.unsigned_count));
    serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes")
}

fn identity_counters_to_value(counters: &av_edge::identity::IdentityCounters) -> Value {
    let mut m: BTreeMap<&str, Value> = BTreeMap::new();
    m.insert("accepted", Value::from(counters.accepted));
    m.insert("expired", Value::from(counters.expired));
    m.insert("malformed_pem", Value::from(counters.malformed_pem));
    m.insert("not_p384", Value::from(counters.not_p384));
    m.insert("not_yet_valid", Value::from(counters.not_yet_valid));
    m.insert("issuer_not_trusted", Value::from(counters.issuer_not_trusted));
    m.insert("openssl_error", Value::from(counters.openssl_error));
    serde_json::to_value(m).expect("BTreeMap<&str, Value> always serializes")
}

/// The rejection-count fields of one [`pb::RejectionCounters`], summed (the eight
/// `av_edge::chain`-owned kinds plus this crate's own `shard_mismatch_count`) -- what
/// [`evidence`]'s top-level `"rejected_total"` folds every producer's own total into.
fn rejected_total_for(counters: &pb::RejectionCounters, shard_mismatch_count: u64) -> u64 {
    counters.unsigned_count
        + counters.bad_signature_count
        + counters.chain_gap_count
        + counters.chain_break_count
        + counters.mislabeled_count
        + counters.over_clearance_count
        + counters.stale_count
        + counters.duplicate_count
        + shard_mismatch_count
}

/// Everything `GET /admin/api/evidence` (E3b) will report: each partition's chain head
/// and record count, every registered producer's rejection counters (all nine kinds --
/// see this module's doc for why `shard_mismatch_count` is folded in here rather than
/// living on `pb::RejectionCounters` itself), the aggregate identity counters, and the
/// platform-wide accepted/rejected totals.
pub fn evidence(ingest: &Ingest) -> Value {
    let mut partitions: BTreeMap<String, Value> = BTreeMap::new();
    for (shard_key, log) in ingest.partitions() {
        let mut p: BTreeMap<&str, Value> = BTreeMap::new();
        p.insert("chain_head", Value::String(hash::hex_encode(&log.tip_hash())));
        p.insert("record_count", Value::from(log.record_count()));
        partitions.insert(shard_key.clone(), serde_json::to_value(p).expect("BTreeMap<&str, Value> always serializes"));
    }

    let mut producers: BTreeMap<String, Value> = BTreeMap::new();
    let mut accepted_total: u64 = 0;
    let mut rejected_total: u64 = 0;
    for producer_id in ingest.producer_ids() {
        let counters = ingest.producer_counters(producer_id);
        let shard_mismatch_count = ingest.shard_mismatch_count(producer_id);
        accepted_total += counters.accepted;
        rejected_total += rejected_total_for(&counters, shard_mismatch_count);
        producers.insert(producer_id.clone(), rejection_counters_to_value(&counters, shard_mismatch_count));
    }

    let mut root: BTreeMap<&str, Value> = BTreeMap::new();
    root.insert("accepted_total", Value::from(accepted_total));
    root.insert("identity", identity_counters_to_value(&ingest.identity_counters()));
    root.insert("partitions", serde_json::to_value(partitions).expect("BTreeMap<String, Value> always serializes"));
    root.insert("producers", serde_json::to_value(producers).expect("BTreeMap<String, Value> always serializes"));
    root.insert("rejected_total", Value::from(rejected_total));
    serde_json::to_value(root).expect("BTreeMap<&str, Value> always serializes")
}

/// One `av_cdm::pb::ChainVerification` per partition this `Ingest` currently has open
/// (`GET /admin/api/evidence/verify` in E3b), keyed by `shard_key` in a `BTreeMap` for the
/// same determinism reason as [`evidence`]. A partition whose `verify()` itself hit an
/// I/O error is reported as a failed verification naming that error, rather than the
/// error propagating out of this function -- an evidence surface that can fail to render
/// because one partition's file briefly could not be read is worse than one that reports
/// "this one partition could not be verified" and still renders every other partition.
pub fn verify_all(ingest: &Ingest) -> BTreeMap<String, pb::ChainVerification> {
    let mut out = BTreeMap::new();
    for (shard_key, log) in ingest.partitions() {
        let verification = log.verify().unwrap_or_else(|e| pb::ChainVerification {
            producer_id: shard_key.clone(),
            ok: false,
            checked: 0,
            broken_at_sequence: 0,
            detail: format!("could not read partition log to verify it: {e}"),
        });
        out.insert(shard_key.clone(), verification);
    }
    out
}
