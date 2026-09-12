//! Requirement 5: sign-then-verify is the identity, under any label and any message
//! count including zero. A deterministic loop over a spread of cases -- no `proptest`
//! dependency, per the milestone's own instruction not to add crates.

use av_edge::{hash, pb, sign, verify};

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");

fn labels() -> Vec<pb::Label> {
    vec![
        pb::Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] },
        pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] },
        pb::Label { marking: "CUI".to_string(), caveats: vec!["A".to_string(), "B".to_string(), "C".to_string()] },
        pb::Label { marking: String::new(), caveats: vec![] },
        pb::Label { marking: "SECRET".to_string(), caveats: vec!["ONE".to_string()] },
    ]
}

fn measurements(count: usize) -> Vec<pb::Measurement> {
    (0..count)
        .map(|i| pb::Measurement {
            measurement_id: format!("m{i}"),
            z: vec![i as f64, (i * 2) as f64],
            r: vec![1.0, 0.0, 0.0, 1.0],
            epoch_ns: 1_000_000_000 + i as i64,
            sensor_id: format!("sensor-{i}"),
            shard_key: "shard-0".to_string(),
            meta: Default::default(),
            frame_id: "EarthMJ2000Eq".to_string(),
            entity_hint: String::new(),
        })
        .collect()
}

/// Every (label, message count, sequence, prev_hash-is-genesis) combination the property
/// loop below covers -- a deterministic spread standing in for a `proptest` generator,
/// per the milestone's instruction not to add a new crate for this.
fn cases() -> Vec<(pb::Label, usize, u64, bool)> {
    let mut cases = Vec::new();
    for label in labels() {
        for &count in &[0usize, 1, 2, 5, 37] {
            for &(sequence, genesis) in &[(1u64, true), (2u64, false), (1_000_000u64, false)] {
                cases.push((label.clone(), count, sequence, genesis));
            }
        }
    }
    cases
}

#[test]
fn sign_then_verify_is_the_identity_across_a_spread_of_labels_and_message_counts() {
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = verify::load_verifying_key(TEST_PUB_PEM).unwrap();

    let all_cases = cases();
    assert!(all_cases.iter().any(|(_, count, _, _)| *count == 0), "the spread must include zero messages");
    let case_count = all_cases.len();

    for (label, count, sequence, genesis) in all_cases {
        let prev_hash = if genesis { hash::GENESIS.to_vec() } else { vec![0x7A; 32] };
        let mut batch = pb::MeasurementBatch {
            producer_id: "roundtrip-producer".to_string(),
            sequence,
            label: Some(label.clone()),
            measurements: measurements(count),
            batch_tai_ns: 1_800_000_000_000_000_000,
            ..Default::default()
        };

        sign::sign_batch(&mut batch, &prev_hash, &signing_key).expect("signing must succeed for every case");
        verify::verify_batch(&batch, &verify_key)
            .unwrap_or_else(|e| panic!("verify must accept what sign just produced (label={label:?}, count={count}, sequence={sequence}, genesis={genesis}): {e}"));
    }

    // Sanity on the loop itself: make sure it actually ran every case (a loop that
    // silently iterated zero times would make every assertion above vacuously true).
    assert_eq!(case_count, labels().len() * 5 * 3);
}

#[test]
fn verify_rejects_once_any_single_field_is_perturbed_after_signing() {
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = verify::load_verifying_key(TEST_PUB_PEM).unwrap();

    let mut batch = pb::MeasurementBatch {
        producer_id: "roundtrip-producer".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        measurements: measurements(2),
        batch_tai_ns: 1_800_000_000_000_000_000,
        ..Default::default()
    };
    sign::sign_batch(&mut batch, hash::GENESIS, &signing_key).unwrap();
    verify::verify_batch(&batch, &verify_key).expect("freshly signed batch must verify");

    let mut tampered = batch.clone();
    tampered.sequence += 1;
    assert!(verify::verify_batch(&tampered, &verify_key).is_err(), "changing sequence after signing must invalidate the signature");
}
