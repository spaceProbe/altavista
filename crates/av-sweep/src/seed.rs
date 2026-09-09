//! Per-sample seed derivation (F1a, `docs/feasibility-plan.md`'s F1 milestone: "derive each
//! sample's seed as SHA-256 over (base seed, sweep hash, point, draw) truncated to 64 bits").
//!
//! ## Why `key` joins the plan's stated tuple
//!
//! The plan text names the tuple `(base seed, sweep hash, point, draw)`. This module adds a
//! fifth input, `key`, and that is a deliberate refinement, not a drift from the plan: a DRM's
//! `Scenario.seeds` (`proto/altavista/v1/system.proto`) is a **map** -- a DRM can, and the demo
//! fixture does not but a real one might, declare more than one seed key (one per fault id /
//! maneuver execution-error seed / future stochastic element). Two different keys that happen
//! to share the same *base* `u64` value would, under the plan's literal four-input tuple,
//! derive the byte-identical substream for the whole study -- every draw of every point would
//! see those two "different" seeds behave as one. Folding `key` in as a fifth input makes every
//! `(point, draw, key)` triple's derived seed depend on that key's own identity, not merely on
//! the numeric value it happened to be declared with.
//!
//! ## Byte layout
//!
//! SHA-256 over exactly this byte string (every fixed-width field is always exactly that width,
//! so the position of every field up to and including `draw_index` never shifts regardless of
//! `key`'s own length or content):
//!
//! ```text
//! b"altavista.v1.sweep.seed/1"          (25 bytes, no separator)
//! || base_seed as u64 big-endian        (8 bytes)
//! || sweep_hash as lowercase hex ASCII  (64 bytes; refused otherwise, see SweepError::InvalidSweepHash)
//! || point_index as u32 big-endian      (4 bytes)
//! || draw_index  as u32 big-endian      (4 bytes)
//! || key.len() as u32 big-endian        (4 bytes)
//! || key UTF-8 bytes
//! ```
//!
//! The derived seed is the first 8 bytes of the SHA-256 digest, read as a big-endian `u64`
//! ("truncated to 64 bits", the plan's own words).
//!
//! `sweep_hash` must be exactly 64 lowercase hex characters (a real SHA-256 hex digest, e.g.
//! from [`crate::hash::canonical_sweep_hash`]) -- refused otherwise
//! ([`SweepError::InvalidSweepHash`]), since the byte layout above fixes that field's width at
//! exactly 64 bytes: an off-length or mixed-case value would silently shift or corrupt every
//! byte after it.
//!
//! [`tests::derived_seed_matches_an_independently_computed_sha256`] pins this against a genuine
//! external oracle: a script (kept at
//! `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/f1a-seed-oracle.py`,
//! named again in `crates/av-sweep/REPORT.md`) writes the exact input bytes for each test vector
//! to a file, and the system `shasum -a 256` binary (not this crate's own `sha2` dependency) is
//! run over that file to produce the expected digest -- see that test's own comment for the
//! exact commands and the resulting values.

use sha2::{Digest, Sha256};

use crate::error::SweepError;

/// The domain-separation prefix (25 bytes, no separator between its own dotted segments) --
/// see the module doc comment's byte layout.
const DOMAIN: &[u8] = b"altavista.v1.sweep.seed/1";
const _: () = assert!(DOMAIN.len() == 25, "byte layout doc comment promises exactly 25 bytes");

