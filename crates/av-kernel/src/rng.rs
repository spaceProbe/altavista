//! A deterministic PCG64 pseudo-random source, seeded from `Scenario.seeds` (ADR-005 section
//! 5, `docs/adr/005-simulation-kernel.md`): "Every random element draws from a seeded PCG64
//! keyed by `Scenario.seeds[<fault id>]`, so the same DRM produces the same fault realization
//! on every run and on every host." This module is the generator; [`super::drm::fault`] is
//! where it is actually keyed by a fault id and used.
//!
//! ## Why hand-rolled, not `rand`/`rand_pcg`
//!
//! ADR-004 (spoore, referenced by this ADR's own "References" section) treats a seed as a
//! logged input whose exact output sequence is part of the replay contract, not an
//! implementation detail a dependency is free to change. `rand`'s `StdRng` documents its own
//! algorithm as unspecified and subject to change between releases; even a *named* algorithm
//! crate (`rand_pcg`) is still a dependency a routine `cargo update` -- or `cargo deny`'s own
//! license/advisory churn -- could bump without this crate's own review catching an output
//! change. Pinning the algorithm here, in three-dozen lines, means it can never drift out from
//! under a golden replay, and keeps `cargo deny check` clean (no additional crate to license-
//! and advisory-audit for a generator this small). This is the same reasoning
//! `spoore-scenarios::rng::DeterministicRng` documents; this is an independent implementation
//! of the same published construction (`av-kernel` does not depend on `spoore`), not a copy of
//! that crate.
//!
//! ## Algorithm: PCG64 "oneseq" XSL-RR (128-bit state, 64-bit output)
//!
//! This is O'Neill's `pcg_oneseq_128_xsl_rr_64` -- one of the family described in ["PCG: A
//! Family of Simple Fast Space-Efficient Statistically Good Algorithms for Random Number
//! Generation"](https://www.pcg-random.org/pdf/toms-oneill-pcg-family-v1.02.pdf) and specified
//! exactly (down to the constants) by the reference implementation at
//! <https://github.com/imneme/pcg-c> (`include/pcg_variants.h`,
//! `pcg_oneseq_128_srandom_r`/`_step_r`/`pcg_output_xsl_rr_128_64`). "Oneseq" (as opposed to
//! "setseq") is the single-fixed-stream member of the family: the multiplier *and* increment
//! are both fixed published constants, and the only per-instance input is one seed folded into
//! the initial state -- exactly the shape `Scenario.seeds[<fault id>]` needs (one `u64` in, one
//! reproducible stream out), with no second stream-selector value to invent or place
//! incorrectly.
//!
//! - **State**: 128 bits (`u128`), advanced each step by `state = state * MULTIPLIER +
//!   INCREMENT` (a linear congruential generator).
//! - **`MULTIPLIER`** = `0x2360_ED05_1FC6_5DA4_4385_DF64_9FCC_F645` -- `PCG_DEFAULT_MULTIPLIER_128`
//!   (`PCG_128BIT_CONSTANT(2549297995355413924u, 4865540595714422341u)` in `pcg_variants.h`).
//! - **`INCREMENT`** = `0x5851_F42D_4C95_7F2D_1405_7B7E_F767_814F` -- `PCG_DEFAULT_INCREMENT_128`
//!   (`PCG_128BIT_CONSTANT(6364136223846793005u, 1442695040888963407u)`), the same Knuth-derived
//!   constant used by `oneseq`'s fixed stream (not derived from any per-instance value).
//! - **Seeding** (`pcg_oneseq_128_srandom_r`): `state = 0; step(); state += seed; step();` --
//!   the standard PCG "seed-then-step-twice" dance, which is why even adjacent low-entropy
//!   seeds (0, 1, 2, ...) start well-separated rather than correlated streams (see
//!   `low_entropy_seeds_are_well_separated` below).
//! - **Output** (`pcg_output_xsl_rr_128_64`, "XSL RR" = xorshift-low, random-rotate): fold the
//!   128-bit state's two halves together with xor, then rotate the low 64 bits right by the
//!   state's own top 6 bits (`state >> 122`) -- `rotr64((state >> 64) ^ state, state >> 122)`.
//!
//! ## Validation
//!
//! [`tests::oneseq_pcg64_matches_the_published_reference_vector`] below checks this
//! implementation's first six `next_u64()` outputs for seed `42` against the pcg-c reference
//! implementation's own checked-in expected output
//! (`imneme/pcg-c`, `test-low/check-oneseq-128-xsl-rr-64.c` + `test-low/expected/
//! check-oneseq-128-xsl-rr-64.out`, fetched and cross-checked against `pcg_variants.h`'s source
//! during this task's own development -- not reproduced from memory). This is a genuine
//! published reference vector for the exact algorithm and seed this module uses, not merely a
//! self-consistency check. Beyond that single external check, the remaining tests are
//! self-consistency and statistical-shape checks only (same seed -> same stream; different
//! seeds -> uncorrelated streams; low-entropy seeds well separated; uniform mean/variance) --
//! no second independently-published multi-value vector for arbitrary seeds was available to
//! check against offline, so those properties are verified by construction and by statistics,
//! not against a second external oracle.
//!
//! ## Key derivation from `Scenario.seeds`
//!
//! `Scenario.seeds` (`proto/altavista/v1/system.proto`) is `map<string, uint64>`, generated
//! crate-wide as `BTreeMap<String, u64>` (`crates/av-cdm/build.rs`'s
//! `prost_build::Config::btree_map(["."])`, chosen for exactly this reason: ADR-004 "no
//! `HashMap` iteration on any output path"). [`seed_for`] is a single keyed lookup --
//! `seeds.get(fault_id)` -- so it does not depend on the map's construction or wire order at
//! all: `BTreeMap::get` returns the same answer regardless of what order `.insert()` calls (or
//! the original protobuf wire encoder) happened to run in. The "iterate it sorted" rule only
//! bites a caller that needs to fold the *entire* map into one derived value (this module has
//! no such caller today; ADR-005 section 5 only ever indexes by a single fault id) -- and for
//! that case, `BTreeMap`'s iteration order is sorted by key unconditionally, so it is already
//! the one order two content-identical seed maps can never disagree about, however differently
//! they were built or decoded from the wire.

