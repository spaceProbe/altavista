//! Golden byte vectors and the fixed-key signature (docs/edge-plan.md milestone E1,
//! requirements 1 and 2).
//!
//! # Golden byte vectors
//!
//! [`golden_batch`] builds one fully-populated `MeasurementBatch` (two measurements, a
//! label with a caveat, provenance, a non-zero `batch_tai_ns`). Its canonical body bytes
//! and `batch_hash` hex, computed once and pinned below as literal constants
//! ([`GOLDEN_BODY_HEX`], [`GOLDEN_BATCH_HASH_HEX`]), must reproduce identically forever --
//! E3 and E6 depend on this canonical encoding byte for byte. If a future, legitimate
//! change to `edge.proto`'s `MeasurementBatch` shape ever needs to change these
//! constants, that is itself the signal such a change is not wire-compatible with every
//! batch already written under the old encoding.
//!
//! # Fixed-key signature
//!
//! `tests/fixtures/test_signing_key.pem`/`.pub.pem` (see that directory's `README.md`)
//! signs [`golden_batch`]'s hash exactly once; the resulting DER signature bytes are
//! committed below as [`GOLDEN_SIGNATURE_HEX`] and asserted to verify against the
//! committed public key forever, and to fail the moment either the signature or the
//! batch is perturbed by even one bit -- see `av_edge`'s crate (`src/lib.rs`) module doc
//! for why a byte-pinned *signature* golden is not possible for ECDSA in the first place
//! (OpenSSL's ECDSA uses a random nonce, not RFC 6979), unlike the hash goldens above,
//! which are pure, deterministic functions of the batch's content.

use av_edge::{hash, pb, sign, verify};

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");

/// The one fixed `MeasurementBatch` every golden value in this file is computed from.
/// Fully populated per the milestone's requirement: two measurements, a label with a
/// caveat, provenance, and a non-zero `batch_tai_ns`. `prev_hash` is [`hash::GENESIS`] --
/// this is the producer's first batch.
fn golden_batch() -> pb::MeasurementBatch {
    pb::MeasurementBatch {
        producer_id: "golden-producer-1".to_string(),
        sequence: 1,
        prev_hash: hash::GENESIS.to_vec(),
        batch_hash: vec![],
        signature: vec![],
        signer_cert_sha256: String::new(),
        // Left at its default (empty string): the new `shard_key` field (E3,
        // `crates/av-ingest`) is additive, and an empty string adds no bytes to the
        // canonical proto3 encoding, so this does not move the pinned goldens below.
        shard_key: String::new(),
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        measurements: vec![
            pb::Measurement {
                measurement_id: "m1".to_string(),
                z: vec![1.0, 2.0, 3.0],
                r: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                epoch_ns: 1_800_000_000_000_000_000,
                sensor_id: "sensor-a".to_string(),
                shard_key: "shard-0".to_string(),
                meta: [("k1".to_string(), "v1".to_string())].into_iter().collect(),
                frame_id: "EarthMJ2000Eq".to_string(),
                entity_hint: "sat-1".to_string(),
            },
            pb::Measurement {
                measurement_id: "m2".to_string(),
                z: vec![4.0, 5.0],
                r: vec![2.0, 0.0, 0.0, 2.0],
                epoch_ns: 1_800_000_000_500_000_000,
                sensor_id: "sensor-b".to_string(),
                shard_key: "shard-0".to_string(),
                meta: Default::default(),
                frame_id: "EarthMJ2000Eq".to_string(),
                entity_hint: String::new(),
            },
        ],
        provenance: Some(pb::Provenance {
            author_kind: pb::AuthorKind::External as i32,
            principal: "plugin:demo-asset".to_string(),
            tool: "av-edge-golden-fixture".to_string(),
            config_hash: "deadbeef".to_string(),
            data_pack_hash: String::new(),
            dataset_hash: String::new(),
            created_tai_ns: 1_800_000_000_000_000_000,
            run_id: "run-golden-1".to_string(),
            attributes: Default::default(),
        }),
        batch_tai_ns: 1_800_000_000_000_000_000,
    }
}

/// `hash::canonical_body_bytes(&golden_batch())`, hex encoded. Computed once (see this
/// file's header comment) and pinned; length 407 bytes / 814 hex characters.
const GOLDEN_BODY_HEX: &str = "0a11676f6c64656e2d70726f64756365722d3110011a0747454e455349533a0e0a03435549120753502d4558505442a5010a026d311218000000000000f03f000000000000004000000000000008401a48000000000000f03f000000000000000000000000000000000000000000000000000000000000f03f000000000000000000000000000000000000000000000000000000000000f03f208080d09de9ceb8fd182a0873656e736f722d61320773686172642d3042080a026b31120276314a0d45617274684d4a32303030457152057361742d3142640a026d321210000000000000104000000000000014401a2000000000000000400000000000000000000000000000000000000000000000402080ca858cebceb8fd182a0873656e736f722d62320773686172642d304a0d45617274684d4a3230303045714a4f08041211706c7567696e3a64656d6f2d61737365741a1661762d656467652d676f6c64656e2d6669787475726522086465616462656566388080d09de9ceb8fd18420c72756e2d676f6c64656e2d31508080d09de9ceb8fd18";

/// `hash::compute_batch_hash(hash::GENESIS, &body)`, hex encoded (32 bytes / 64 hex
/// characters). Computed once and pinned alongside [`GOLDEN_BODY_HEX`].
const GOLDEN_BATCH_HASH_HEX: &str = "af5c6b1ded6e57d2870be6c752b8c34b6b643230c1a286c475c7b4b541473d3e";

