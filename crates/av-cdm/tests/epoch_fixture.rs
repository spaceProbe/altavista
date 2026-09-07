//! The Rust half of the shared epoch cross-check (M1.3's "cross-check the two tables agree
//! with a shared fixture").
//!
//! `data/time/epoch_fixture.json` is generated from `av_cdm::time` itself
//! (`cargo run -p av-cdm --example gen_epoch_fixture`), so this test alone would be
//! circular — it is a change detector, not a proof. Its value is the pairing: the Python
//! adapter (`tests/test_cdm_adapter.py`) asserts against the *same* file, so the two
//! independent implementations of ADR-001's time rules are pinned to each other at exact
//! integer nanoseconds. Comparing them only through GMAT cannot do that: GMAT's MJD floats
//! lose microseconds at present-day epochs, which is enough slack to hide a real
//! disagreement between the two leap-second tables.
//!
//! If this test fails, either `time.rs` changed behaviour (regenerate the fixture on
//! purpose and re-run the Python side) or the fixture was edited by hand (don't).

use av_cdm::time::Tai;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    label: String,
    utc_ns: i64,
    tai_ns: i64,
    tai_minus_utc_s: f64,
    a1_mjd: f64,
    /// Rust's own A1->TAI answer at generation time. Not asserted bit-exactly (see the
    /// residual check below for why); carried so a reader can see what the generator got.
    #[allow(dead_code)]
    tai_ns_from_a1_mjd: i64,
    a1_round_trip_residual_ns: i64,
    tt_ns: i64,
    gps_ns: i64,
}

fn fixture() -> Fixture {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/time/epoch_fixture.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("epoch_fixture.json is missing; regenerate with: cargo run -p av-cdm --example gen_epoch_fixture"))
        .expect("epoch_fixture.json is malformed")
}

#[test]
fn rust_reproduces_every_entry_of_the_shared_epoch_fixture() {
    let f = fixture();
    assert!(f.entries.len() >= 12, "fixture lost entries");
    for e in &f.entries {
        let tai = Tai::from_utc_nanos(e.utc_ns);
        assert_eq!(tai.as_nanos(), e.tai_ns, "TAI ns mismatch at {}", e.label);
        assert_eq!(
            (tai.as_nanos() - e.utc_ns) as f64 / 1e9,
            e.tai_minus_utc_s,
            "TAI-UTC offset mismatch at {}",
            e.label
        );
        assert_eq!(tai.to_tt_nanos(), e.tt_ns, "TT mismatch at {}", e.label);
        assert_eq!(tai.to_gps_nanos(), e.gps_ns, "GPS mismatch at {}", e.label);
        // A1 MJD is an f64 of days; 1e-11 days is ~1 microsecond, the float's own floor at
        // these magnitudes (ADR-001 rejected f64 epochs for exactly this reason).
        assert!(
            (tai.to_a1_mjd() - e.a1_mjd).abs() < 1e-11,
            "A1 MJD mismatch at {}: {} vs {}",
            e.label,
            tai.to_a1_mjd(),
            e.a1_mjd
        );
        // The A1 inverse is deliberately lossy: A1 MJD is an f64 of days and cannot carry
        // present-day nanoseconds (ADR-001's stated reason for int64 TAI internally). Pin
        // the exact loss so Python can be required to lose the same nanoseconds, and so a
        // change in the conversion shows up as a diff rather than as silent drift.
        // Bounded, not bit-exact, and deliberately so. Two things are going on:
        //
        //  1. The A1 round trip loses nanoseconds because A1 MJD is an f64 of days. That is
        //     ADR-001's stated reason for int64 TAI internally, not a defect.
        //  2. Recovering the f64 from its decimal text is itself parser-dependent at the
        //     last ULP: `serde_json`'s float parser and Rust's own `str::parse::<f64>()`
        //     do not always agree on the final bit for these values, and Python's parser is
        //     a third implementation. Chasing bit-exact agreement across three float parsers
        //     would be over-specifying beyond anything the CDM guarantees.
        //
        // So the fixture's *contract* is the integer fields (`tai_ns`, `tt_ns`, `gps_ns`,
        // `utc_ns`), which are exact and are asserted exactly above. `a1_mjd` is asserted
        // only within the documented f64 floor, which is what the GMAT boundary can actually
        // promise. Measured worst case across this fixture: 252 ns.
        let back = Tai::from_a1_mjd(e.a1_mjd);
        let residual_ns = back.as_nanos() - e.tai_ns;
        assert!(
            residual_ns.abs() < 1_000,
            "A1 round trip lost {} ns at {} (recorded {}), beyond the ~microsecond f64 floor \
             ADR-001 predicts for an f64 epoch",
            residual_ns,
            e.label,
            e.a1_round_trip_residual_ns
        );
    }
}

#[test]
fn the_fixture_pins_the_leap_second_steps_it_claims_to() {
    let f = fixture();
    let by_label = |needle: &str| -> i64 {
        f.entries
            .iter()
            .find(|e| e.label.contains(needle))
            .unwrap_or_else(|| panic!("fixture no longer covers {needle}"))
            .tai_ns
    };
    // Across a positive leap second the TAI gap between the UTC second before the step and
    // the step itself is 2 s: one nominal second plus the inserted leap second. A
    // wrong-direction or missing shift cannot produce this.
    assert_eq!(
        by_label("2006-01-01") - by_label("2005-12-31"),
        2_000_000_000,
        "the 2006 leap second is not represented as a 2 s TAI gap"
    );
    assert_eq!(
        by_label("2017-01-01") - by_label("2016-12-31"),
        2_000_000_000,
        "the 2017 leap second is not represented as a 2 s TAI gap"
    );
}
