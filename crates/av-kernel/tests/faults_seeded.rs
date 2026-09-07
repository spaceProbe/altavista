//! M8.3 (ADR-005 section 5, `docs/adr/005-simulation-kernel.md`): seeded faults for all three
//! kinds this crate owns -- `FAULT_TARGET_KIND_DYNAMICS` (applied, pre-existing behavior kept
//! green here too), `_PORT` and `_SENSOR` (validated, seeded from `Scenario.seeds`, and
//! explicitly refused -- see `av_kernel::drm::fault`'s own module doc comment for exactly why
//! neither has a runtime to apply to yet).
//!
//! This file exercises `drm::fault`'s public functions directly (unit-adjacent, not through
//! `executor::execute`): `executor.rs`'s own fault-collecting loop only ever looks at DYNAMICS
//! faults today (see `fault`'s "Integration note" doc comment), and `executor.rs` is outside
//! this task's file ownership, so a PORT/SENSOR fault declared in a real DRM is not yet reached
//! by `execute()` at all. `tests/faults_determinism.rs` is the acceptance test that runs the
//! *existing* end-to-end pipeline (DYNAMICS faults, no PORT/SENSOR) twice and compares
//! serialized bytes.

use std::collections::BTreeMap;

use av_cdm::pb::{Fault, FaultTargetKind};
use av_kernel::drm::{fault, DrmError};
use av_kernel::rng::{seed_for, Pcg64};

fn port_fault(id: &str, kind: &str) -> Fault {
    Fault { id: id.to_string(), tai_ns: 1_000_000_000, target_kind: FaultTargetKind::Port as i32, instance: "gs_link".to_string(), target: "link.veh_gs".to_string(), kind: kind.to_string(), ..Default::default() }
}
fn sensor_fault(id: &str, kind: &str) -> Fault {
    Fault { id: id.to_string(), tai_ns: 2_000_000_000, target_kind: FaultTargetKind::Sensor as i32, instance: "veh".to_string(), target: "sensor.imu".to_string(), kind: kind.to_string(), ..Default::default() }
}

/// PORT: every documented kind ("drop", "delay", "corrupt", "duplicate") is accepted through
/// validation and then explicitly refused -- never a silent skip.
#[test]
fn every_documented_port_kind_is_validated_and_then_explicitly_refused() {
    for kind in ["drop", "delay", "corrupt", "duplicate"] {
        let seeds = BTreeMap::from([("f_port".to_string(), 100u64)]);
        let err = fault::realize_unapplied_fault(&port_fault("f_port", kind), &seeds);
        assert!(
            matches!(err, DrmError::FaultTargetKindNotSupported { ref fault_id, ref target_kind, .. } if fault_id == "f_port" && target_kind == "FAULT_TARGET_KIND_PORT"),
            "kind {kind:?}: {err:?}"
        );
    }
}

/// SENSOR: every documented kind ("bias", "noise", "dropout", "misalign") is accepted through
/// validation and then explicitly refused.
#[test]
fn every_documented_sensor_kind_is_validated_and_then_explicitly_refused() {
    for kind in ["bias", "noise", "dropout", "misalign"] {
        let seeds = BTreeMap::from([("f_sensor".to_string(), 200u64)]);
        let err = fault::realize_unapplied_fault(&sensor_fault("f_sensor", kind), &seeds);
        assert!(
            matches!(err, DrmError::FaultTargetKindNotSupported { ref fault_id, ref target_kind, .. } if fault_id == "f_sensor" && target_kind == "FAULT_TARGET_KIND_SENSOR"),
            "kind {kind:?}: {err:?}"
        );
    }
}

/// The refusal is not merely "an error" -- it carries the same reproducible draw the seeded
/// stream would give a real caller, computed via the same `Scenario.seeds[<fault id>]` key
/// derivation ADR-005 section 5 specifies.
#[test]
fn the_refusal_carries_the_deterministic_seeded_draw() {
    let seeds = BTreeMap::from([("f1".to_string(), 999u64)]);
    let err = fault::realize_unapplied_fault(&port_fault("f1", "drop"), &seeds);
    let DrmError::FaultTargetKindNotSupported { realized_draw, .. } = err else { panic!("expected FaultTargetKindNotSupported, got {err:?}") };

    // The same derivation, done by hand: look the fault id up in Scenario.seeds, seed a
    // Pcg64, take its first draw.
    let expected_seed = seed_for(&seeds, "f1").expect("seed present");
    let expected_draw = Pcg64::new(expected_seed).next_u64();
    assert_eq!(realized_draw, expected_draw);
}

