//! Canonical hashing for `DesignReferenceMission` / `SosConfiguration` / `SystemDefinition`
//! (question 87's "compute and verify the canonical hashes").
//!
//! **Scheme.** Clear the message's own `hash` field, encode it with `prost::Message`'s
//! canonical binary encoding (field-number order; every map field in `av_cdm::pb` is a
//! `BTreeMap` -- `crates/av-cdm/build.rs`'s `.btree_map(["."])` -- so map entries serialize in
//! sorted-key order too, matching protobuf's own deterministic-serialization contract), then
//! SHA-256 the bytes and record the hex digest. This is exactly the convention
//! `tests/test_cdm_v1.py::test_drm_with_ric_frame_and_mixed_bindings_round_trips` already
//! uses on the Python side (`hashlib.sha256(msg.SerializeToString(deterministic=True))` with
//! `hash` cleared first) -- `prost::Message::encode_to_vec` is this crate's equivalent of
//! `SerializeToString(deterministic=True)`.
//!
//! **Why `sha2`, not `openssl`.** The task brief allows `sha2` "if it is already a dependency
//! somewhere in this workspace" -- it is: `crates/av-dynamics/src/lib.rs::settings_hash` uses
//! it already (pure Rust, no bundled C crypto, matching ADR-004's crypto rule), so this module
//! reuses the same crate rather than pulling in `openssl` for a second, unrelated hashing need.

use sha2::{Digest, Sha256};

use av_cdm::pb::{DesignReferenceMission, SosConfiguration, SystemDefinition};

use super::DrmError;

/// SHA-256 hex digest of arbitrary bytes -- `pub(crate)` (question 175, M25.4a) so `executor::
/// execute` can reuse this crate's one SHA-256 helper for `RunProducts.port_traffic_hash`
/// (the `PortTrafficLog` sidecar's own hash) rather than hand-rolling a second call to `sha2`
/// or pulling in a new crate. Every other caller in this module still goes through the
/// `canonical_*_hash` wrappers below, which additionally clear a message's own `hash` field
/// first -- this is the one raw primitive underneath all of them.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The canonical hash of `drm`, as if its own `hash` field were empty.
pub fn canonical_drm_hash(drm: &DesignReferenceMission) -> String {
    let mut cleared = drm.clone();
    cleared.hash.clear();
    sha256_hex(&prost::Message::encode_to_vec(&cleared))
}

/// The canonical hash of `sos`, as if its own `hash` field were empty.
pub fn canonical_sos_hash(sos: &SosConfiguration) -> String {
    let mut cleared = sos.clone();
    cleared.hash.clear();
    sha256_hex(&prost::Message::encode_to_vec(&cleared))
}

/// The canonical hash of `sys`, as if its own `hash` field were empty.
pub fn canonical_system_hash(sys: &SystemDefinition) -> String {
    let mut cleared = sys.clone();
    cleared.hash.clear();
    sha256_hex(&prost::Message::encode_to_vec(&cleared))
}

/// Verify `drm.hash` against its own canonical hash, refusing (never merely warning) on a
/// mismatch -- a run never starts on a DRM whose declared hash does not match its content.
/// Returns the computed hash on success, for `executor::execute` to reuse (`Trajectory
/// .config_hash`) without hashing the message a second time.
pub fn verify_drm_hash(drm: &DesignReferenceMission) -> Result<String, DrmError> {
    let computed = canonical_drm_hash(drm);
    if drm.hash != computed {
        return Err(DrmError::HashMismatch { artifact: "DesignReferenceMission", id: drm.id.clone(), declared: drm.hash.clone(), computed });
    }
    Ok(computed)
}

/// Like [`verify_drm_hash`], for a `SosConfiguration`.
pub fn verify_sos_hash(sos: &SosConfiguration) -> Result<String, DrmError> {
    let computed = canonical_sos_hash(sos);
    if sos.hash != computed {
        return Err(DrmError::HashMismatch { artifact: "SosConfiguration", id: sos.id.clone(), declared: sos.hash.clone(), computed });
    }
    Ok(computed)
}

