//! Canonical hashing for `altavista.v1.MeasurementBatch` -- the first three lines of the
//! four-line definition pinned in this crate's module doc (`crate::sign`/`crate::verify`
//! own the fourth, the signature itself).
//!
//! Deliberately mirrors `crates/av-kernel/src/drm/hash.rs` (`prost::Message::encode_to_vec`
//! of a message with its own hash-bearing fields cleared) and
//! `crates/av-dynamics-service/src/evidence.rs` (`openssl::sha::sha256`, the `GENESIS`
//! sentinel, `hex_encode`/`hex_decode`, `SHA-256(prev_hash_bytes || body_bytes)`) rather
//! than inventing a third convention -- see those modules' own doc comments for the
//! reasoning this crate borrows without repeating in full.

use openssl::sha::sha256;

use crate::pb;

/// The suite's ledger convention (`envelope.proto`'s `SignedBatch.prev_hash` doc comment,
/// `av-dynamics-service/src/evidence.rs::GENESIS`, verbatim): a producer's first batch
/// chains from this literal string, not from a hash of anything -- its ASCII bytes are
/// exactly what gets hashed alongside that first batch's own body.
pub const GENESIS: &[u8] = b"GENESIS";

/// Hex-encode `bytes`, lowercase, two characters per byte. A pure display/log
/// convenience -- the wire type (`MeasurementBatch.prev_hash`/`batch_hash`) is always raw
/// bytes, never this string form.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The inverse of [`hex_encode`]. `None` for anything that is not an even-length string
/// of ASCII hex digits (including the literal string `"GENESIS"`, which is a distinct,
/// never-hex-decoded sentinel -- callers that need to distinguish "no prior batch" from
/// "a real hash" should compare against [`GENESIS`]/its hex form directly, not by
/// round-tripping through this function).
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Step 1 of the canonical hash definition: `batch`'s own deterministic protobuf
/// encoding, as if its `batch_hash` and `signature` fields were empty. Every other field
/// -- including `prev_hash`, which is *not* cleared -- is encoded exactly as `batch`
/// carries it; see this crate's module doc for why `prev_hash` is covered by the body
/// even though it is also used as the hash's prefix.
pub fn canonical_body_bytes(batch: &pb::MeasurementBatch) -> Vec<u8> {
    let mut cleared = batch.clone();
    cleared.batch_hash.clear();
    cleared.signature.clear();
    prost::Message::encode_to_vec(&cleared)
}

/// Steps 2-3 of the canonical hash definition: `SHA-256(prev_hash_bytes || body_bytes)`,
/// 32 raw bytes. `prev_hash_bytes` is whatever the caller supplies -- [`GENESIS`] for a
/// producer's first batch, or a previous batch's own `batch_hash`; this function does
/// not itself decide which, since that decision belongs to chain state
/// ([`crate::chain::ChainVerifier`]), not to hashing.
pub fn compute_batch_hash(prev_hash_bytes: &[u8], body_bytes: &[u8]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(prev_hash_bytes.len() + body_bytes.len());
    buf.extend_from_slice(prev_hash_bytes);
    buf.extend_from_slice(body_bytes);
    sha256(&buf)
}

/// What `batch.batch_hash` *should* equal, given `batch`'s own currently-set
/// `prev_hash` field -- a **self-consistency** check, not a chain-state check. This
/// recomputes the hash from the batch's own declared `prev_hash`, so it answers "is this
/// specific `(prev_hash, body, batch_hash)` triple internally coherent, i.e. could
/// `batch_hash` genuinely have been produced from this exact `prev_hash` and this exact
/// body". Whether the *declared* `prev_hash` is the one this producer's chain actually
/// expects next is a separate question ([`crate::chain`]'s CHAIN_BREAK/CHAIN_GAP checks),
/// deliberately kept out of this function so signature/hash self-consistency
/// (`BAD_SIGNATURE`) and chain linkage (`CHAIN_BREAK`) stay two independently testable
/// checks instead of one that conflates "forged" with "does not chain from what we saw
/// last".
pub fn recompute_batch_hash(batch: &pb::MeasurementBatch) -> [u8; 32] {
    let body = canonical_body_bytes(batch);
    compute_batch_hash(&batch.prev_hash, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_batch() -> pb::MeasurementBatch {
        pb::MeasurementBatch { producer_id: "p1".to_string(), sequence: 1, ..Default::default() }
    }

    #[test]
    fn hex_round_trips() {
        let bytes = sha256(b"hello world");
        let hex = hex_encode(&bytes);
        assert_eq!(hex.len(), 64);
        assert_eq!(hex_decode(&hex).unwrap(), bytes.to_vec());
    }

    #[test]
    fn hex_decode_rejects_malformed_input() {
        assert_eq!(hex_decode("abc"), None, "odd length");
        assert_eq!(hex_decode("zz"), None, "non-hex digits");
        assert_eq!(hex_decode(""), Some(vec![]), "empty string decodes to empty bytes");
    }

    /// The property `crates/av-kernel/src/drm/hash.rs::hash_is_stable_and_excludes_the_hash_field_itself`
    /// asserts for `DesignReferenceMission`, here for `MeasurementBatch`: pre-filling
    /// `batch_hash`/`signature` with garbage before hashing must not change the computed
    /// canonical body bytes or hash, because both fields are cleared before encoding.
    #[test]
    fn canonical_body_bytes_excludes_batch_hash_and_signature() {
        let mut batch = sample_batch();
        let body_before = canonical_body_bytes(&batch);
        batch.batch_hash = vec![0xAB; 32];
        batch.signature = vec![0xCD; 64];
        let body_after = canonical_body_bytes(&batch);
        assert_eq!(body_before, body_after, "batch_hash/signature must not affect the canonical body encoding");
    }

    #[test]
    fn compute_batch_hash_is_32_bytes_and_deterministic() {
        let h1 = compute_batch_hash(GENESIS, b"body");
        let h2 = compute_batch_hash(GENESIS, b"body");
        assert_eq!(h1.len(), 32);
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_batch_hash_differs_when_prev_hash_or_body_differs() {
        let base = compute_batch_hash(GENESIS, b"body");
        assert_ne!(base, compute_batch_hash(b"other-prev", b"body"), "prev_hash must affect the hash");
        assert_ne!(base, compute_batch_hash(GENESIS, b"other-body"), "body must affect the hash");
    }

    #[test]
    fn recompute_batch_hash_uses_the_batchs_own_prev_hash_field() {
        let mut batch = sample_batch();
        batch.prev_hash = GENESIS.to_vec();
        let h_genesis = recompute_batch_hash(&batch);
        batch.prev_hash = b"some-other-previous-hash-bytes".to_vec();
        let h_other = recompute_batch_hash(&batch);
        assert_ne!(h_genesis, h_other, "changing prev_hash must change the recomputed hash");

        // And it must match compute_batch_hash over the same body bytes directly.
        let body = canonical_body_bytes(&batch);
        assert_eq!(h_other, compute_batch_hash(&batch.prev_hash, &body));
    }
}