/// Two runs of the same PORT/SENSOR fault against the same `Scenario.seeds` produce the exact
/// same realized draw -- ADR-005 section 5's "the same DRM produces the same fault realization
/// on every run and on every host", checked directly against this module rather than only
/// through the full executor.
#[test]
fn the_same_fault_and_seed_map_realize_identically_on_repeat_calls() {
    let seeds = BTreeMap::from([("f_repeat".to_string(), 424242u64)]);
    let a = fault::realize_unapplied_fault(&sensor_fault("f_repeat", "noise"), &seeds);
    let b = fault::realize_unapplied_fault(&sensor_fault("f_repeat", "noise"), &seeds);
    let (DrmError::FaultTargetKindNotSupported { realized_draw: da, .. }, DrmError::FaultTargetKindNotSupported { realized_draw: db, .. }) = (a, b) else { panic!("expected FaultTargetKindNotSupported both times") };
    assert_eq!(da, db);
}

/// Different fault ids (hence different `Scenario.seeds` entries) realize different draws --
/// the streams are not accidentally shared/aliased across faults.
#[test]
fn different_faults_with_different_seeds_realize_different_draws() {
    let seeds = BTreeMap::from([("f_a".to_string(), 1u64), ("f_b".to_string(), 2u64)]);
    let DrmError::FaultTargetKindNotSupported { realized_draw: da, .. } = fault::realize_unapplied_fault(&port_fault("f_a", "drop"), &seeds) else { panic!() };
    let DrmError::FaultTargetKindNotSupported { realized_draw: db, .. } = fault::realize_unapplied_fault(&port_fault("f_b", "drop"), &seeds) else { panic!() };
    assert_ne!(da, db);
}

/// A PORT fault with no matching `Scenario.seeds` entry is a typed `MissingFaultSeed`, not a
/// panic or a silently-assumed default seed (ADR-004: seeds are logged inputs).
#[test]
fn a_port_fault_with_no_scenario_seed_entry_is_refused_with_a_typed_error() {
    let seeds = BTreeMap::new();
    let err = fault::realize_unapplied_fault(&port_fault("f_no_seed", "duplicate"), &seeds);
    assert!(matches!(err, DrmError::MissingFaultSeed { ref fault_id } if fault_id == "f_no_seed"), "{err:?}");
}

/// An unrecognized `kind` for PORT is refused before the seed is even looked up (kind
/// validation happens first) -- checked by giving it an empty seed map and confirming the
/// error is about the kind, not the missing seed.
#[test]
fn an_unrecognized_kind_is_refused_before_seed_lookup() {
    let seeds = BTreeMap::new();
    let err = fault::realize_unapplied_fault(&sensor_fault("f1", "definitely_not_a_real_kind"), &seeds);
    assert!(matches!(err, DrmError::UnknownParameter { .. }), "expected UnknownParameter (kind checked first), got {err:?}");
}

/// `Scenario.seeds` is decoded as `BTreeMap<String, u64>` (`av-cdm/build.rs`'s crate-wide
/// `.btree_map(["."])`), so `seed_for`'s lookup is a direct keyed read -- this test builds the
/// same seed set two different ways (insertion order reversed) and confirms both the map and
/// every fault's realized draw come out identical, demonstrating the order-independence
/// `av_kernel::rng`'s module doc comment documents.
#[test]
fn seed_derivation_does_not_depend_on_the_order_seeds_were_inserted() {
    let mut forward = BTreeMap::new();
    for (k, v) in [("f1", 10u64), ("f2", 20), ("f3", 30)] {
        forward.insert(k.to_string(), v);
    }
    let mut reverse = BTreeMap::new();
    for (k, v) in [("f3", 30u64), ("f2", 20), ("f1", 10)] {
        reverse.insert(k.to_string(), v);
    }
    assert_eq!(forward, reverse, "BTreeMap content must be identical regardless of insertion order");

    for id in ["f1", "f2", "f3"] {
        let a = fault::realize_unapplied_fault(&port_fault(id, "drop"), &forward);
        let b = fault::realize_unapplied_fault(&port_fault(id, "drop"), &reverse);
        let (DrmError::FaultTargetKindNotSupported { realized_draw: da, .. }, DrmError::FaultTargetKindNotSupported { realized_draw: db, .. }) = (a, b) else { panic!() };
        assert_eq!(da, db, "fault {id}: draw must not depend on how the seed map was built");
    }
}

/// `fault::epoch_id_order` implements ADR-005 section 5's required application order --
/// "sorted `(epoch, id)` order" -- ready for `executor.rs` to use in place of its current
/// epoch-only sort (see `fault`'s own "Integration note").
#[test]
fn epoch_id_order_breaks_same_epoch_ties_by_id() {
    let mut faults = vec![port_fault("z", "drop"), port_fault("a", "drop")];
    for f in &mut faults {
        f.tai_ns = 500;
    }
    faults.push({
        let mut earlier = port_fault("late_by_id_but_earliest_epoch", "drop");
        earlier.tai_ns = 100;
        earlier
    });
    faults.sort_by_key(fault::epoch_id_order);
    let ids: Vec<&str> = faults.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["late_by_id_but_earliest_epoch", "a", "z"]);
}
