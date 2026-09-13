//! D5: a query id, deterministic and derived from the query's own canonical content --
//! never a random id, never a counter that depends on wall-clock arrival order, so a
//! replay resolves it exactly the same way this gateway did.
//!
//! Mirrors `crates/av-command/src/policy.rs`'s `canonical_input_json`/`compute_decision_id`
//! convention byte for byte: one function builds the canonical JSON document (sorted,
//! fixed key set, no insignificant whitespace -- `serde_json::to_string` over a
//! `serde_json::Map` built key by key, never derived from a struct whose field order could
//! silently change), and a second hashes it with a length-prefixed preimage via
//! `openssl::sha::sha256` (ADR-004's crypto rule: SHA-256 only, through the system OpenSSL,
//! no `sha2`, no `ring`).
//!
//! Deliberately excludes any epoch or clock reading from the preimage (unlike
//! `compute_decision_id`, which folds in `evaluated_tai_ns`): D5 requires the SAME query
//! (same run identity, same caller clearance, same selector) to resolve to the SAME query
//! id no matter when it is issued or replayed, which an epoch in the preimage would break.

use av_cdm::pb::GatewaySelector;
use openssl::sha::sha256;

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The exact canonical JSON document a query id is computed over. Public so a test (or a
/// replay tool) can reproduce the preimage independently of [`compute_query_id`] itself.
pub fn canonical_query_json(run_id: &str, config_hash: &str, caller_clearance: &str, selector: GatewaySelector) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("run_id".to_string(), serde_json::Value::String(run_id.to_string()));
    obj.insert("config_hash".to_string(), serde_json::Value::String(config_hash.to_string()));
    obj.insert("caller_clearance".to_string(), serde_json::Value::String(caller_clearance.to_string()));
    obj.insert("selector".to_string(), serde_json::Value::String(selector.as_str_name().to_string()));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("a document built only from strings never fails to serialize")
}

/// `SHA-256(len(canonical_json) || canonical_json)`, hex-encoded -- see the module doc.
pub fn compute_query_id(run_id: &str, config_hash: &str, caller_clearance: &str, selector: GatewaySelector) -> String {
    let canonical = canonical_query_json(run_id, config_hash, caller_clearance, selector);
    let mut buf = Vec::with_capacity(4 + canonical.len());
    let bytes = canonical.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
    hex_encode(&sha256(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_query_id_is_deterministic_over_the_same_content() {
        let a = compute_query_id("run-1", "hash-1", "CUI", GatewaySelector::All);
        let b = compute_query_id("run-1", "hash-1", "CUI", GatewaySelector::All);
        assert_eq!(a, b);
    }

    #[test]
    fn compute_query_id_differs_when_any_input_differs() {
        let base = compute_query_id("run-1", "hash-1", "CUI", GatewaySelector::All);
        assert_ne!(base, compute_query_id("run-2", "hash-1", "CUI", GatewaySelector::All));
        assert_ne!(base, compute_query_id("run-1", "hash-2", "CUI", GatewaySelector::All));
        assert_ne!(base, compute_query_id("run-1", "hash-1", "SECRET", GatewaySelector::All));
        assert_ne!(base, compute_query_id("run-1", "hash-1", "CUI", GatewaySelector::Trajectories));
    }

    #[test]
    fn compute_query_id_is_a_64_character_lowercase_hex_string() {
        let id = compute_query_id("run-1", "hash-1", "CUI", GatewaySelector::Scores);
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
