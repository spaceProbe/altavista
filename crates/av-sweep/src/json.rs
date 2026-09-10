//! A hand-written proto3 canonical JSON encoder for exactly the message tree
//! `altavista.v1.SweepResults` reaches (`SweepResults`, `SweepSample`, `ScoreAggregate`,
//! `ScoreResult`, `Provenance`) -- not a generic protobuf-to-JSON mapping, and no new
//! dependency: `serde_json` (already a workspace dependency of this crate) supplies string
//! escaping and finite-float formatting, but the field layout, renaming and omission rules
//! below are this module's own, written by hand against the proto3 JSON canonical mapping
//! (https://protobuf.dev/programming-guides/json/) so that `google.protobuf.json_format.Parse`
//! (the independent oracle `tests/test_sweep_results_json.py` uses) accepts the output.
//!
//! **F2 move, disclosed.** This module lived at `src/bin/av-sweep/json.rs` (F1b) and moved here
//! unchanged (module doc comment aside) so `src/store.rs`'s `FileStudyStore` (F2,
//! `docs/feasibility-plan.md`'s F2 milestone: "reuse the existing JSON encoder rather than
//! writing a second one") can call [`sweep_results_to_json`]/[`sweep_sample_json`] as a library
//! function instead of this crate duplicating the encoder a second time for `samples.jsonl`. The
//! binary (`src/bin/av-sweep/study.rs`) now calls `av_sweep::json::sweep_results_to_json` the
//! same way; `crates/av-sweep/examples/gen_sweep_results_json_golden.rs` likewise now does
//! `use av_sweep::json;` instead of `#[path = "../src/bin/av-sweep/json.rs"] mod json;`. Every
//! test below moved with the module, unchanged, and still passes (including the byte-for-byte
//! golden pin) -- proof this is a pure relocation, not a rewrite.
//!
//! Rules this module applies, all load-bearing (each has a named test in this file's own
//! `#[cfg(test)]` module):
//!
//! - Field names are lowerCamelCase (`sweep_id` -> `sweepId`, `point_index` -> `pointIndex`, ...).
//! - `uint64`/`int64` fields (`SweepSample.seeds`' own map VALUES, `Provenance.created_tai_ns`)
//!   encode as a JSON **string** of decimal digits -- proto3's own canonical mapping for 64-bit
//!   integer types (a JSON number cannot represent the full 64-bit range without precision loss
//!   in every mainstream JSON parser, including Python's and JavaScript's). This is exactly what
//!   the removed single-`uint64` `SweepSample.seed` field did (question 192(b) replaced it with
//!   the `seeds` map); a `map<string, uint64>`'s VALUES get the same string treatment its own
//!   scalar `uint64` predecessor did -- proto3's canonical JSON mapping applies per scalar type,
//!   not per top-level-field-vs-map-value.
//! - **`SweepSample.seeds`, when non-empty, is a JSON object** keyed by the map's own string
//!   keys (unescaped further -- `Scenario.seeds` keys are plain identifiers in every fixture this
//!   crate has seen, and `kv`'s own `esc` call still JSON-escapes them correctly regardless),
//!   each value a quoted decimal string per the bullet above. Omitted entirely when empty (the
//!   general "every other scalar/map field at its zero/empty default is omitted" rule below,
//!   applied to a map the same way it already was for `axisValues`/`scores`). **Field-order
//!   decision, disclosed:** `seeds` is `run.proto`'s field 10, declared after `error`'s field 9;
//!   this encoder emits it in that same relative order -- after `error`, last of `SweepSample`'s
//!   own fields -- matching this module's existing convention of following the proto's own field
//!   declaration order (see `sweep_sample_json`'s own field-by-field body) rather than, say,
//!   grouping it next to `axisValues` (both are per-sample "declared inputs") -- pinned exactly
//!   by `tests::sweep_results_json_matches_a_hand_pinned_expectation`.
//! - `uint32` fields (`point_index`, `draw_index`, `draws`) stay a plain JSON number -- only the
//!   64-bit integer types get the string treatment.
//! - Enum fields (`ScoreResult.unit`, `Provenance.author_kind`) encode by **name**
//!   (`"UNIT_METER"`, `"AUTHOR_KIND_AGENT"`), never by number.
//! - `optional` fields (`ScoreResult.passed`, `ScoreAggregate.pass_fraction`) are emitted only
//!   when `Some`; a proto3 message-typed field (`SweepResults.provenance`) is emitted whenever
//!   the `Option` is `Some`, regardless of whether its own nested fields are all default.
//! - Every other (non-optional) scalar field at its proto3 zero/empty default is **omitted**,
//!   matching the reference `protobuf` library's own default `MessageToJson` behaviour (and,
//!   since omission and "present with the zero value" parse to the identical message either
//!   way, this is a style choice that cannot break the round-trip oracle test either way).
//! - `double` fields: finite values use the shortest round-tripping decimal form (delegated to
//!   `serde_json`'s own float formatter); non-finite values encode as the proto3 JSON mapping's
//!   quoted tokens `"NaN"`/`"Infinity"`/`"-Infinity"` (never a bare `null` or a JSON syntax
//!   error -- neither is valid JSON for a raw `NaN`/`Infinity` token).
//!
//! **Known, disclosed limitation:** finite doubles are formatted by `f64::to_string()`, which
//! never switches to exponential notation (unlike some canonical JSON writers for very large or
//! very small magnitudes) -- still valid JSON, just not the most compact possible form. None of
//! this crate's own data (orbital ranges, seeds-as-strings, scores) ever approaches a magnitude
//! where this matters.

