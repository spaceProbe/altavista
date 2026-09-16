//! The claim check: an [`av_cdm::pb::AssetRef`] naming a stored object's location, hash,
//! size, media type, label and provenance -- what a hot-track message carries instead of the
//! object's bytes (`docs/heavy-plan.md` H1, question 216).
//!
//! [`verify_payload`] is the mechanical half of that promise: whatever bytes eventually come
//! back from the object store for a given `AssetRef` are checked against the hash and size
//! the claim check itself already named, so a caller that dereferences a claim check can
//! never be silently handed different bytes than the ones the message actually claimed.

use av_cdm::pb::AssetRef;
use bytes::Bytes;
use openssl::memcmp;
use openssl::sha::sha256;

use crate::error::StoreError;
use crate::keys::{hex_decode_64, hex_encode, validate_sha256_hex};

/// An object fetched from the store, paired with the claim check that named it -- what
/// [`crate::client::StoreClient::get`] hands back once [`verify_payload`] has already
/// passed.
pub struct StoredObject {
    pub asset: AssetRef,
    pub bytes: Bytes,
}

/// Builds the [`AssetRef`] for `bytes` as they are about to be (or just were) stored at
/// `key` in `bucket`: hashes `bytes` itself (this crate never trusts a caller-supplied hash
/// for what becomes the claim check's own `sha256` field -- see `crate::client::StoreClient::
/// put`'s module doc for why the hash this function computes is also the object key
/// `crate::keys::object_key` derives, not a coincidence), and fills `uri` as
/// `"s3://<bucket>/<key>"`.
///
/// Does not set `spatial_extent`/`temporal_extent`: this round's callers (the next task's
/// MinIO integration test, and any future H2 catalog caller) do not yet have a payload's
/// geospatial/temporal extent to hand at store time -- `AssetRef.spatial_extent`/
/// `temporal_extent` are additive-only proto fields (this task's binding rule 10), so a
/// future caller that does have an extent sets it on the returned value itself; this
/// function never invents an extent it was not given.
pub fn asset_ref_for(bucket: &str, key: &str, bytes: &[u8], media_type: &str, label: av_cdm::pb::Label, provenance: av_cdm::pb::Provenance) -> AssetRef {
    let digest = sha256(bytes);
    AssetRef {
        uri: format!("s3://{bucket}/{key}"),
        sha256: hex_encode(&digest),
        size_bytes: bytes.len() as u64,
        media_type: media_type.to_string(),
        label: Some(label),
        spatial_extent: None,
        temporal_extent: None,
        provenance: Some(provenance),
        attributes: Default::default(),
    }
}

/// Refuses `bytes` unless both its length and its SHA-256 match what `asset` claims.
/// **Size is checked first**: a truncated or appended-to body is a cheaper, more specific
/// diagnosis (`StoreError::SizeMismatch` names the two lengths directly) than reporting a
/// full hash mismatch for what is really just "wrong length", though a body that is
/// corrupted *without* changing length still falls through to the hash comparison below.
/// The hash comparison itself is constant-time (`openssl::memcmp::eq`), matching this
/// platform's other tamper-detection compares (mirrors, in spirit,
/// `crates/av-command/src/ledger.rs::Ledger::verify`'s own "recomputed hash must match" step,
/// though that one compares two already-computed digests it has no reason to make
/// constant-time; this comparison guards a network-delivered payload against a byte an
/// attacker chose, which is exactly the shape a timing side-channel could matter for, however
/// small that risk is for a content hash rather than a secret).
pub fn verify_payload(asset: &AssetRef, bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.len() as u64 != asset.size_bytes {
        return Err(StoreError::SizeMismatch { expected: asset.size_bytes, actual: bytes.len() as u64 });
    }
    validate_sha256_hex(&asset.sha256).map_err(|_| StoreError::InvalidAssetHash { hash: asset.sha256.clone(), reason: "must be exactly 64 lowercase hex characters" })?;
    let expected = hex_decode_64(&asset.sha256);
    let actual = sha256(bytes);
    if !memcmp::eq(&expected, &actual) {
        return Err(StoreError::HashMismatch { expected: asset.sha256.clone(), actual: hex_encode(&actual), size_bytes: bytes.len() as u64 });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{Label, Provenance};

    fn label() -> Label {
        Label { marking: "CUI".to_string(), caveats: vec![] }
    }

    #[test]
    fn asset_ref_for_fills_uri_hash_and_size() {
        let bytes = b"heavy payload bytes";
        let asset = asset_ref_for("altavista-heavy", "imagery/ab/cd/abcd...", bytes, "image/tiff", label(), Provenance::default());
        assert_eq!(asset.uri, "s3://altavista-heavy/imagery/ab/cd/abcd...");
        assert_eq!(asset.sha256, hex_encode(&sha256(bytes)));
        assert_eq!(asset.size_bytes, bytes.len() as u64);
        assert_eq!(asset.media_type, "image/tiff");
    }

    #[test]
    fn verify_payload_accepts_the_exact_bytes_it_was_built_from() {
        let bytes = b"round trip me exactly";
        let asset = asset_ref_for("b", "k", bytes, "application/octet-stream", label(), Provenance::default());
        verify_payload(&asset, bytes).unwrap();
    }

    #[test]
    fn verify_payload_rejects_a_single_flipped_bit() {
        let bytes = b"exact bytes matter".to_vec();
        let asset = asset_ref_for("b", "k", &bytes, "application/octet-stream", label(), Provenance::default());
        let mut tampered = bytes.clone();
        tampered[0] ^= 0x01;
        let err = verify_payload(&asset, &tampered).unwrap_err();
        assert!(matches!(err, StoreError::HashMismatch { .. }), "{err:?}");
    }

    #[test]
    fn verify_payload_rejects_a_truncated_body() {
        let bytes = b"a body that gets truncated".to_vec();
        let asset = asset_ref_for("b", "k", &bytes, "application/octet-stream", label(), Provenance::default());
        let truncated = &bytes[..bytes.len() - 3];
        let err = verify_payload(&asset, truncated).unwrap_err();
        assert!(matches!(err, StoreError::SizeMismatch { .. }), "{err:?}");
    }

    #[test]
    fn verify_payload_rejects_an_appended_byte() {
        let bytes = b"a body that gets a byte appended".to_vec();
        let asset = asset_ref_for("b", "k", &bytes, "application/octet-stream", label(), Provenance::default());
        let mut appended = bytes.clone();
        appended.push(0x42);
        let err = verify_payload(&asset, &appended).unwrap_err();
        assert!(matches!(err, StoreError::SizeMismatch { .. }), "{err:?}");
    }
}
