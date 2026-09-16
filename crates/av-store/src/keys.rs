//! The content-addressed object key layout: `"<prefix>/<hex[0..2]>/<hex[2..4]>/<hex>"`.
//!
//! Splitting the first two hex-digit pairs into their own path segments (the same "shard by
//! the first few hex characters of the hash" layout S3-compatible object stores and most
//! content-addressed caches use) keeps any one MinIO "directory" -- really a common key
//! prefix, since S3 has no real directories -- from accumulating every object the store ever
//! holds under one flat prefix, which is a known MinIO/S3 listing-performance trap once an
//! object count reaches into the millions. `prefix` itself is deployment-chosen (e.g.
//! `"imagery/2026"`, `crate::client::StoreConfig::key_prefix`) so more than one logical
//! collection can share one bucket without their keys colliding.
//!
//! [`object_key`] never panics and never silently accepts a malformed hash: a hex string
//! that is not exactly 64 lowercase characters, or a prefix with a leading/trailing `/`, a
//! `..` segment, an empty segment, or a non-ASCII byte, is a typed [`StoreError`] refusal.
//! `..` is refused for the ordinary path-traversal reason (a prefix is deployment
//! configuration, not attacker input, but this crate has no way to know that at the type
//! level, and the check costs nothing); uppercase hex is refused rather than lowercased on
//! the caller's behalf, because accepting both cases would mean the same content hash could
//! address two different-looking-but-equal keys, and this crate would rather a caller fix
//! its own hex casing once than silently paper over the inconsistency forever.

use crate::error::StoreError;

/// The content-addressed key `"<prefix>/<hex[0..2]>/<hex[2..4]>/<hex>"`. `prefix` must not
/// have a leading or trailing `/`, must not contain a `..` segment, must not contain an
/// empty segment (so `"a//b"` is refused, not silently collapsed), and must be ASCII.
/// `sha256_hex` must be exactly 64 lowercase hex characters. Either failing is a typed
/// refusal, never a panic and never a key built from an unchecked hash.
pub fn object_key(prefix: &str, sha256_hex: &str) -> Result<String, StoreError> {
    validate_prefix(prefix)?;
    validate_sha256_hex(sha256_hex)?;
    Ok(format!("{prefix}/{}/{}/{sha256_hex}", &sha256_hex[0..2], &sha256_hex[2..4]))
}

fn validate_prefix(prefix: &str) -> Result<(), StoreError> {
    let fail = |reason: &'static str| StoreError::InvalidPrefix { prefix: prefix.to_string(), reason };
    if prefix.is_empty() {
        return Err(fail("must not be empty"));
    }
    if !prefix.is_ascii() {
        return Err(fail("must be ASCII"));
    }
    if prefix.starts_with('/') {
        return Err(fail("must not start with '/'"));
    }
    if prefix.ends_with('/') {
        return Err(fail("must not end with '/'"));
    }
    for segment in prefix.split('/') {
        if segment.is_empty() {
            return Err(fail("must not contain an empty segment (e.g. 'a//b')"));
        }
        if segment == ".." {
            return Err(fail("must not contain a '..' segment"));
        }
    }
    Ok(())
}

/// Shared with [`crate::claim_check::verify_payload`] (which must validate `AssetRef.sha256`
/// itself the same way before comparing it) and [`object_key`] above -- one validation
/// function, so "what counts as a well-formed content hash" is defined exactly once.
pub(crate) fn validate_sha256_hex(hash: &str) -> Result<(), StoreError> {
    let fail = |reason: &'static str| StoreError::InvalidHash { hash: hash.to_string(), reason };
    if hash.len() != 64 {
        return Err(fail("must be exactly 64 characters"));
    }
    if !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(fail("must be lowercase hex ([0-9a-f]) only"));
    }
    Ok(())
}

/// Lowercase hex encoding, shared by every module in this crate that turns a raw SHA-256
/// digest into the string form `AssetRef.sha256` / `x-amz-meta-av-sha256` / the object key
/// itself all carry. Kept here (rather than duplicated, or pulled in as a dependency --
/// `hex`/`data-encoding` are both unnecessary for something this small) because
/// [`object_key`] is the one function in this crate that most obviously has to agree with
/// every caller on exactly what "the hex form of a hash" means.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The inverse of [`hex_encode`], for [`crate::claim_check::verify_payload`]'s constant-time
/// compare (which needs `AssetRef.sha256`'s raw bytes, not its hex string, to hand to
/// `openssl::memcmp::eq`). Callers must validate with [`validate_sha256_hex`] first -- this
/// function panics on malformed input rather than duplicating that check, since every call
/// site in this crate already validates before decoding.
pub(crate) fn hex_decode_64(hash: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = (hash.as_bytes()[i * 2] as char).to_digit(16).expect("validated hex");
        let lo = (hash.as_bytes()[i * 2 + 1] as char).to_digit(16).expect("validated hex");
        *byte = ((hi << 4) | lo) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn object_key_shards_by_the_first_two_hex_byte_pairs() {
        let key = object_key("imagery/2026", HASH).unwrap();
        assert_eq!(key, format!("imagery/2026/e3/b0/{HASH}"));
    }

    #[test]
    fn object_key_refuses_a_hash_that_is_too_short() {
        let err = object_key("p", "abcd").unwrap_err();
        assert!(matches!(err, StoreError::InvalidHash { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_uppercase_hex() {
        let upper = HASH.to_uppercase();
        let err = object_key("p", &upper).unwrap_err();
        assert!(matches!(err, StoreError::InvalidHash { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_a_non_hex_character() {
        let mut bad = HASH.to_string();
        bad.replace_range(0..1, "g");
        let err = object_key("p", &bad).unwrap_err();
        assert!(matches!(err, StoreError::InvalidHash { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_a_leading_slash_prefix() {
        let err = object_key("/p", HASH).unwrap_err();
        assert!(matches!(err, StoreError::InvalidPrefix { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_a_trailing_slash_prefix() {
        let err = object_key("p/", HASH).unwrap_err();
        assert!(matches!(err, StoreError::InvalidPrefix { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_a_dot_dot_segment() {
        let err = object_key("p/../q", HASH).unwrap_err();
        assert!(matches!(err, StoreError::InvalidPrefix { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_an_empty_segment() {
        let err = object_key("p//q", HASH).unwrap_err();
        assert!(matches!(err, StoreError::InvalidPrefix { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_an_empty_prefix() {
        let err = object_key("", HASH).unwrap_err();
        assert!(matches!(err, StoreError::InvalidPrefix { .. }), "{err:?}");
    }

    #[test]
    fn object_key_refuses_a_non_ascii_prefix() {
        let err = object_key("prefix-\u{00e9}", HASH).unwrap_err();
        assert!(matches!(err, StoreError::InvalidPrefix { .. }), "{err:?}");
    }

    #[test]
    fn hex_encode_and_hex_decode_64_round_trip() {
        let bytes = openssl::sha::sha256(b"round trip me");
        let hex = hex_encode(&bytes);
        validate_sha256_hex(&hex).unwrap();
        assert_eq!(hex_decode_64(&hex), bytes);
    }
}