use std::collections::BTreeMap;

/// `PCG_DEFAULT_MULTIPLIER_128` (`pcg_variants.h`): the fixed 128-bit LCG multiplier every
/// `oneseq`/`setseq` PCG64 variant shares.
const MULTIPLIER: u128 = 0x2360_ED05_1FC6_5DA4_4385_DF64_9FCC_F645;
/// `PCG_DEFAULT_INCREMENT_128` (`pcg_variants.h`): the fixed 128-bit LCG increment the
/// "oneseq" family uses for every instance (as opposed to "setseq", which derives a
/// per-instance increment from a second seed value this module does not need -- see the
/// module doc comment).
const INCREMENT: u128 = 0x5851_F42D_4C95_7F2D_1405_7B7E_F767_814F;

/// A PCG64 "oneseq" XSL-RR generator (see the module doc comment for the exact algorithm and
/// its published reference vector). Seeded once from a single `u64`; every subsequent draw
/// advances its own 128-bit state deterministically -- the same seed always produces the same
/// sequence of draws, on every host, forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcg64 {
    state: u128,
}

impl Pcg64 {
    /// Seed a new stream. Two different seeds are, with overwhelming probability, two
    /// unrelated streams (`different_seeds_give_uncorrelated_streams` below); two calls with
    /// the *same* seed always agree bit for bit, forever (`the_same_seed_gives_the_same_
    /// stream` below) -- the entire property ADR-005 section 5 and ADR-004 rely on.
    pub fn new(seed: u64) -> Self {
        let mut rng = Pcg64 { state: 0 };
        rng.step();
        rng.state = rng.state.wrapping_add(seed as u128);
        rng.step();
        rng
    }

