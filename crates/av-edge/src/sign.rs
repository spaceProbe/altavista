//! Loading a P-384 EC private key from a PEM file and signing a `MeasurementBatch` with
//! it (step 4 of the canonical hash/signature definition, `crate` module doc).
//!
//! Uses `openssl::ecdsa::EcdsaSig::sign` directly on the 32 raw `batch_hash` bytes,
//! **not** `openssl::sign::Signer` -- `Signer` is built to hash a message with a chosen
//! digest algorithm and then sign that digest; `batch_hash` is already the exact digest
//! that must be signed, so routing it through `Signer` would silently sign
//! `SHA-whatever(batch_hash)` instead of `batch_hash` itself, which is a different value
//! `crate::verify` would never accept.

use openssl::ec::{EcKey, EcKeyRef};
use openssl::ecdsa::EcdsaSig;
use openssl::nid::Nid;
use openssl::pkey::Private;

use crate::hash;
use crate::pb;

/// What can go wrong loading a signing key or producing a signature. Every variant names
/// a refusal, never a silent fallback -- in particular [`SigningError::WrongCurve`] is
/// the manager's explicit requirement that a non-P-384 key is refused outright, not
/// merely warned about.
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    /// The PEM did not parse as an EC private key at all (wrong format, corrupt file, or
    /// not EC).
    #[error("could not parse an EC private key from the given PEM: {0}")]
    InvalidPem(String),
    /// The key parsed, but its curve is not P-384 (secp384r1, `Nid::SECP384R1`). Signing
    /// with any other curve would produce a signature `crate::verify` (and every other
    /// consumer on this track) is not built to check, and ADR-004 names ECDSA P-384
    /// specifically -- so this is refused rather than silently accepted.
    #[error("signing key is on curve {actual:?}, not P-384 (secp384r1) -- refusing to sign with the wrong curve")]
    WrongCurve { actual: Option<Nid> },
    /// The underlying OpenSSL ECDSA signing operation itself failed (an `ErrorStack`,
    /// stringified since `openssl::error::ErrorStack` is not `Clone`/`PartialEq`, and
    /// callers of this crate only ever need to report the failure, not match on it).
    #[error("OpenSSL ECDSA signing operation failed: {0}")]
    Openssl(String),
}

/// Parses `pem` as an EC private key (`openssl ecparam -genkey`'s own output format,
/// "-----BEGIN EC PRIVATE KEY-----") and refuses it unless its curve is P-384.
pub fn load_signing_key(pem: &[u8]) -> Result<EcKey<Private>, SigningError> {
    let key = EcKey::private_key_from_pem(pem).map_err(|e| SigningError::InvalidPem(e.to_string()))?;
    require_p384(key.group().curve_name())?;
    Ok(key)
}

fn require_p384(curve: Option<Nid>) -> Result<(), SigningError> {
    if curve != Some(Nid::SECP384R1) {
        return Err(SigningError::WrongCurve { actual: curve });
    }
    Ok(())
}

/// ECDSA P-384 over `digest`'s 32 bytes, DER encoded -- step 4 of the canonical
/// signature definition, as a primitive over a bare digest rather than a whole batch (so
/// `crate::chain`'s tests can also sign an already-computed hash directly without
/// round-tripping through a full `MeasurementBatch`).
pub fn sign_digest(key: &EcKeyRef<Private>, digest: &[u8; 32]) -> Result<Vec<u8>, SigningError> {
    let sig = EcdsaSig::sign(digest, key).map_err(|e| SigningError::Openssl(e.to_string()))?;
    sig.to_der().map_err(|e| SigningError::Openssl(e.to_string()))
}

/// Fills in `batch.prev_hash`, `batch.batch_hash` and `batch.signature` per the canonical
/// definition: `prev_hash` becomes exactly the bytes the caller supplies (`crate::hash::
/// GENESIS` for a producer's first batch, or a previous batch's `batch_hash` otherwise --
/// deciding which is the caller's responsibility, not this function's), `batch_hash` is
/// computed over the batch's own canonical body with that `prev_hash`, and `signature` is
/// [`sign_digest`] over `batch_hash`. Every other field of `batch` (label, measurements,
/// provenance, `producer_id`, `sequence`, `batch_tai_ns`) must already be set by the
/// caller before this is called -- this function only ever adds the three chain/signature
/// fields, never touches the payload.
pub fn sign_batch(batch: &mut pb::MeasurementBatch, prev_hash: &[u8], key: &EcKeyRef<Private>) -> Result<(), SigningError> {
    batch.prev_hash = prev_hash.to_vec();
    batch.batch_hash.clear();
    batch.signature.clear();
    let body = hash::canonical_body_bytes(batch);
    let digest = hash::compute_batch_hash(prev_hash, &body);
    let signature = sign_digest(key, &digest)?;
    batch.batch_hash = digest.to_vec();
    batch.signature = signature;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY_PEM: &[u8] = include_bytes!("../tests/fixtures/test_signing_key.pem");

    #[test]
    fn load_signing_key_accepts_the_committed_p384_test_key() {
        load_signing_key(TEST_KEY_PEM).expect("the committed test fixture is a P-384 key");
    }

    #[test]
    fn load_signing_key_rejects_garbage_pem() {
        let err = load_signing_key(b"not a pem at all").unwrap_err();
        assert!(matches!(err, SigningError::InvalidPem(_)), "{err:?}");
    }

    #[test]
    fn load_signing_key_rejects_a_non_p384_curve() {
        // secp256r1 (P-256): a real EC key, just the wrong curve -- proves the curve
        // check runs (and refuses) rather than accepting any EC key.
        let p256 = EcKey::generate(&openssl::ec::EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap();
        let pem = p256.private_key_to_pem().unwrap();
        let err = load_signing_key(&pem).unwrap_err();
        assert!(matches!(err, SigningError::WrongCurve { actual: Some(Nid::X9_62_PRIME256V1) }), "{err:?}");
    }

    #[test]
    fn sign_batch_fills_in_prev_hash_batch_hash_and_signature() {
        let key = load_signing_key(TEST_KEY_PEM).unwrap();
        let mut batch = pb::MeasurementBatch { producer_id: "p1".to_string(), sequence: 1, ..Default::default() };
        sign_batch(&mut batch, hash::GENESIS, &key).unwrap();
        assert_eq!(batch.prev_hash, hash::GENESIS);
        assert_eq!(batch.batch_hash.len(), 32);
        assert!(!batch.signature.is_empty());
    }
}