use av_cdm::pb;

fn esc(s: &str) -> String {
    // serde_json's own string serialization: correct JSON escaping (quotes, backslashes,
    // control characters, unicode) without this module re-implementing it.
    serde_json::to_string(s).expect("String -> JSON string serialization is infallible")
}

/// A finite, NaN or infinite `double` as its proto3 JSON token -- see the module doc comment's
/// "double fields" bullet.
fn num(v: f64) -> String {
    if v.is_nan() {
        "\"NaN\"".to_string()
    } else if v == f64::INFINITY {
        "\"Infinity\"".to_string()
    } else if v == f64::NEG_INFINITY {
        "\"-Infinity\"".to_string()
    } else {
        // Finite: serde_json's Number formatter gives the shortest round-tripping decimal form.
        serde_json::Number::from_f64(v).expect("finite by the branches above").to_string()
    }
}

fn u64s(v: u64) -> String {
    format!("\"{v}\"")
}

fn i64s(v: i64) -> String {
    format!("\"{v}\"")
}

/// One already-JSON-encoded `"key":value` member, built from a raw (unescaped) key name and an
/// already-serialized JSON value.
fn kv(key: &str, value_json: String) -> String {
    format!("{}:{value_json}", esc(key))
}

fn obj(fields: Vec<String>) -> String {
    format!("{{{}}}", fields.join(","))
}

fn arr(items: Vec<String>) -> String {
    format!("[{}]", items.join(","))
}

fn unit_name(v: i32) -> &'static str {
    pb::Unit::try_from(v).unwrap_or(pb::Unit::Unspecified).as_str_name()
}

fn author_kind_name(v: i32) -> &'static str {
    pb::AuthorKind::try_from(v).unwrap_or(pb::AuthorKind::Unspecified).as_str_name()
}

fn score_result_json(s: &pb::ScoreResult) -> String {
    let mut f = Vec::new();
    if !s.name.is_empty() {
        f.push(kv("name", esc(&s.name)));
    }
    if s.value != 0.0 {
        f.push(kv("value", num(s.value)));
    }
    if s.unit != 0 {
        f.push(kv("unit", esc(unit_name(s.unit))));
    }
    if let Some(p) = s.passed {
        f.push(kv("passed", p.to_string()));
    }
    obj(f)
}