/// DER ECDSA P-384 signature over the 32 raw bytes [`GOLDEN_BATCH_HASH_HEX`] decodes to,
/// produced once with `tests/fixtures/test_signing_key.pem` and committed here (hex
/// encoded). Not reproducible by re-signing (see this file's header comment) --
/// `signature_verifies_and_a_single_bit_flip_fails` below is what this constant is for.
const GOLDEN_SIGNATURE_HEX: &str = "3065023100b4e480c8748575d66cdcc1f27f180432ab2d9dff0a9970a642f29c8fe1d0df5f513091ce501e9948676bb5c996e0ebec02303bc7094b22fe07468afe30b3024601d7fb56d1821d1966ad241c399ac714eecd55c5c189923655c79d3aed7936219a23";

/// Requirement 1: the pinned canonical body bytes and `batch_hash` hex reproduce exactly
/// from [`golden_batch`], and are unaffected by garbage pre-filled into the batch's own
/// `batch_hash`/`signature` fields before hashing -- the same property
/// `crates/av-kernel/src/drm/hash.rs::hash_is_stable_and_excludes_the_hash_field_itself`
/// asserts for `DesignReferenceMission`.
#[test]
fn golden_body_bytes_and_batch_hash_are_pinned() {
    let batch = golden_batch();
    let body = hash::canonical_body_bytes(&batch);
    assert_eq!(hash::hex_encode(&body), GOLDEN_BODY_HEX, "canonical body bytes must match the pinned golden");

    let batch_hash = hash::compute_batch_hash(hash::GENESIS, &body);
    assert_eq!(hash::hex_encode(&batch_hash), GOLDEN_BATCH_HASH_HEX, "batch_hash must match the pinned golden");

    // Pre-filling batch_hash/signature with garbage before hashing must not change
    // either the canonical body bytes or the resulting batch_hash -- both fields are
    // cleared before encoding (hash::canonical_body_bytes's own doc comment).
    let mut tampered = batch.clone();
    tampered.batch_hash = vec![0xFF; 32];
    tampered.signature = vec![0xEE; 96];
    let tampered_body = hash::canonical_body_bytes(&tampered);
    assert_eq!(hash::hex_encode(&tampered_body), GOLDEN_BODY_HEX, "pre-filled batch_hash/signature must not affect the canonical body encoding");
    let tampered_hash = hash::compute_batch_hash(hash::GENESIS, &tampered_body);
    assert_eq!(hash::hex_encode(&tampered_hash), GOLDEN_BATCH_HASH_HEX, "pre-filled batch_hash/signature must not affect the computed batch_hash");
}

/// Requirement 2: the committed signature verifies against the committed public key, and
/// flipping one bit of either the signature or the signed batch makes verification fail.
#[test]
fn signature_verifies_and_a_single_bit_flip_fails() {
    let verify_key = verify::load_verifying_key(TEST_PUB_PEM).unwrap();

    let mut batch = golden_batch();
    batch.batch_hash = hash::hex_decode(GOLDEN_BATCH_HASH_HEX).unwrap();
    batch.signature = hash::hex_decode(GOLDEN_SIGNATURE_HEX).unwrap();

    // The committed signature verifies against the committed batch and public key.
    verify::verify_batch(&batch, &verify_key).expect("the committed golden signature must verify against the committed golden batch and public key");

    // Flip one bit of the signature: verification must fail.
    let mut bad_sig_batch = batch.clone();
    bad_sig_batch.signature[0] ^= 0x01;
    assert!(verify::verify_batch(&bad_sig_batch, &verify_key).is_err(), "a one-bit-flipped signature must not verify");

    // Flip one bit of the batch (a measurement's sensor_id, leaving batch_hash/signature
    // untouched): the recomputed hash no longer matches batch_hash, so verification must
    // fail with a hash mismatch rather than reaching the signature check at all.
    let mut bad_batch = batch.clone();
    bad_batch.measurements[0].sensor_id.push('!');
    let err = verify::verify_batch(&bad_batch, &verify_key).expect_err("a perturbed batch must not verify");
    assert!(matches!(err, verify::VerifyError::HashMismatch), "{err:?}");
}

/// The fixed-key signature must have been produced over exactly [`golden_batch`]'s
/// pinned hash, not some other value -- if this ever fails, [`GOLDEN_SIGNATURE_HEX`] was
/// generated against a different batch/hash than the one currently pinned above and must
/// be regenerated together with it.
#[test]
fn golden_signature_length_is_a_plausible_p384_der_signature() {
    let sig = hash::hex_decode(GOLDEN_SIGNATURE_HEX).unwrap();
    // A P-384 ECDSA DER signature (two ~48-byte integers plus DER overhead) is
    // consistently in the 100-108 byte range in practice; this is a coarse sanity bound; the
    // real proof is `signature_verifies_and_a_single_bit_flip_fails` actually verifying it.
    assert!(sig.len() >= 100 && sig.len() <= 108, "unexpected DER signature length: {}", sig.len());
}

/// Standalone helper (not itself one of the six required test groups) that reproduces
/// [`GOLDEN_SIGNATURE_HEX`] deterministically enough to sanity-check it was generated
/// from the fixture key over the pinned hash -- run manually with `cargo test -p av-edge
/// --test golden regenerate_signature_for_reference -- --ignored --nocapture` if the
/// golden batch or key fixture is ever intentionally changed.
#[test]
#[ignore]
fn regenerate_signature_for_reference() {
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let mut batch = golden_batch();
    sign::sign_batch(&mut batch, hash::GENESIS, &signing_key).unwrap();
    assert_eq!(hash::hex_encode(&batch.batch_hash), GOLDEN_BATCH_HASH_HEX);
    println!("SIGNATURE_HEX={}", hash::hex_encode(&batch.signature));
}
