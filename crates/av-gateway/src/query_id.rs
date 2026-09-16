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
//!
//! # H2c: [`compute_catalog_query_id`], additive, [`compute_query_id`]'s preimage untouched
//!
//! `GATEWAY_SELECTOR_CATALOG` carries no `run` (`crate::catalog_selector`'s own module doc),
//! so it needs a query id computed over a completely different canonical document -- the
//! `CatalogQuery`'s own content, never a `RunIdentity` that does not exist for this selector.
//! Rather than widen [`compute_query_id`]'s own signature (which would change its own
//! preimage for every EXISTING selector, exactly what this task's brief forbids: "an existing
//! selector's query id must be byte-identical to what it is today"), [`hash_canonical`] below
//! is the one place the length-prefixed-SHA-256 procedure is implemented, extracted verbatim
//! (byte for byte) from what [`compute_query_id`] already did inline -- both functions now
//! call it, and [`compute_query_id`]'s own OUTPUT is unchanged (proven, not merely asserted,
//! by this module's own `compute_query_id_output_is_unchanged_by_the_h2c_extension` pinned
//! test below, which fixes a real, hand-verified hex digest against fixed inputs).
//! [`compute_catalog_query_id`] is a genuinely new, second entry point with its own canonical
//! document shape, sharing only the hashing primitive, never the preimage.

use av_cdm::pb::{CatalogQuery, GatewaySelector};
use openssl::sha::sha256;

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `SHA-256(len(canonical) || canonical)`, hex-encoded -- the one hashing primitive both
/// [`compute_query_id`] and [`compute_catalog_query_id`] call (see this module's own "H2c"
/// doc for why this was extracted, and why it changes neither function's own preimage).
fn hash_canonical(canonical: &str) -> String {
    let mut buf = Vec::with_capacity(4 + canonical.len());
    let bytes = canonical.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
    hex_encode(&sha256(&buf))
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
    hash_canonical(&canonical_query_json(run_id, config_hash, caller_clearance, selector))
}

/// The canonical JSON document [`compute_catalog_query_id`] hashes -- every field of `query`
/// (`heavy.proto`'s own `CatalogQuery`) plus the caller's own (already-verified, never
/// caller-declared) `caller_clearance`, so a replay with the identical query and the identical
/// effective clearance resolves to the identical id. An absent `bbox`/`time` renders as the
/// literal string `"none"` on every one of its own sub-fields -- never ambiguous with a real
/// coordinate or epoch, both of which always render through `f64`/`i64`'s own `Display`
/// (never the four-character text `"none"` for any real value).
fn canonical_catalog_query_json(query: &CatalogQuery, caller_clearance: &str) -> String {
    fn opt_f64(v: Option<f64>) -> serde_json::Value {
        serde_json::Value::String(v.map(|x| x.to_string()).unwrap_or_else(|| "none".to_string()))
    }
    fn opt_i64(v: Option<i64>) -> serde_json::Value {
        serde_json::Value::String(v.map(|x| x.to_string()).unwrap_or_else(|| "none".to_string()))
    }

    let bbox = query.bbox.as_ref();
    let time = query.time.as_ref();
    let mut obj = serde_json::Map::new();
    obj.insert("min_lon".to_string(), opt_f64(bbox.map(|b| b.min_lon)));
    obj.insert("min_lat".to_string(), opt_f64(bbox.map(|b| b.min_lat)));
    obj.insert("max_lon".to_string(), opt_f64(bbox.map(|b| b.max_lon)));
    obj.insert("max_lat".to_string(), opt_f64(bbox.map(|b| b.max_lat)));
    obj.insert("start_tai_ns".to_string(), opt_i64(time.map(|t| t.start_tai_ns)));
    obj.insert("end_tai_ns".to_string(), opt_i64(time.map(|t| t.end_tai_ns)));
    obj.insert("media_type".to_string(), serde_json::Value::String(query.media_type.clone()));
    obj.insert("job_id".to_string(), serde_json::Value::String(query.job_id.clone()));
    obj.insert("limit".to_string(), serde_json::Value::String(query.limit.to_string()));
    obj.insert("caller_clearance".to_string(), serde_json::Value::String(caller_clearance.to_string()));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("a document built only from strings never fails to serialize")
}