fn provenance_json(p: &pb::Provenance) -> String {
    let mut f = Vec::new();
    if p.author_kind != 0 {
        f.push(kv("authorKind", esc(author_kind_name(p.author_kind))));
    }
    if !p.principal.is_empty() {
        f.push(kv("principal", esc(&p.principal)));
    }
    if !p.tool.is_empty() {
        f.push(kv("tool", esc(&p.tool)));
    }
    if !p.config_hash.is_empty() {
        f.push(kv("configHash", esc(&p.config_hash)));
    }
    if !p.data_pack_hash.is_empty() {
        f.push(kv("dataPackHash", esc(&p.data_pack_hash)));
    }
    if !p.dataset_hash.is_empty() {
        f.push(kv("datasetHash", esc(&p.dataset_hash)));
    }
    if p.created_tai_ns != 0 {
        f.push(kv("createdTaiNs", i64s(p.created_tai_ns)));
    }
    if !p.run_id.is_empty() {
        f.push(kv("runId", esc(&p.run_id)));
    }
    if !p.attributes.is_empty() {
        let entries = p.attributes.iter().map(|(k, v)| kv(k, esc(v))).collect();
        f.push(kv("attributes", obj(entries)));
    }
    obj(f)
}

/// One `SweepSample` as proto3 canonical JSON text -- `pub` (not just used internally by
/// [`sweep_results_to_json`]) so `src/store.rs`'s `FileStudyStore` can reuse this exact encoder
/// for `samples.jsonl` (one such object per line) rather than writing a second one.
pub fn sweep_sample_json(s: &pb::SweepSample) -> String {
    let mut f = Vec::new();
    if s.point_index != 0 {
        f.push(kv("pointIndex", s.point_index.to_string()));
    }
    if s.draw_index != 0 {
        f.push(kv("drawIndex", s.draw_index.to_string()));
    }
    if !s.axis_values.is_empty() {
        let entries = s.axis_values.iter().map(|(k, v)| kv(k, num(*v))).collect();
        f.push(kv("axisValues", obj(entries)));
    }
    if !s.run_id.is_empty() {
        f.push(kv("runId", esc(&s.run_id)));
    }
    if !s.config_hash.is_empty() {
        f.push(kv("configHash", esc(&s.config_hash)));
    }
    if !s.scores.is_empty() {
        let entries = s.scores.iter().map(|(k, v)| kv(k, score_result_json(v))).collect();
        f.push(kv("scores", obj(entries)));
    }
    if !s.products_uri.is_empty() {
        f.push(kv("productsUri", esc(&s.products_uri)));
    }
    if !s.error.is_empty() {
        f.push(kv("error", esc(&s.error)));
    }
    // Field 10, declared after error's field 9 -- emitted last, in that same relative order; see
    // the module doc comment's "seeds" bullet for the full field-order disclosure.
    if !s.seeds.is_empty() {
        let entries = s.seeds.iter().map(|(k, v)| kv(k, u64s(*v))).collect();
        f.push(kv("seeds", obj(entries)));
    }
    obj(f)
}

fn score_aggregate_json(a: &pb::ScoreAggregate) -> String {
    let mut f = Vec::new();
    if !a.name.is_empty() {
        f.push(kv("name", esc(&a.name)));
    }
    if a.point_index != 0 {
        f.push(kv("pointIndex", a.point_index.to_string()));
    }
    if a.draws != 0 {
        f.push(kv("draws", a.draws.to_string()));
    }
    if a.mean != 0.0 {
        f.push(kv("mean", num(a.mean)));
    }
    if a.std_dev != 0.0 {
        f.push(kv("stdDev", num(a.std_dev)));
    }
    if a.min != 0.0 {
        f.push(kv("min", num(a.min)));
    }
    if a.max != 0.0 {
        f.push(kv("max", num(a.max)));
    }
    if let Some(p) = a.pass_fraction {
        f.push(kv("passFraction", num(p)));
    }
    obj(f)
}

