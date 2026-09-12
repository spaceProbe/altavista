//! Loading a P-384 EC public key from a PEM (a plain public key, or a certificate) and
//! verifying one `MeasurementBatch`'s `batch_hash`/`signature` against it.
//!
//! Like `crate::sign`, uses `openssl::ecdsa::EcdsaSig::verify` directly on the 32 raw
//! `batch_hash` bytes rather than `openssl::sign::Verifier` -- see `crate::sign`'s module
//! doc for why re-hashing an already-hashed digest would silently check the wrong bytes.

use openssl::ec::{EcKey, EcKeyRef};
use openssl::ecdsa::EcdsaSig;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Public};
use openssl::x509::X509;

use crate::hash;
use crate::pb;

/// What can go wrong loading a verifying key or checking a batch's signature. Every
/// variant is a refusal a caller must count, never silently swallow (ADR-004:
/// "everything rejected is counted").
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// `pem` did not parse as either an X.509 certificate or a plain EC public key.
    #[error("could not parse a public key from the given PEM (tried as a certificate and as a bare EC public key): {0}")]
    InvalidPem(String),
    /// The key parsed, but its curve is not P-384 -- refused for the same reason
    /// `crate::sign::SigningError::WrongCurve` refuses a signing key.
    #[error("verifying key is on curve {actual:?}, not P-384 (secp384r1)")]
    WrongCurve { actual: Option<Nid> },
    /// `MeasurementBatch.signature` was empty. Kept as its own variant (rather than
    /// folded into `SignatureInvalid`) because the ingest boundary counts this as its own
    /// rejection kind, `BATCH_REJECTION_UNSIGNED`, distinct from a signature that is
    /// present but wrong.
    #[error("batch is unsigned (signature field is empty)")]
    Unsigned,
    /// `batch.batch_hash` does not equal the hash recomputed from the batch's own
    /// content and its own declared `prev_hash` -- the batch's content and its claimed
    /// hash disagree, so the signature (which is only ever checked against the
    /// recomputed hash, never against the batch's possibly-tampered `batch_hash` field
    /// directly) cannot be trusted either.
    #[error("batch_hash does not match the hash recomputed from the batch's own content")]
    HashMismatch,
    /// `batch.signature` was not a well-formed DER ECDSA signature at all.
    #[error("signature is not a well-formed DER ECDSA signature: {0}")]
    MalformedSignature(String),
    /// The signature parsed and the hash matched, but the ECDSA verification itself
    /// returned false: this key did not produce this signature over this digest.
    #[error("signature does not verify against the given public key")]
    SignatureInvalid,
    /// The underlying OpenSSL operation failed outright (not a verification failure --
    /// an actual `ErrorStack`, e.g. a malformed key).
    #[error("OpenSSL ECDSA verification operation failed: {0}")]
    Openssl(String),
}

fn require_p384(curve: Option<Nid>) -> Result<(), VerifyError> {
    if curve != Some(Nid::SECP384R1) {
        return Err(VerifyError::WrongCurve { actual: curve });
    }
    Ok(())
}

fn ec_key_from_pkey(pkey: PKey<Public>) -> Result<EcKey<Public>, VerifyError> {
    let ec = pkey.ec_key().map_err(|e| VerifyError::InvalidPem(format!("public key is not an EC key: {e}")))?;
    require_p384(ec.group().curve_name())?;
    Ok(ec)
}

/// Parses `pem` as a public key: tries an X.509 certificate first (`openssl req
/// -x509 ...`'s output, "-----BEGIN CERTIFICATE-----" -- what a seccert-issued leaf will
/// be, from E2 onward), then falls back to a bare `SubjectPublicKeyInfo` public key PEM
/// (`openssl ec -pubout`'s output, "-----BEGIN PUBLIC KEY-----" -- what E1's tests use,
/// with no certificate in the loop yet). Refuses anything not on the P-384 curve.
pub fn load_verifying_key(pem: &[u8]) -> Result<EcKey<Public>, VerifyError> {
    if let Ok(cert) = X509::from_pem(pem) {
        let pkey = cert.public_key().map_err(|e| VerifyError::InvalidPem(format!("certificate has no usable public key: {e}")))?;
        return ec_key_from_pkey(pkey);
    }
    let pkey = PKey::public_key_from_pem(pem).map_err(|e| VerifyError::InvalidPem(e.to_string()))?;
    ec_key_from_pkey(pkey)
}