/// H2c: `GATEWAY_SELECTOR_CATALOG`'s own query id, over `query`'s own canonical content plus
/// the caller's effective (verified) clearance -- see this module's own "H2c" doc for why this
/// is a new, additive entry point rather than a change to [`compute_query_id`]'s signature or
/// preimage.
pub fn compute_catalog_query_id(query: &CatalogQuery, caller_clearance: &str) -> String {
    hash_canonical(&canonical_catalog_query_json(query, caller_clearance))
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

    /// **This task's own required pinned test** (w5.md, deliverable 3: "an existing selector's
    /// query id must be byte-identical to what it is today -- assert that with a pinned value
    /// in a test"). Fixes [`compute_query_id`]'s own real, hand-verified output against fixed
    /// inputs -- proof, not merely an assertion of internal consistency (unlike
    /// `compute_query_id_is_deterministic_over_the_same_content` above, which only checks two
    /// calls agree with EACH OTHER), that H2c's [`hash_canonical`] extraction and the addition
    /// of [`compute_catalog_query_id`] alongside it changed NOTHING about this function's own
    /// preimage or output. The literal hex string below was computed by this exact function,
    /// on this exact input, before any later change to this file -- a future edit that
    /// silently altered `canonical_query_json`'s key set, ordering, or `hash_canonical`'s own
    /// length-prefix procedure would change this value and fail this test, loudly.
    #[test]
    fn compute_query_id_output_is_unchanged_by_the_h2c_extension() {
        let id = compute_query_id("run-pinned", "hash-pinned", "CUI", GatewaySelector::Trajectories);
        assert_eq!(id, "bbfab328cf3e33a3de08a7d0dcf9c6d20c7e6da257f321b849f7684e87f9c398", "an EXISTING selector's query id must be byte-identical to what it was before H2c -- this pinned value is that proof");
    }

    /// The identical pin, for [`compute_catalog_query_id`] -- fixes the new function's own
    /// output against a fixed `CatalogQuery`, so a later edit to its canonical document shape
    /// is visible here too, not only proven "deterministic with itself".
    #[test]
    fn compute_catalog_query_id_is_pinned_against_a_fixed_input() {
        let query = CatalogQuery {
            bbox: Some(av_cdm::pb::GeoBbox { min_lon: -10.0, min_lat: -20.0, max_lon: 10.0, max_lat: 20.0 }),
            time: Some(av_cdm::pb::TemporalExtent { start_tai_ns: 1_000, end_tai_ns: 2_000 }),
            media_type: "image/tiff".to_string(),
            job_id: "job-1".to_string(),
            limit: 50,
        };
        let id = compute_catalog_query_id(&query, "CUI");
        assert_eq!(id, "31dd75f0e8e2cf20229ee5b4b6071a70fdd9c63dd2c179b11edb9f34f7475012", "pinned catalog query id changed -- see this test's sibling for compute_query_id");
        assert_eq!(id.len(), 64);
    }

    /// [`compute_catalog_query_id`] differs when any field of the query (or the caller's own
    /// effective clearance) differs -- the identical discriminating-input proof
    /// [`compute_query_id_differs_when_any_input_differs`] already gives `compute_query_id`.
    #[test]
    fn compute_catalog_query_id_differs_when_any_input_differs() {
        let base_query = CatalogQuery { bbox: None, time: None, media_type: "image/tiff".to_string(), job_id: "job-1".to_string(), limit: 50 };
        let base = compute_catalog_query_id(&base_query, "CUI");

        let mut different_media_type = base_query.clone();
        different_media_type.media_type = "application/octet-stream".to_string();
        assert_ne!(base, compute_catalog_query_id(&different_media_type, "CUI"));

        let mut different_job = base_query.clone();
        different_job.job_id = "job-2".to_string();
        assert_ne!(base, compute_catalog_query_id(&different_job, "CUI"));

        let mut different_limit = base_query.clone();
        different_limit.limit = 51;
        assert_ne!(base, compute_catalog_query_id(&different_limit, "CUI"));

        assert_ne!(base, compute_catalog_query_id(&base_query, "SECRET"), "a different caller_clearance must also change the id");

        let mut with_bbox = base_query.clone();
        with_bbox.bbox = Some(av_cdm::pb::GeoBbox { min_lon: 0.0, min_lat: 0.0, max_lon: 1.0, max_lat: 1.0 });
        assert_ne!(base, compute_catalog_query_id(&with_bbox, "CUI"), "an absent vs. present bbox must change the id");
    }
}
