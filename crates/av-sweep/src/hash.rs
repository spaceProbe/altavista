//! Canonical hashing for `ParameterSweep` (mirrors `av_kernel::drm::hash`'s scheme exactly:
//! clear the message's own `hash` field, encode with `prost::Message`'s canonical binary
//! encoding, SHA-256 the bytes, record the hex digest -- see that module's own doc comment for
//! why this is deterministic across map-field insertion order) plus the F1a-specific
//! [`sample_config_hash`], which is *not* a mirror of anything in `av_kernel::drm::hash`.
//!
//! **`sha256_hex` is reimplemented here, deliberately.** `av_kernel::drm::hash::sha256_hex`
//! exists and does exactly this, but it is `pub(crate)` there (scoped to `av-kernel` itself,
//! for `executor::execute`'s own `RunProducts.port_traffic_hash` use) -- not reachable from this
//! crate. This is a second, independent one-line wrapper around the same `sha2::Sha256`
//! (already a workspace dependency, pure Rust, no bundled C crypto per ADR-004), not a new
//! hashing scheme.
//!
//! **`canonical_drm_hash`/`canonical_sos_hash`/`canonical_system_hash`, by contrast, are NOT
//! reimplemented here.** Those three *are* `pub` on `av_kernel::drm::hash`, so
//! [`crate::sample::sample_config`] calls them directly (`av_kernel::drm::hash::canonical_drm_hash`
//! / `canonical_sos_hash`) rather than duplicating the exact same clear-encode-hash sequence a
//! second time in this crate.
//!
//! ## `sample_config_hash`: where the task brief's own proto comment and reality diverge
//!
//! `proto/altavista/v1/run.proto`'s `SweepSample.config_hash` field comment calls it "the
//! per-sample DRM hash". Taken literally, that is not enough to reproduce a sample: F1a's own
//! axis values are applied to the **SOS** (`SystemInstance.parameter_overrides`, since
//! `SweepAxis.instance` names a `SystemInstance`), not the DRM, and `run.proto` has no separate
//! field for a per-sample SOS hash (this task's hard boundary forbids editing `proto/**`, so
//! that gap cannot be closed by adding one). `config_hash` is therefore defined **here** as the
//! per-sample **configuration** hash: a single value covering every artifact the sample actually
//! ran with (the per-sample DRM, the per-sample SOS, and every `SystemDefinition` referenced),
//! computed directly over the exact bytes F1b will write to `drm.pb`/`sos.pb`/`sys_<id>.pb`, so
//! the hash is independently recomputable from the files on disk once F1b exists. This is
//! broader than the proto comment's literal words but not in conflict with the field's evident
//! purpose ("verify this sample's inputs weren't tampered with"); see this crate's `REPORT.md`
//! for the disclosure this deserves as a place the brief and the proto text disagree.
//!
//! Byte layout (every length a `u32` big-endian prefix, so no two distinct inputs can produce
//! the same byte string):
//! ```text
//! b"altavista.v1.sweep.sample-config/1"
//! || u32be(len(drm_bytes)) || drm_bytes   // per-sample DRM, hash field already populated
//! || u32be(len(sos_bytes)) || sos_bytes   // per-sample SOS, likewise
//! || u32be(number of systems)
//! || for each (id, system) in BTreeMap (i.e. sorted-by-id) order:
//!      u32be(len(id)) || id_bytes || u32be(len(system_bytes)) || system_bytes
//! ```

use std::collections::BTreeMap;

use av_cdm::pb;
use sha2::{Digest, Sha256};

use crate::error::SweepError;

/// SHA-256 hex digest of arbitrary bytes. See the module doc comment for why this crate has its
/// own copy rather than reusing `av_kernel::drm::hash::sha256_hex` (`pub(crate)` there).
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The canonical hash of `sweep`, as if its own `hash` field were empty.
pub fn canonical_sweep_hash(sweep: &pb::ParameterSweep) -> String {
    let mut cleared = sweep.clone();
    cleared.hash.clear();
    sha256_hex(&prost::Message::encode_to_vec(&cleared))
}

/// Verify `sweep.hash` against its own canonical hash, refusing (never merely warning) on a
/// mismatch. Returns the computed hash on success, mirroring `av_kernel::drm::hash::verify_drm_hash`.
pub fn verify_sweep_hash(sweep: &pb::ParameterSweep) -> Result<String, SweepError> {
    let computed = canonical_sweep_hash(sweep);
    if sweep.hash != computed {
        return Err(SweepError::HashMismatch { id: sweep.id.clone(), declared: sweep.hash.clone(), computed });
    }
    Ok(computed)
}