fn is_lowercase_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Derive one sample's seed. See the module doc comment for the exact byte layout and for why
/// `key` is a fifth input beyond the plan's stated `(base seed, sweep hash, point, draw)` tuple.
pub fn derive_seed(base_seed: u64, sweep_hash: &str, point_index: u32, draw_index: u32, key: &str) -> Result<u64, SweepError> {
    if !is_lowercase_hex(sweep_hash) {
        return Err(SweepError::InvalidSweepHash { sweep_hash: sweep_hash.to_string() });
    }
    let mut buf = Vec::with_capacity(DOMAIN.len() + 8 + 64 + 4 + 4 + 4 + key.len());
    buf.extend_from_slice(DOMAIN);
    buf.extend_from_slice(&base_seed.to_be_bytes());
    buf.extend_from_slice(sweep_hash.as_bytes());
    buf.extend_from_slice(&point_index.to_be_bytes());
    buf.extend_from_slice(&draw_index.to_be_bytes());
    buf.extend_from_slice(&(key.len() as u32).to_be_bytes());
    buf.extend_from_slice(key.as_bytes());

    let digest = Sha256::digest(&buf);
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&digest[0..8]);
    Ok(u64::from_be_bytes(eight))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Pinned against an independent oracle -- see the module doc comment's last paragraph for
    /// how these two vectors were produced (a script writing the exact input bytes to a file,
    /// hashed by the system `shasum -a 256` binary, not this crate's own code).
    ///
    /// Vector 1: base_seed=42, sweep_hash=HASH_A, point_index=0, draw_index=0, key="fault1".
    ///   Built and hashed with (`f1a-seed-oracle.py` writes `vector1.bin` with exactly this
    ///   module's byte layout, `shasum` is the system binary, not this crate's own code):
    ///     python3 f1a-seed-oracle.py .
    ///     shasum -a 256 vector1.bin
    ///     -> 956b864b6eef565e9b5189473adb7e1ec768c2e29a2ceb3382efa6709eae8a94
    ///   First 8 bytes (16 hex chars), big-endian: 0x956b864b6eef565e.
    /// Vector 2: base_seed=123456789, sweep_hash=HASH_B, point_index=7, draw_index=3,
    ///   key="burn_seed".
    ///     shasum -a 256 vector2.bin
    ///     -> 3406eb0f09dde40c755022225f4e87c0f956ced26cc1322b83264178060be76d
    ///   First 8 bytes: 0x3406eb0f09dde40c.
    #[test]
    fn derived_seed_matches_an_independently_computed_sha256() {
        assert_eq!(derive_seed(42, HASH_A, 0, 0, "fault1").unwrap(), 0x956b864b6eef565eu64);
        assert_eq!(derive_seed(123_456_789, HASH_B, 7, 3, "burn_seed").unwrap(), 0x3406eb0f09dde40cu64);
    }

    #[test]
    fn derived_seed_is_byte_identical_for_identical_inputs() {
        let a = derive_seed(42, HASH_A, 3, 1, "k").unwrap();
        let b = derive_seed(42, HASH_A, 3, 1, "k").unwrap();
        assert_eq!(a, b);

        // Two tuples differing only in `key` must never derive the same seed -- otherwise a
        // DRM with two Scenario.seeds keys sharing a base value would collapse to one substream
        // (see the module doc comment's "Why key joins the plan's stated tuple" section).
        let by_key_a = derive_seed(1, HASH_A, 0, 0, "ab");
        let by_key_b = derive_seed(1, HASH_A, 0, 0, "a");
        assert_ne!(by_key_a.unwrap(), by_key_b.unwrap());
    }

    #[test]
    fn derived_seeds_are_distinct_across_points_draws_and_keys() {
        let mut seeds = std::collections::HashSet::new();
        let keys = ["k0", "k1"];
        for point in 0..4u32 {
            for draw in 0..4u32 {
                for key in keys {
                    let s = derive_seed(7, HASH_A, point, draw, key).unwrap();
                    assert!(seeds.insert(s), "collision at point={point} draw={draw} key={key}");
                }
            }
        }
        assert_eq!(seeds.len(), 4 * 4 * 2);
    }

    #[test]
    fn derived_seed_changes_when_the_sweep_hash_changes() {
        let a = derive_seed(42, HASH_A, 0, 0, "k").unwrap();
        let b = derive_seed(42, HASH_B, 0, 0, "k").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn refuses_a_sweep_hash_that_is_not_64_lowercase_hex_chars() {
        assert!(matches!(derive_seed(1, "too_short", 0, 0, "k"), Err(SweepError::InvalidSweepHash { .. })));
        assert!(matches!(derive_seed(1, &"a".repeat(65), 0, 0, "k"), Err(SweepError::InvalidSweepHash { .. })));
        let uppercase = "A".repeat(64);
        assert!(matches!(derive_seed(1, &uppercase, 0, 0, "k"), Err(SweepError::InvalidSweepHash { .. })), "uppercase hex must be refused, not silently lowercased");
    }
}
