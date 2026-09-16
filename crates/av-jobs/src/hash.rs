//! Canonical hashing for this crate's job-queue records -- the same convention
//! `av_edge::hash`/`crates/av-dynamics-service/src/evidence.rs` already use for their own
//! hash chains (a literal `GENESIS` sentinel, `SHA-256(prev || payload)`), **reimplemented
//! here rather than depended on**: `crates/av-jobs` may not depend on `av-edge` (off this
//! heavy track entirely -- `crates/av-store/src/labels.rs`'s former module doc recorded
//! exactly this reasoning for the clearance-ladder convention, before question 218 extracted
//! that convention into `av-label`; this crate applies the identical dependency-direction
//! boundary to the hash-chain convention, which has no shared home to extract into the way
//! the ladder did -- `av_edge::hash::compute_batch_hash` is `pub(crate)`-adjacent API of a
//! crate this track does not touch, not a workspace-shared utility). Two lines of logic
//! duplicated, deliberately, across independently-owned crates is the accepted cost of that
//! boundary -- see `crate::log`'s own module doc for how this primitive is used.

use openssl::sha::sha256;

/// The suite's ledger convention (`av_edge::hash::GENESIS`, `crates/av-dynamics-service/src/
/// evidence.rs::GENESIS`, verbatim): a chain's first record hashes from this literal
/// string's ASCII bytes, never from a hash of anything.
pub const GENESIS: &[u8] = b"GENESIS";

/// `SHA-256(prev || payload)`, 32 raw bytes -- identical in shape to
/// `av_edge::hash::compute_batch_hash` and `crates/av-dynamics-service/src/evidence.rs`'s own
/// chain step, applied here to a `JobLogRecord` payload instead of a `MeasurementBatch`/
/// evidence-entry body. `prev` is [`GENESIS`] for a log's first record, or else the previous
/// record's own raw 32-byte hash -- this function does not itself decide which; that is
/// `crate::log::JobLog`'s job.
pub fn chain_hash(prev: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(prev.len() + payload.len());
    buf.extend_from_slice(prev);
    buf.extend_from_slice(payload);
    sha256(&buf)
}

/// Lowercase hex encoding, two characters per byte -- the display/`spec_sha256`/
/// `input_sha256` string form. The on-disk `record_hash` itself is always raw bytes (see
/// `crate::log`'s own module doc, "Record framing").
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The inverse of [`hex_encode`]: `None` for anything that is not an even-length string of
/// ASCII hex digits -- `crate::runner::Runner::execute_spec`'s own input-hash-verification
/// step uses this to decode `AssetRef.sha256` before the constant-time `openssl::memcmp::eq`
/// compare, mirroring `av_store::keys::hex_decode_64`'s "validate, then decode" discipline
/// (here folded into one call: an odd length, a non-hex character, or a decoded length other
/// than 32 bytes all fail the caller's own `matches!` check rather than panicking).
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_hash_is_32_bytes_and_deterministic() {
        let h1 = chain_hash(GENESIS, b"payload");
        let h2 = chain_hash(GENESIS, b"payload");
        assert_eq!(h1.len(), 32);
        assert_eq!(h1, h2);
    }

    #[test]
    fn chain_hash_differs_when_prev_or_payload_differs() {
        let base = chain_hash(GENESIS, b"payload");
        assert_ne!(base, chain_hash(b"other-prev", b"payload"), "prev must affect the hash");
        assert_ne!(base, chain_hash(GENESIS, b"other-payload"), "payload must affect the hash");
    }

    #[test]
    fn hex_encode_is_lowercase_and_two_chars_per_byte() {
        let hex = hex_encode(&[0xAB, 0x01, 0xff]);
        assert_eq!(hex, "ab01ff");
    }

    #[test]
    fn hex_encode_and_hex_decode_round_trip() {
        let bytes = openssl::sha::sha256(b"round trip me");
        let hex = hex_encode(&bytes);
        assert_eq!(hex_decode(&hex).unwrap(), bytes.to_vec());
    }

    #[test]
    fn hex_decode_rejects_malformed_input() {
        assert_eq!(hex_decode("abc"), None, "odd length");
        assert_eq!(hex_decode("zz"), None, "non-hex digits");
        assert_eq!(hex_decode(""), Some(vec![]), "empty string decodes to empty bytes");
    }

    /// A known-answer pin, independently re-derived with the `openssl(1)` CLI at review
    /// time (the same discipline `crates/av-store/src/sigv4.rs`'s own known-answer test
    /// documents), NOT by calling [`chain_hash`] itself.
    #[test]
    fn chain_hash_matches_an_independently_computed_openssl_digest() {
        // `printf 'GENESISabc' | openssl dgst -sha256` (this host's OpenSSL CLI) prints:
        // 9aa61f4d404613738aae503ec8c32eabbba5824c968aa39056e3ab453b242986
        let expected = "9aa61f4d404613738aae503ec8c32eabbba5824c968aa39056e3ab453b242986";
        let got = chain_hash(GENESIS, b"abc");
        assert_eq!(hex_encode(&got), expected);
    }
}