/// The per-sample configuration hash -- see the module doc comment's "`sample_config_hash`"
/// section for the exact byte layout and why it is broader than `run.proto`'s own
/// `SweepSample.config_hash` comment. `drm`/`sos` must already carry their own populated `hash`
/// field (the per-sample, post-mutation hash, set by [`crate::sample::sample_config`] via
/// `av_kernel::drm::hash::canonical_drm_hash`/`canonical_sos_hash` before this is called) --
/// this function hashes them exactly as given, it does not clear or recompute either field
/// itself. `systems` is iterated in `BTreeMap` (sorted-by-id) order, so two callers holding the
/// same systems built or inserted in different orders compute the identical hash.
pub fn sample_config_hash(drm: &pb::DesignReferenceMission, sos: &pb::SosConfiguration, systems: &BTreeMap<String, pb::SystemDefinition>) -> String {
    let drm_bytes = prost::Message::encode_to_vec(drm);
    let sos_bytes = prost::Message::encode_to_vec(sos);
    let mut buf = Vec::new();
    buf.extend_from_slice(b"altavista.v1.sweep.sample-config/1");
    buf.extend_from_slice(&(drm_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(&drm_bytes);
    buf.extend_from_slice(&(sos_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(&sos_bytes);
    buf.extend_from_slice(&(systems.len() as u32).to_be_bytes());
    for (id, sys) in systems {
        let sys_bytes = prost::Message::encode_to_vec(sys);
        buf.extend_from_slice(&(id.len() as u32).to_be_bytes());
        buf.extend_from_slice(id.as_bytes());
        buf.extend_from_slice(&(sys_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(&sys_bytes);
    }
    sha256_hex(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep(id: &str) -> pb::ParameterSweep {
        pb::ParameterSweep { id: id.to_string(), ..Default::default() }
    }

    #[test]
    fn sweep_hash_is_stable_and_excludes_the_hash_field_itself() {
        let mut s = sweep("sweep1");
        let h1 = canonical_sweep_hash(&s);
        assert_eq!(h1.len(), 64, "hex-encoded SHA-256 is 64 chars");
        s.hash = h1.clone();
        assert_eq!(canonical_sweep_hash(&s), h1, "setting hash to its own correct value must not change the computed hash");
        s.hash = "not-the-real-hash".to_string();
        assert_eq!(canonical_sweep_hash(&s), h1, "the field's own prior content must never affect the computed hash");
    }

    #[test]
    fn verify_sweep_hash_refuses_a_tampered_sweep() {
        let mut s = sweep("sweep1");
        s.hash = canonical_sweep_hash(&s);
        assert!(verify_sweep_hash(&s).is_ok());

        let mut tampered = s.clone();
        tampered.drm_id = "a field the sweep's own hash did not cover when it was computed".to_string();
        let err = verify_sweep_hash(&tampered).unwrap_err();
        assert!(matches!(err, SweepError::HashMismatch { .. }), "{err:?}");
    }

    #[test]
    fn sample_config_hash_changes_with_the_drm_the_sos_a_system_and_a_system_id() {
        let drm = pb::DesignReferenceMission { id: "d1".to_string(), ..Default::default() };
        let sos = pb::SosConfiguration { id: "s1".to_string(), ..Default::default() };
        let mut systems = BTreeMap::new();
        systems.insert("sys1".to_string(), pb::SystemDefinition { id: "sys1".to_string(), ..Default::default() });
        let base = sample_config_hash(&drm, &sos, &systems);

        let mut drm2 = drm.clone();
        drm2.name = "changed".to_string();
        assert_ne!(sample_config_hash(&drm2, &sos, &systems), base, "the DRM's own content must affect the hash");

        let mut sos2 = sos.clone();
        sos2.name = "changed".to_string();
        assert_ne!(sample_config_hash(&drm, &sos2, &systems), base, "the SOS's own content must affect the hash");

        let mut systems_content_changed = systems.clone();
        systems_content_changed.get_mut("sys1").unwrap().name = "changed".to_string();
        assert_ne!(sample_config_hash(&drm, &sos, &systems_content_changed), base, "a system's own content must affect the hash");

        let mut systems_id_changed = BTreeMap::new();
        systems_id_changed.insert("sys2".to_string(), pb::SystemDefinition { id: "sys1".to_string(), ..Default::default() });
        assert_ne!(sample_config_hash(&drm, &sos, &systems_id_changed), base, "a system's own map key (id) must affect the hash, independent of its content");

        assert_eq!(sample_config_hash(&drm, &sos, &systems), base, "otherwise stable");
    }
}