    fn step(&mut self) {
        self.state = self.state.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT);
    }

    /// A uniformly distributed `u64` -- the generator's native output width.
    pub fn next_u64(&mut self) -> u64 {
        self.step();
        // XSL RR: xor-fold the two 64-bit halves of the state together, then rotate right by
        // the state's own top 6 bits (`pcg_output_xsl_rr_128_64`).
        let xorshifted = ((self.state >> 64) ^ self.state) as u64;
        let rot = (self.state >> 122) as u32;
        xorshifted.rotate_right(rot)
    }

    /// A uniform `f64` in `[0, 1)`. Takes the top 53 bits of [`next_u64`](Self::next_u64) --
    /// exactly the `f64` mantissa width -- so every representable value in the range is
    /// reachable and none is double-counted.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1_u64 << 53) as f64)
    }

    /// A uniform `f64` in `[low, high)`.
    pub fn uniform_range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.uniform()
    }

    /// `true` with probability `p` (`p` outside `[0, 1]` saturates rather than panicking --
    /// `uniform()` is always `< 1.0` and `>= 0.0`, so `p >= 1.0` is always `true` and `p <= 0.0`
    /// is always `false`).
    pub fn bernoulli(&mut self, p: f64) -> bool {
        self.uniform() < p
    }

    /// A standard-normal (`N(0,1)`) draw via the Box-Muller transform (question 100, the Gates
    /// maneuver execution error model's "three standard normals"). Consumes exactly two
    /// [`uniform`](Self::uniform) draws per call; the paired second normal Box-Muller also
    /// produces is discarded rather than cached across calls -- a deliberate, documented
    /// simplification: caching would make one "logical" `standard_normal()` call sometimes
    /// consume the underlying stream and sometimes not (depending on whether a cached value was
    /// pending), which is a subtler determinism/reproducibility hazard than the wasted second
    /// draw is worth. `u1` is mapped to `(0, 1]` (`1.0 - uniform()`, not `uniform()` itself,
    /// which can be exactly `0.0`) so `u1.ln()` never sees `ln(0) = -inf`.
    pub fn standard_normal(&mut self) -> f64 {
        let u1 = 1.0 - self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// FNV-1a, 64-bit (<https://datatracker.ietf.org/doc/html/draft-eastlake-fnv>): a tiny,
/// dependency-free, non-cryptographic string hash, hand-rolled for exactly the reason the
/// module doc comment gives for hand-rolling PCG64 itself (no additional crate to license/
/// advisory-audit for something this small, and no risk of an upstream algorithm change
/// silently breaking a replay). Used only by [`event_rng`] to fold an event id into a
/// well-distributed `u64` -- **not** used anywhere a cryptographic hash is required
/// (`crate::drm::hash` owns canonical artifact hashing with SHA-256; this is a disjoint,
/// non-cryptographic use and does not relax `deny.toml`'s crypto rules).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// A fresh, deterministic PCG64 substream for one named event, keyed by both `base_seed` (a
/// value looked up from `Scenario.seeds`, e.g. via [`seed_for`]) and `event_id` (docs/
/// open-questions.md question 100: "a fresh substream per event id so adding an event does not
/// shift another event's draw"). Two different scenario events -- whether or not they name the
/// *same* `Scenario.seeds` key -- get unrelated streams, because each stream's own seed is
/// `base_seed XOR fnv1a_64(event_id)`: this event's own seed depends on nothing but its own
/// `base_seed`/`event_id` pair, so adding, removing, or reordering *other* events (even ones
/// sharing the same `base_seed`) can never change it. See
/// [`tests::event_substream_is_independent_of_other_events`] and
/// `crates/av-kernel/tests/gates_execution_error.rs`'s end-to-end proof of the same property
/// through the full DRM executor.
pub fn event_rng(base_seed: u64, event_id: &str) -> Pcg64 {
    Pcg64::new(base_seed ^ fnv1a_64(event_id.as_bytes()))
}

/// Look up the seed for `key` (an ADR-005 section 5 fault id: `Scenario.seeds[<fault id>]`) in
/// a decoded `Scenario.seeds` map. See the module doc comment's "Key derivation" section for
/// why this is order-independent. Returns `None` (rather than a default/derived fallback) when
/// `key` has no entry -- ADR-004 "seeds are logged inputs": a caller that needs randomness and
/// finds no logged seed must refuse, not synthesize one silently.
pub fn seed_for(seeds: &BTreeMap<String, u64>, key: &str) -> Option<u64> {
    seeds.get(key).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pcg-c reference implementation's own checked-in expected output for
    /// `pcg_oneseq_128_xsl_rr_64_random_r` seeded with `pcg_oneseq_128_srandom_r(&rng, 42u)` --
    /// exactly this module's algorithm and seed -- fetched from
    /// `imneme/pcg-c` (`test-low/expected/check-oneseq-128-xsl-rr-64.out`, "Round 1"'s
    /// `64bit:`/`Again:` line, cross-checked against that repository's own
    /// `test-low/check-oneseq-128-xsl-rr-64.c` harness and `include/pcg_variants.h` source
    /// during this task's development) -- a genuine external oracle, not a self-consistency
    /// check.
    #[test]
    fn oneseq_pcg64_matches_the_published_reference_vector() {
        let expected: [u64; 6] = [0x287472e87ff5705a, 0xbbd190b04ed0b545, 0xb6cee3580db14880, 0xbf5f7d7e4c3d1864, 0x734eedbe7e50bbc5, 0xa5b6b5f867691c77];
        let mut rng = Pcg64::new(42);
        let got: Vec<u64> = (0..6).map(|_| rng.next_u64()).collect();
        assert_eq!(got, expected, "first 6 draws for seed 42 must match the pcg-c reference implementation's own recorded output");
    }

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let mut a = Pcg64::new(42);
        let mut b = Pcg64::new(42);
        for _ in 0..1_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_give_uncorrelated_streams() {
        let mut a = Pcg64::new(1);
        let mut b = Pcg64::new(2);
        let differing = (0..100).filter(|_| a.next_u64() != b.next_u64()).count();
        assert!(differing > 95, "streams are suspiciously correlated");
    }

    #[test]
    fn low_entropy_seeds_are_well_separated() {
        // Seeds 0..8 are exactly what a fault author reaches for in Scenario.seeds. A naive
        // seeding scheme would leave them correlated; the seed-then-step-twice dance
        // (`Pcg64::new`) exists specifically to prevent that.
        let firsts: Vec<u64> = (0..8u64).map(|s| Pcg64::new(s).next_u64()).collect();
        let mut sorted = firsts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), firsts.len(), "seeds collided: {firsts:?}");
    }

    #[test]
    fn uniform_stays_in_range_and_has_the_right_moments() {
        let mut rng = Pcg64::new(7);
        let xs: Vec<f64> = (0..200_000)
            .map(|_| {
                let u = rng.uniform();
                assert!((0.0..1.0).contains(&u), "{u} outside [0, 1)");
                u
            })
            .collect();
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        // U(0,1): mean 1/2, variance 1/12.
        assert!((mean - 0.5).abs() < 0.005, "mean {mean}");
        assert!((var - 1.0 / 12.0).abs() < 0.005, "variance {var}");
    }

    #[test]
    fn uniform_range_maps_correctly() {
        let mut rng = Pcg64::new(13);
        for _ in 0..10_000 {
            let u = rng.uniform_range(-5.0, 5.0);
            assert!((-5.0..5.0).contains(&u), "{u}");
        }
    }

    #[test]
    fn bernoulli_has_the_right_rate() {
        let mut rng = Pcg64::new(41);
        let hits = (0..100_000).filter(|_| rng.bernoulli(0.3)).count() as f64 / 100_000.0;
        assert!((hits - 0.3).abs() < 0.01, "{hits}");
    }

    #[test]
    fn bernoulli_of_zero_and_one_are_absolute() {
        let mut rng = Pcg64::new(43);
        for _ in 0..1_000 {
            assert!(!rng.bernoulli(0.0));
            assert!(rng.bernoulli(1.0));
        }
    }

    #[test]
    fn standard_normal_has_zero_mean_and_unit_variance() {
        let mut rng = Pcg64::new(99);
        let xs: Vec<f64> = (0..200_000).map(|_| rng.standard_normal()).collect();
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        assert!((mean - 0.0).abs() < 0.02, "mean {mean}");
        assert!((var - 1.0).abs() < 0.02, "variance {var}");
    }

    #[test]
    fn standard_normal_is_deterministic_for_the_same_seed() {
        let mut a = Pcg64::new(55);
        let mut b = Pcg64::new(55);
        for _ in 0..1_000 {
            assert_eq!(a.standard_normal(), b.standard_normal());
        }
    }

    #[test]
    fn event_rng_is_deterministic_for_the_same_base_seed_and_id() {
        let mut a = event_rng(42, "burn1");
        let mut b = event_rng(42, "burn1");
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn event_substream_is_independent_of_other_events() {
        // Same base_seed, different event ids -> unrelated streams (the substream depends on
        // the id, not merely on the shared Scenario.seeds value).
        let mut a = event_rng(7, "burn_a");
        let mut b = event_rng(7, "burn_b");
        let differing = (0..50).filter(|_| a.next_u64() != b.next_u64()).count();
        assert!(differing > 45, "streams keyed by different event ids are suspiciously correlated");

        // The core property question 100 asks for: adding a second event (even one sharing the
        // same base_seed) never changes the first event's own draws -- event_rng's substream
        // for "burn_a" is exactly the same whether or not "burn_b" exists at all, because it is
        // a pure function of (base_seed, "burn_a") alone.
        let mut a_alone = event_rng(7, "burn_a");
        let mut a_with_sibling = event_rng(7, "burn_a");
        let _ = event_rng(7, "burn_b"); // constructing a sibling stream must not perturb a's.
        for _ in 0..20 {
            assert_eq!(a_alone.next_u64(), a_with_sibling.next_u64());
        }
    }

    #[test]
    fn seed_for_is_a_direct_keyed_lookup_independent_of_how_the_map_was_built() {
        // Two BTreeMaps holding the same entries, built by inserting in different orders, are
        // the same map (BTreeMap sorts on insert) -- this is what "order-independent" means at
        // the Rust level; see the module doc comment for why this also holds across the wire.
        let mut a = BTreeMap::new();
        a.insert("f1".to_string(), 10u64);
        a.insert("f2".to_string(), 20u64);
        a.insert("f3".to_string(), 30u64);

        let mut b = BTreeMap::new();
        b.insert("f3".to_string(), 30u64);
        b.insert("f1".to_string(), 10u64);
        b.insert("f2".to_string(), 20u64);

        assert_eq!(a, b);
        for key in ["f1", "f2", "f3"] {
            assert_eq!(seed_for(&a, key), seed_for(&b, key));
        }
        assert_eq!(seed_for(&a, "no-such-fault"), None);
    }
}
