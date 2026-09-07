//! Generate `data/time/epoch_fixture.json`: the shared epoch fixture that pins the Rust and
//! Python implementations of the CDM's time conventions to each other.
//!
//! `crates/av-cdm/src/time.rs` is the reference implementation (ADR-001 "Time"), so this
//! example emits *its* answers and both sides then assert against the same file:
//!
//! - Rust: `crates/av-cdm/tests/epoch_fixture.rs`
//! - Python: `tests/test_cdm_adapter.py`
//!
//! Without a shared fixture the two implementations can only be compared through GMAT, whose
//! MJD floats lose ~microseconds at present-day epochs — which is enough slack to hide a
//! genuine integer-nanosecond disagreement between the two tables.
//!
//! Regenerate deliberately, never from a test:
//!   cargo run -p av-cdm --example gen_epoch_fixture
//! Then re-run `cargo test --workspace` and `.venv/bin/python -m pytest -q`.

use av_cdm::time::Tai;

/// Epochs chosen to exercise every documented rule in `time.rs`: the pre-1972 clamp, each
/// side of several leap-second steps, the post-2017 extrapolation, and ordinary dates.
const UTC_EPOCHS: &[(&str, i64)] = &[
    ("1970-01-01T00:00:00Z (pre-1972 clamp)", 0),
    ("1971-12-31T23:59:59Z (last instant of the clamp)", 63_071_999_000_000_000),
    ("1972-01-01T00:00:00Z (first table entry, offset 10)", 63_072_000_000_000_000),
    ("1972-07-01T00:00:00Z (offset 11)", 78_796_800_000_000_000),
    ("1998-12-31T23:59:59Z (one second before the 1999 step)", 915_148_799_000_000_000),
    ("1999-01-01T00:00:00Z (offset 32)", 915_148_800_000_000_000),
    ("2005-12-31T23:59:59Z (one second before the 2006 step)", 1_136_073_599_000_000_000),
    ("2006-01-01T00:00:00Z (offset 33)", 1_136_073_600_000_000_000),
    ("2012-07-01T00:00:00Z (offset 35)", 1_341_100_800_000_000_000),
    ("2016-12-31T23:59:59Z (one second before the final step)", 1_483_228_799_000_000_000),
    ("2017-01-01T00:00:00Z (final table entry, offset 37)", 1_483_228_800_000_000_000),
    ("2026-09-02T00:00:00Z (post-2017 extrapolation)", 1_788_307_200_000_000_000),
];

fn main() {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"generated_by\": \"cargo run -p av-cdm --example gen_epoch_fixture\",\n");
    out.push_str(
        "  \"reference_implementation\": \"crates/av-cdm/src/time.rs (ADR-001 Time); \
         Python must reproduce these exactly\",\n",
    );
    out.push_str(
        "  \"note\": \"tai_ns is integer and exact. a1_mjd is an f64 of days, emitted in \
         shortest-round-trip form ({:?}) so both Rust and Python parse back the identical \
         f64 -- a fixed-precision format such as {:.17} does NOT round-trip and silently \
         shifts the value by one ULP (~157 ns at these magnitudes), which is exactly the \
         class of error this fixture exists to catch.\",\n",
    );
    out.push_str("  \"entries\": [\n");
    for (i, (label, utc_ns)) in UTC_EPOCHS.iter().enumerate() {
        let tai = Tai::from_utc_nanos(*utc_ns);
        let comma = if i + 1 == UTC_EPOCHS.len() { "" } else { "," };
        // Emit, then re-parse, and record answers for the RE-PARSED value. The fixture must
        // describe what a reader of the file actually sees: `a1_mjd` is written as a decimal
        // string, and if the shortest-round-trip decimal does not recover the original f64
        // bit-for-bit, then the in-memory answer is not the answer any consumer can compute.
        // Recording the in-memory one instead makes the fixture unreproducible by its own
        // readers (Rust and Python alike) — which is how a 1-ULP, ~157 ns discrepancy first
        // showed up here.
        let a1_text = format!("{:?}", tai.to_a1_mjd());
        let a1: f64 = a1_text.parse().expect("f64 formatted by Rust must parse back");
        // The A1 round trip is NOT exact and is not supposed to be: A1 MJD is an f64 of
        // days, which cannot carry present-day nanoseconds (ADR-001 rejected f64 epochs for
        // exactly this reason). What matters for the cross-check is that Python loses the
        // *same* nanoseconds Rust does, so the fixture records Rust's own inverse and the
        // residual it implies rather than pretending the round trip closes.
        let a1_back = Tai::from_a1_mjd(a1);
        out.push_str(&format!(
            "    {{\"label\": \"{}\", \"utc_ns\": {}, \"tai_ns\": {}, \"tai_minus_utc_s\": {}, \
             \"a1_mjd\": {}, \"tai_ns_from_a1_mjd\": {}, \"a1_round_trip_residual_ns\": {}, \
             \"tt_ns\": {}, \"gps_ns\": {}}}{}\n",
            label,
            utc_ns,
            tai.as_nanos(),
            (tai.as_nanos() - utc_ns) as f64 / 1e9,
            a1_text,
            a1_back.as_nanos(),
            a1_back.as_nanos() - tai.as_nanos(),
            tai.to_tt_nanos(),
            tai.to_gps_nanos(),
            comma
        ));
    }
    out.push_str("  ]\n}\n");
    print!("{out}");
}