/// The whole `SweepResults` message as proto3 canonical JSON text -- see the module doc comment
/// for the exact rules. F2 (this crate's own `src/aggregate.rs`) now fills `aggregates`; an empty
/// `repeated` field is still omitted here exactly like every other default-valued field, so a
/// study with no aggregate rows (e.g. every sample failed) never carries a stray
/// `"aggregates":[]`.
pub fn sweep_results_to_json(r: &pb::SweepResults) -> String {
    let mut f = Vec::new();
    if !r.sweep_id.is_empty() {
        f.push(kv("sweepId", esc(&r.sweep_id)));
    }
    if !r.sweep_hash.is_empty() {
        f.push(kv("sweepHash", esc(&r.sweep_hash)));
    }
    if !r.drm_hash.is_empty() {
        f.push(kv("drmHash", esc(&r.drm_hash)));
    }
    if !r.samples.is_empty() {
        f.push(kv("samples", arr(r.samples.iter().map(sweep_sample_json).collect())));
    }
    if !r.aggregates.is_empty() {
        f.push(kv("aggregates", arr(r.aggregates.iter().map(score_aggregate_json).collect())));
    }
    if let Some(p) = &r.provenance {
        f.push(kv("provenance", provenance_json(p)));
    }
    obj(f)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn json_encodes_uint64_map_values_as_strings_and_omits_unset_optionals() {
        let sample = pb::SweepSample {
            point_index: 0,
            draw_index: 0,
            seeds: BTreeMap::from([("burn_seed".to_string(), 42u64)]),
            scores: BTreeMap::from([("s1".to_string(), pb::ScoreResult { name: "s1".to_string(), value: 1.5, unit: pb::Unit::Meter as i32, passed: None })]),
            ..Default::default()
        };
        let json = sweep_sample_json(&sample);
        assert!(json.contains("\"seeds\":{\"burn_seed\":\"42\"}"), "a seeds map value must encode as a quoted string: {json}");
        assert!(!json.contains("\"burn_seed\":42"), "must never encode as a bare JSON number: {json}");
        assert!(!json.contains("passed"), "an unset optional bool must be omitted entirely: {json}");

        // An empty seeds map is omitted entirely, like every other empty map field.
        let no_seeds = pb::SweepSample { point_index: 0, draw_index: 0, ..Default::default() };
        assert!(!sweep_sample_json(&no_seeds).contains("seeds"), "an empty seeds map must be omitted, not emitted as {{}}");

        let aggregate = pb::ScoreAggregate { name: "a1".to_string(), pass_fraction: None, ..Default::default() };
        let ajson = score_aggregate_json(&aggregate);
        assert!(!ajson.contains("passFraction"), "an unset optional double must be omitted entirely: {ajson}");

        // Sanity: a SET optional bool/double IS present, with its real value.
        let passed_result = pb::ScoreResult { name: "s2".to_string(), passed: Some(false), ..Default::default() };
        let pjson = score_result_json(&passed_result);
        assert!(pjson.contains("\"passed\":false"), "{pjson}");
        let with_fraction = pb::ScoreAggregate { name: "a2".to_string(), pass_fraction: Some(0.0), ..Default::default() };
        let fjson = score_aggregate_json(&with_fraction);
        assert!(fjson.contains("\"passFraction\":0"), "Some(0.0) must still be emitted, unlike None: {fjson}");
    }

    #[test]
    fn json_encodes_non_finite_doubles_per_proto3() {
        assert_eq!(num(f64::NAN), "\"NaN\"");
        assert_eq!(num(f64::INFINITY), "\"Infinity\"");
        assert_eq!(num(f64::NEG_INFINITY), "\"-Infinity\"");
        assert_eq!(num(5.0), "5.0");

        let nan_score = pb::ScoreResult { name: "n".to_string(), value: f64::NAN, ..Default::default() };
        assert!(score_result_json(&nan_score).contains("\"value\":\"NaN\""));
        let inf_score = pb::ScoreResult { name: "i".to_string(), value: f64::INFINITY, ..Default::default() };
        assert!(score_result_json(&inf_score).contains("\"value\":\"Infinity\""));
        let neg_inf_score = pb::ScoreResult { name: "j".to_string(), value: f64::NEG_INFINITY, ..Default::default() };
        assert!(score_result_json(&neg_inf_score).contains("\"value\":\"-Infinity\""));
    }

    /// A small, fully hand-traceable `SweepResults` pinned against its exact expected JSON text
    /// -- a regression pin on this module's own byte-for-byte output (field order, omission,
    /// renaming), NOT the independent oracle (that is
    /// `tests/test_sweep_results_json.py`'s round trip through Python's real protobuf library).
    #[test]
    fn sweep_results_json_matches_a_hand_pinned_expectation() {
        let mut scores = BTreeMap::new();
        scores.insert("demo_flt_rmag_at_end".to_string(), pb::ScoreResult { name: "demo_flt_rmag_at_end".to_string(), value: 6870517.5, unit: pb::Unit::Meter as i32, passed: None });
        // Two keys, deliberately inserted zeta-before-alpha, so this test also exercises map
        // ordering (question 192(b)'s own brief: "use a map with TWO keys, so map ordering is
        // actually exercised") -- BTreeMap<String, u64> already iterates key-sorted (the
        // generated `pb::SweepSample.seeds` field type, confirmed against
        // target/debug/build/av-cdm-*/out/altavista.v1.rs's own `#[prost(btree_map = ...)]`), so
        // the expected JSON below has "aaa_seed" before "zeta_seed" regardless of insertion order.
        let seeds = BTreeMap::from([("zeta_seed".to_string(), 99999u64), ("aaa_seed".to_string(), 12345u64)]);
        let sample = pb::SweepSample {
            point_index: 1,
            draw_index: 0,
            axis_values: BTreeMap::from([("demo_flt.spacecraft.DragArea".to_string(), 25.0)]),
            run_id: "sweep1_p1_d0".to_string(),
            config_hash: "abc123".to_string(),
            scores,
            products_uri: "/tmp/out/sample_p1_d0".to_string(),
            error: String::new(),
            seeds,
        };
        let results = pb::SweepResults {
            sweep_id: "sweep1".to_string(),
            sweep_hash: "deadbeef".to_string(),
            drm_hash: "beefdead".to_string(),
            samples: vec![sample],
            aggregates: vec![],
            provenance: Some(pb::Provenance {
                author_kind: pb::AuthorKind::Agent as i32,
                tool: "av-sweep".to_string(),
                run_id: "sweep1".to_string(),
                config_hash: "deadbeef".to_string(),
                created_tai_ns: 0,
                ..Default::default()
            }),
        };
        let json = sweep_results_to_json(&results);
        // seeds (field 10) emitted last, after error (field 9) -- see the module doc comment's
        // "seeds" bullet for the field-order disclosure this pins.
        let expected = "{\"sweepId\":\"sweep1\",\"sweepHash\":\"deadbeef\",\"drmHash\":\"beefdead\",\"samples\":[{\"pointIndex\":1,\"axisValues\":{\"demo_flt.spacecraft.DragArea\":25.0},\"runId\":\"sweep1_p1_d0\",\"configHash\":\"abc123\",\"scores\":{\"demo_flt_rmag_at_end\":{\"name\":\"demo_flt_rmag_at_end\",\"value\":6870517.5,\"unit\":\"UNIT_METER\"}},\"productsUri\":\"/tmp/out/sample_p1_d0\",\"seeds\":{\"aaa_seed\":\"12345\",\"zeta_seed\":\"99999\"}}],\"provenance\":{\"authorKind\":\"AUTHOR_KIND_AGENT\",\"tool\":\"av-sweep\",\"configHash\":\"deadbeef\",\"runId\":\"sweep1\"}}";
        assert_eq!(json, expected);
    }
}