/// Verifies a bare 32-byte digest against a raw DER ECDSA signature -- the primitive
/// underneath [`verify_batch`], exposed so `crate::chain`'s tests can check a signature
/// produced by [`crate::sign::sign_digest`] without round-tripping through a whole
/// `MeasurementBatch`.
pub fn verify_digest(key: &EcKeyRef<Public>, digest: &[u8; 32], signature: &[u8]) -> Result<(), VerifyError> {
    if signature.is_empty() {
        return Err(VerifyError::Unsigned);
    }
    let sig = EcdsaSig::from_der(signature).map_err(|e| VerifyError::MalformedSignature(e.to_string()))?;
    let ok = sig.verify(digest, key).map_err(|e| VerifyError::Openssl(e.to_string()))?;
    if !ok {
        return Err(VerifyError::SignatureInvalid);
    }
    Ok(())
}

/// Full verification of one `MeasurementBatch`: refuses an empty signature
/// ([`VerifyError::Unsigned`]), refuses a `batch_hash` that does not match what the
/// batch's own content (and its own declared `prev_hash`) recomputes to
/// ([`VerifyError::HashMismatch`]), then checks the signature against the recomputed
/// hash (never against the batch's own possibly-tampered `batch_hash` field). Returns
/// the recomputed 32-byte hash on success, so a caller (`crate::chain`) that already
/// needs it for chain-linking does not have to recompute it a second time.
pub fn verify_batch(batch: &pb::MeasurementBatch, key: &EcKeyRef<Public>) -> Result<[u8; 32], VerifyError> {
    if batch.signature.is_empty() {
        return Err(VerifyError::Unsigned);
    }
    let recomputed = hash::recompute_batch_hash(batch);
    if batch.batch_hash.as_slice() != recomputed.as_slice() {
        return Err(VerifyError::HashMismatch);
    }
    verify_digest(key, &recomputed, &batch.signature)?;
    Ok(recomputed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign;

    const TEST_KEY_PEM: &[u8] = include_bytes!("../tests/fixtures/test_signing_key.pem");
    const TEST_PUB_PEM: &[u8] = include_bytes!("../tests/fixtures/test_signing_key.pub.pem");

    #[test]
    fn load_verifying_key_accepts_the_committed_public_key_pem() {
        load_verifying_key(TEST_PUB_PEM).expect("the committed test fixture is a P-384 public key");
    }

    #[test]
    fn load_verifying_key_rejects_garbage_pem() {
        let err = load_verifying_key(b"not a pem at all").unwrap_err();
        assert!(matches!(err, VerifyError::InvalidPem(_)), "{err:?}");
    }

    #[test]
    fn verify_batch_accepts_a_batch_signed_with_the_matching_key() {
        let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
        let verifying_key = load_verifying_key(TEST_PUB_PEM).unwrap();
        let mut batch = pb::MeasurementBatch { producer_id: "p1".to_string(), sequence: 1, ..Default::default() };
        sign::sign_batch(&mut batch, hash::GENESIS, &signing_key).unwrap();
        verify_batch(&batch, &verifying_key).expect("a freshly signed batch must verify");
    }

    #[test]
    fn verify_batch_rejects_an_unsigned_batch() {
        let verifying_key = load_verifying_key(TEST_PUB_PEM).unwrap();
        let batch = pb::MeasurementBatch { producer_id: "p1".to_string(), sequence: 1, prev_hash: hash::GENESIS.to_vec(), ..Default::default() };
        assert!(matches!(verify_batch(&batch, &verifying_key), Err(VerifyError::Unsigned)));
    }
}