/// Like [`verify_drm_hash`], for a `SystemDefinition`. `id` is the caller's own lookup key
/// (usually equal to `sys.id`) so a mismatch's error message names the key a caller supplied
/// `sys` under, not just `sys.id` (which is itself part of what could be tampered).
pub fn verify_system_hash(id: &str, sys: &SystemDefinition) -> Result<String, DrmError> {
    let computed = canonical_system_hash(sys);
    if sys.hash != computed {
        return Err(DrmError::HashMismatch { artifact: "SystemDefinition", id: id.to_string(), declared: sys.hash.clone(), computed });
    }
    Ok(computed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_excludes_the_hash_field_itself() {
        let mut drm = DesignReferenceMission { id: "d1".to_string(), ..Default::default() };
        let h1 = canonical_drm_hash(&drm);
        assert_eq!(h1.len(), 64, "hex-encoded SHA-256 is 64 chars");
        drm.hash = h1.clone();
        // Setting drm.hash to its own correct value must not change what canonical_drm_hash
        // computes (it clears the field before hashing) -- otherwise no hash could ever be
        // both stored and self-consistent.
        assert_eq!(canonical_drm_hash(&drm), h1);
        drm.hash = "not-the-real-hash".to_string();
        assert_eq!(canonical_drm_hash(&drm), h1, "the field's own prior content must never affect the computed hash");
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let a = DesignReferenceMission { id: "a".to_string(), ..Default::default() };
        let b = DesignReferenceMission { id: "b".to_string(), ..Default::default() };
        assert_ne!(canonical_drm_hash(&a), canonical_drm_hash(&b));
    }

    #[test]
    fn map_field_ordering_does_not_affect_the_hash() {
        use std::collections::BTreeMap;
        let mut seeds_a = BTreeMap::new();
        seeds_a.insert("z".to_string(), 1u64);
        seeds_a.insert("a".to_string(), 2u64);
        let mut seeds_b = BTreeMap::new();
        seeds_b.insert("a".to_string(), 2u64);
        seeds_b.insert("z".to_string(), 1u64);
        let drm_a = DesignReferenceMission {
            id: "d".to_string(),
            scenario: Some(av_cdm::pb::Scenario { seeds: seeds_a, ..Default::default() }),
            ..Default::default()
        };
        let drm_b = DesignReferenceMission {
            id: "d".to_string(),
            scenario: Some(av_cdm::pb::Scenario { seeds: seeds_b, ..Default::default() }),
            ..Default::default()
        };
        assert_eq!(canonical_drm_hash(&drm_a), canonical_drm_hash(&drm_b), "BTreeMap insertion order must not leak into the hash");
    }

    #[test]
    fn verify_drm_hash_accepts_a_correct_hash_and_refuses_a_tampered_one() {
        let mut drm = DesignReferenceMission { id: "d1".to_string(), ..Default::default() };
        drm.hash = canonical_drm_hash(&drm);
        assert!(verify_drm_hash(&drm).is_ok());

        let mut tampered = drm.clone();
        tampered.name = "a field the DRM's own hash did not cover when it was computed".to_string();
        let err = verify_drm_hash(&tampered).unwrap_err();
        assert!(matches!(err, DrmError::HashMismatch { artifact: "DesignReferenceMission", .. }), "{err:?}");
    }

    #[test]
    fn verify_sos_hash_and_verify_system_hash_refuse_a_tampered_copy_too() {
        let mut sos = SosConfiguration { id: "s1".to_string(), ..Default::default() };
        sos.hash = canonical_sos_hash(&sos);
        assert!(verify_sos_hash(&sos).is_ok());
        sos.name = "tampered".to_string();
        assert!(matches!(verify_sos_hash(&sos), Err(DrmError::HashMismatch { artifact: "SosConfiguration", .. })));

        let mut sys = SystemDefinition { id: "sys1".to_string(), ..Default::default() };
        sys.hash = canonical_system_hash(&sys);
        assert!(verify_system_hash("sys1", &sys).is_ok());
        sys.name = "tampered".to_string();
        assert!(matches!(verify_system_hash("sys1", &sys), Err(DrmError::HashMismatch { artifact: "SystemDefinition", .. })));
    }
}
