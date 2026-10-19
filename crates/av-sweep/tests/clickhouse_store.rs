//! The ClickHouse study store against a **real** ClickHouse server (question 245): the image
//! `services/clickhouse/IMAGE_DIGEST.md` records, run under the host-wide docker lock with the
//! gate / label / prune discipline of `crates/av-store/tests/minio_store.rs` (see `tests/common`).
//! Nothing here mocks the server; the fake-server counterparts (the wire format the *client*
//! produces, and faults a healthy server never shows) are `tests/clickhouse_fake_server.rs`.
//!
//! Four tests, four containers (a container costs about five seconds to start):
//!
//! 1. [`the_store_round_trips_exactly_and_answers_the_moe_query_in_sql`]: migration idempotence,
//!    exact-float and every-field round trips, ordering, special ids, immutability, orphan
//!    invisibility, the winner of a lost race, tamper detection, the MoE query against
//!    `av_sweep::aggregate` (with the measurement of ClickHouse's native `avg`/`sum`), live schema
//!    mismatches, and real server exception codes.
//! 2. [`a_read_only_user_can_read_but_every_write_is_a_typed_refusal`]: real read-only users.
//! 3. [`tls_to_the_real_server_verifies_the_certificate_and_the_host`]: `https://` with a test CA.
//! 4. [`the_plan_check_and_the_two_phase_write_hold_against_the_real_server`]: the up-front plan
//!    check, `prepare`/`abort`/`commit` invisibility, and `write_study` with the file store.
//!
//! The real-sweep equality proof (a GMAT study written through both stores in one run) is
//! `tests/clickhouse_real_sweep.rs`.

#[macro_use]
mod common;

use std::collections::BTreeMap;
use std::fs;

use av_cdm::pb;
use av_lockstep::docker::{lock_docker_tests, prune_stale_test_resources};
use av_sweep::clickhouse::{ChClient, ClickHouseConfig, ClickHouseStudyStore, SchemaState};
use av_sweep::json;
use av_sweep::store::{StoreError, StudyStore, WriteRefusal};
use av_sweep::{aggregate, sha256_hex};
use prost::Message;

use common::{ChServer, CopyIn, StartOpts, Stage};

// ---------------------------------------------------------------------------------------------
// Studies.
// ---------------------------------------------------------------------------------------------

fn nan_payload() -> f64 {
    f64::from_bits(0x7ff8_dead_beef_0001)
}

/// Every field of every message non-default somewhere, the hard floats (`-0.0`, subnormals,
/// `+-inf`, NaN with a payload), samples and aggregates deliberately NOT in sorted order, empty
/// maps on some samples, a failed sample, an all-default sample, an unknown enum value.
fn hard_float_study() -> pb::SweepResults {
    let full = pb::SweepSample {
        point_index: 5,
        draw_index: 1,
        axis_values: BTreeMap::from([
            ("inst.negzero".to_string(), -0.0),
            ("inst.subnormal".to_string(), f64::from_bits(1)),
            ("event:e.v".to_string(), f64::INFINITY),
            ("z".to_string(), f64::NEG_INFINITY),
            ("nan".to_string(), nan_payload()),
            (String::new(), 1.5),
        ]),
        run_id: "run-p5-d1".to_string(),
        config_hash: "cfg-p5-d1".to_string(),
        scores: BTreeMap::from([
            ("m1".to_string(), pb::ScoreResult { name: "display of m1".to_string(), value: nan_payload(), unit: pb::Unit::Meter as i32, passed: Some(true) }),
            ("m2".to_string(), pb::ScoreResult { name: String::new(), value: -0.0, unit: 12345, passed: Some(false) }),
            ("m3".to_string(), pb::ScoreResult { name: "m3".to_string(), value: f64::from_bits(0x000f_ffff_ffff_ffff), unit: 0, passed: None }),
            ("m4".to_string(), pb::ScoreResult { name: "other name than the key".to_string(), value: f64::MAX, unit: i32::MIN, passed: None }),
        ]),
        products_uri: "/tmp/out/sample_p5_d1".to_string(),
        error: String::new(),
        seeds: BTreeMap::from([("a".to_string(), u64::MAX), ("b".to_string(), 0), ("zeta".to_string(), 1 << 63)]),
    };
    let failed = pb::SweepSample {
        point_index: 0,
        draw_index: 0,
        error: "child exited 101: panicked at 'x', src/y.rs:1\n  with \"quotes\", a tab\t, ünï \u{1F680} and a NUL \u{0} byte".to_string(),
        ..Default::default()
    };
    let all_default = pb::SweepSample::default();
    let only_seeds = pb::SweepSample { point_index: 2, draw_index: 3, seeds: BTreeMap::from([("s".to_string(), 42)]), ..Default::default() };
    pb::SweepResults {
        sweep_id: "hard-floats".to_string(),
        sweep_hash: "sweep-hash".to_string(),
        drm_hash: "drm-hash".to_string(),
        // Deliberately unsorted: (5,1), (0,0 failed), (5,0 default... point 0 draw 0 default), (2,3).
        samples: vec![full, failed, all_default, only_seeds],
        aggregates: vec![
            pb::ScoreAggregate { name: "m1".to_string(), point_index: 5, draws: 2, mean: f64::NAN, std_dev: -0.0, min: f64::NEG_INFINITY, max: f64::INFINITY, pass_fraction: Some(0.0) },
            pb::ScoreAggregate { name: "m3".to_string(), point_index: 0, draws: 0, mean: 0.0, std_dev: 0.0, min: 0.0, max: 0.0, pass_fraction: None },
            pb::ScoreAggregate { name: String::new(), point_index: 0, draws: u32::MAX, mean: f64::from_bits(1), std_dev: nan_payload(), min: -0.0, max: f64::MIN, pass_fraction: Some(-0.0) },
        ],
        provenance: Some(pb::Provenance {
            author_kind: pb::AuthorKind::External as i32,
            principal: "principal".to_string(),
            tool: "tool 1.2".to_string(),
            config_hash: "c".to_string(),
            data_pack_hash: "d".to_string(),
            dataset_hash: "s".to_string(),
            created_tai_ns: i64::MIN,
            run_id: "r".to_string(),
            attributes: BTreeMap::from([("k1".to_string(), "v1".to_string()), (String::new(), String::new()), ("k0".to_string(), "uni \u{1F680}".to_string())]),
        }),
    }
}

fn plain_study(sweep_id: &str, marker: f64) -> pb::SweepResults {
    pb::SweepResults {
        sweep_id: sweep_id.to_string(),
        sweep_hash: format!("{sweep_id}-hash"),
        drm_hash: "drm".to_string(),
        samples: (0..3u32)
            .map(|d| pb::SweepSample {
                point_index: 0,
                draw_index: d,
                run_id: format!("{sweep_id}_p0_d{d}"),
                scores: BTreeMap::from([("m".to_string(), pb::ScoreResult { name: "m".to_string(), value: marker + f64::from(d), unit: 0, passed: None })]),
                seeds: BTreeMap::from([("k".to_string(), u64::from(d) + 1)]),
                ..Default::default()
            })
            .collect(),
        aggregates: vec![pb::ScoreAggregate { name: "m".to_string(), point_index: 0, draws: 3, mean: marker + 1.0, std_dev: 0.5, min: marker, max: marker + 2.0, pass_fraction: None }],
        provenance: None,
    }
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn unit(&mut self) -> f64 {
        self.next() as f64 / (1u64 << 53) as f64
    }

    /// A value whose magnitude is anywhere from 1e-8 to 1e16 and whose sign varies: sums of these
    /// depend on the order they are added in.
    fn awkward(&mut self) -> f64 {
        let scale = 10f64.powi((self.next() % 25) as i32 - 8);
        (self.unit() - 0.5) * scale
    }
}

fn score(key: &str, value: f64, passed: Option<bool>) -> (String, pb::ScoreResult) {
    (key.to_string(), pb::ScoreResult { name: format!("display {key}"), value, unit: pb::Unit::Meter as i32, passed })
}

fn sample_with(point: u32, draw: u32, scores: Vec<(String, pb::ScoreResult)>) -> pb::SweepSample {
    pb::SweepSample { point_index: point, draw_index: draw, run_id: format!("moe_p{point}_d{draw}"), scores: scores.into_iter().collect(), ..Default::default() }
}

/// 60 points x 24 draws of awkward values on `alpha`, an objective `beta` (pass/fail), `gamma`
/// on even points only, some failed samples that still carry adversarial scores, and special
/// points for every hard float. Samples are shuffled; `aggregate()` is order-sensitive in the
/// list order, which is the ordinal order the SQL folds in.
fn moe_study(shuffle: bool) -> pb::SweepResults {
    let mut rng = Lcg(0x5eed_1234_abcd);
    let mut samples = Vec::new();
    for p in 0..60u32 {
        for d in 0..24u32 {
            if p % 7 == 3 && d % 5 == 2 {
                let mut failed = pb::SweepSample { point_index: p, draw_index: d, error: "deliberately failed".to_string(), ..Default::default() };
                failed.scores.extend([score("alpha", 1e300, None)]);
                samples.push(failed);
                continue;
            }
            let mut scores = vec![score("alpha", rng.awkward(), None), score("beta", rng.awkward(), Some(rng.unit() > 0.4))];
            if p % 2 == 0 {
                scores.push(score("gamma", rng.awkward(), None));
            }
            samples.push(sample_with(p, d, scores));
        }
    }
    let specials: Vec<(u32, Vec<f64>)> = vec![
        (100, vec![f64::NAN, f64::NAN, f64::NAN]),
        (101, vec![f64::NAN, 1.0, 2.0]),
        (102, vec![f64::INFINITY, f64::NEG_INFINITY, 1.0]),
        (103, vec![-0.0, -0.0]),
        (104, vec![0.0, -0.0, 0.0]),
        (105, vec![-0.0, 0.0]),
        (106, vec![f64::from_bits(1), f64::from_bits(2), f64::MIN_POSITIVE]),
        (107, vec![f64::MAX, f64::MAX]),
        (108, vec![42.0]),
        (109, vec![0.1, 0.2, 0.3]),
        (110, vec![1e16, 1.0, -1e16, 1.0]),
        (111, vec![1.0, 1e16, 1.0, -1e16]),
    ];
    for (p, values) in specials {
        for (d, v) in values.into_iter().enumerate() {
            samples.push(sample_with(p, d as u32, vec![score("alpha", v, None)]));
        }
    }
    // A point whose every draw failed: no aggregate row at all.
    for d in 0..3 {
        samples.push(pb::SweepSample { point_index: 200, draw_index: d, error: "all failed".to_string(), ..Default::default() });
    }
    if shuffle {
        let mut keyed: Vec<(u64, pb::SweepSample)> = samples.into_iter().map(|s| (rng.next(), s)).collect();
        keyed.sort_by_key(|(k, _)| *k);
        samples = keyed.into_iter().map(|(_, s)| s).collect();
    }
    pb::SweepResults { sweep_id: if shuffle { "moe-shuffled".to_string() } else { "moe-sorted".to_string() }, sweep_hash: "h".to_string(), drm_hash: "d".to_string(), samples, aggregates: Vec::new(), provenance: None }
}

fn f_same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// Bit-for-bit comparison of every f64 (NaN payloads included) plus the canonical encoding and the
/// canonical JSON: the strictest "the same study" this crate can state.
fn assert_identical(got: &pb::SweepResults, want: &pb::SweepResults, what: &str) {
    assert_eq!(got.sweep_id, want.sweep_id, "{what}: sweep_id");
    assert_eq!(got.samples.len(), want.samples.len(), "{what}: sample count");
    for (i, (g, w)) in got.samples.iter().zip(&want.samples).enumerate() {
        let pb::SweepSample { point_index, draw_index, axis_values, run_id, config_hash, scores, products_uri, error, seeds } = g;
        assert_eq!((*point_index, *draw_index), (w.point_index, w.draw_index), "{what}: sample {i} point/draw");
        assert_eq!(axis_values.len(), w.axis_values.len(), "{what}: sample {i} axis_values len");
        for (k, v) in axis_values {
            assert_eq!(v.to_bits(), w.axis_values[k].to_bits(), "{what}: sample {i} axis {k:?}: {v:?} vs {:?}", w.axis_values[k]);
        }
        assert_eq!((run_id, config_hash, products_uri, error), (&w.run_id, &w.config_hash, &w.products_uri, &w.error), "{what}: sample {i} strings");
        assert_eq!(seeds, &w.seeds, "{what}: sample {i} seeds");
        assert_eq!(scores.len(), w.scores.len(), "{what}: sample {i} scores len");
        for (k, s) in scores {
            let ws = &w.scores[k];
            assert_eq!(s.name, ws.name, "{what}: sample {i} score {k:?} name (distinct from its key)");
            assert_eq!(s.value.to_bits(), ws.value.to_bits(), "{what}: sample {i} score {k:?} value {:?} vs {:?}", s.value, ws.value);
            assert_eq!((s.unit, s.passed), (ws.unit, ws.passed), "{what}: sample {i} score {k:?} unit/passed");
        }
    }
    assert_eq!(got.aggregates.len(), want.aggregates.len(), "{what}: aggregate count");
    for (i, (g, w)) in got.aggregates.iter().zip(&want.aggregates).enumerate() {
        assert_eq!((&g.name, g.point_index, g.draws), (&w.name, w.point_index, w.draws), "{what}: aggregate {i} key/draws");
        for (field, a, b) in [("mean", g.mean, w.mean), ("std_dev", g.std_dev, w.std_dev), ("min", g.min, w.min), ("max", g.max, w.max)] {
            assert_eq!(a.to_bits(), b.to_bits(), "{what}: aggregate {i} {field}: {a:?} vs {b:?}");
        }
        assert_eq!(g.pass_fraction.map(f64::to_bits), w.pass_fraction.map(f64::to_bits), "{what}: aggregate {i} pass_fraction");
    }
    assert_eq!(got.provenance.as_ref().map(|p| (p.attributes.clone(), p.created_tai_ns)), want.provenance.as_ref().map(|p| (p.attributes.clone(), p.created_tai_ns)), "{what}: provenance attributes/created");
    assert_eq!(got.encode_to_vec(), want.encode_to_vec(), "{what}: canonical protobuf encoding");
    assert_eq!(json::sweep_results_to_json(got), json::sweep_results_to_json(want), "{what}: canonical JSON, byte for byte");
}

fn step(name: &str) {
    println!("[step] {name}");
}

fn count(admin: &ChClient, sql: &str) -> u64 {
    let bytes = admin.query(&format!("{sql} FORMAT RowBinary"), &[], "count").expect("count query");
    assert_eq!(bytes.len(), 8, "{sql}");
    u64::from_le_bytes(bytes.try_into().unwrap())
}

fn show_create(admin: &ClickHouseStudyStore) -> BTreeMap<String, String> {
    av_sweep::clickhouse::schema::TABLES
        .iter()
        .map(|t| {
            let bytes = admin.client().query(&format!("SHOW CREATE TABLE `{}` FORMAT TSVRaw", t.name), &[], "show create").expect("SHOW CREATE TABLE");
            (t.name.to_string(), String::from_utf8(bytes).unwrap())
        })
        .collect()
}

fn expect_server(err: StoreError, want_code: i64) -> av_sweep::clickhouse::ServerException {
    match err {
        StoreError::Server { exception, .. } => {
            assert_eq!(exception.code, Some(want_code), "{exception:?}");
            exception
        }
        other => panic!("expected StoreError::Server with code {want_code}, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// 1. The main store suite.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_store_round_trips_exactly_and_answers_the_moe_query_in_sql() {
    const NAME: &str = "the_store_round_trips_exactly_and_answers_the_moe_query_in_sql";
    gate_or_skip!(NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let server = ChServer::start(NAME, StartOpts::default());
    let config = server.config();

    step("migration: an empty database is migrated, and migrating again leaves SHOW CREATE TABLE unchanged");
    let mut store = ClickHouseStudyStore::connect(&config).expect("connect + migrate an empty database");
    assert_eq!(store.verify_schema().unwrap(), SchemaState::Ready);
    let first = show_create(&store);
    assert_eq!(first.len(), av_sweep::clickhouse::schema::TABLES.len());
    store.migrate().expect("second migrate");
    store.migrate().expect("third migrate");
    let second = show_create(&store);
    assert_eq!(first, second, "migrate twice must leave SHOW CREATE TABLE byte-identical");
    let reopened = ClickHouseStudyStore::connect(&config).expect("reopen: verify only, no migration needed");
    assert_eq!(show_create(&reopened), first);
    let versions = count(store.client(), "SELECT count() FROM `av_sweep_schema_version`");
    assert_eq!(versions, 1, "exactly one version row, however often the schema was migrated or opened");
    println!("  SHOW CREATE TABLE of {} tables identical after 2 further migrations; 1 schema-version row", first.len());

    step("exact round trip: every field non-default, the hard floats, unsorted samples/aggregates, empty maps, a failed sample");
    let study = hard_float_study();
    assert!(study.samples[0].scores["m1"].value.is_nan(), "sanity: the fixture really carries a NaN");
    let location = store.put_study(&study).expect("put hard_float_study");
    println!("  put -> {location}");
    let got = store.get_study("hard-floats").expect("get").expect("present");
    assert_identical(&got, &study, "hard floats");
    println!("  canonical JSON sha256: {}", sha256_hex(json::sweep_results_to_json(&got).as_bytes()));

    step("an empty study, and None provenance vs all-default provenance");
    let mut empty = pb::SweepResults { sweep_id: "empty".to_string(), ..Default::default() };
    store.put_study(&empty).unwrap();
    assert_identical(&store.get_study("empty").unwrap().unwrap(), &empty, "empty study");
    empty.sweep_id = "empty-default-provenance".to_string();
    empty.provenance = Some(pb::Provenance::default());
    store.put_study(&empty).unwrap();
    let got = store.get_study("empty-default-provenance").unwrap().unwrap();
    assert_eq!(got.provenance, Some(pb::Provenance::default()), "Some(default) must not collapse to None");
    assert_identical(&got, &empty, "default provenance");
    assert_eq!(store.get_study("never-written").unwrap(), None, "a missing study is Ok(None)");

    step("list_studies: sorted by sweep_id, one entry per id, sample counts from the header");
    let listed = store.list_studies().unwrap();
    let ids: Vec<&str> = listed.iter().map(|s| s.sweep_id.as_str()).collect();
    assert_eq!(ids, vec!["empty", "empty-default-provenance", "hard-floats"]);
    assert_eq!(listed[2].sample_count, 4);
    assert_eq!(listed[2].sweep_hash, "sweep-hash");

    step("special sweep ids are bound parameters: quote, backslash, tab, newline, unicode, empty, SQL-looking");
    for (i, id) in ["x' OR 1=1 --", "back\\slash", "tab\there", "new\nline", "ünï\u{1F680}", "", "a b/c?d=e&f", "'; DROP TABLE av_sweep_studies; --"].iter().enumerate() {
        let mut s = plain_study(id, i as f64 * 10.0);
        s.sweep_id = id.to_string();
        store.put_study(&s).unwrap_or_else(|e| panic!("put {id:?}: {e}"));
        let got = store.get_study(id).unwrap().unwrap_or_else(|| panic!("get {id:?}"));
        assert_identical(&got, &s, &format!("id {id:?}"));
    }
    assert!(store.get_study("x' OR 1=1 -- ").unwrap().is_none(), "a near-miss id is not found");
    assert!(store.get_study("hard-floats").unwrap().is_some(), "the tables survived the injection attempts");

    step("immutability: identical content is an idempotent Ok writing nothing; different content is a typed RefusedWrite");
    let rows_before = count(store.client(), "SELECT count() FROM `av_sweep_samples`");
    let again = store.put_study(&study).expect("identical content is idempotent");
    assert!(again.contains("hard-floats"), "{again}");
    assert_eq!(count(store.client(), "SELECT count() FROM `av_sweep_samples`"), rows_before, "an idempotent put inserts no rows");
    assert_eq!(count(store.client(), "SELECT count() FROM `av_sweep_studies` WHERE sweep_id = 'hard-floats'"), 1);
    let mut changed = study.clone();
    changed.samples[0].scores.get_mut("m3").unwrap().value = 1.0;
    let err = store.put_study(&changed).unwrap_err();
    match &err {
        StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::DifferentContent { existing_sha256, new_sha256, .. } } => {
            assert_eq!(sweep_id, "hard-floats");
            assert_ne!(existing_sha256, new_sha256);
        }
        other => panic!("expected RefusedWrite/DifferentContent, got {other:?}"),
    }
    assert!(err.to_string().contains("hard-floats") && err.to_string().contains("immutable"), "{err}");
    assert_eq!(count(store.client(), "SELECT count() FROM `av_sweep_samples`"), rows_before, "a refused put inserts no rows");
    assert_identical(&store.get_study("hard-floats").unwrap().unwrap(), &study, "after the refused overwrite");
    // A one-bit difference in a NaN payload is different content.
    let mut payload = study.clone();
    payload.samples[0].scores.get_mut("m1").unwrap().value = f64::from_bits(0x7ff8_dead_beef_0002);
    assert!(matches!(store.put_study(&payload), Err(StoreError::RefusedWrite { .. })), "NaN payloads count");

    step("orphan invisibility: rows inserted without their header are never read");
    let orphan = plain_study("orphan-only", 1.0);
    let wid = store.write_rows_for_tests(&orphan, false).expect("insert rows, no header");
    assert!(count(store.client(), &format!("SELECT count() FROM `av_sweep_samples` WHERE write_id = '{wid}'")) > 0, "the orphan rows really are in the table");
    assert_eq!(store.get_study("orphan-only").unwrap(), None, "no header, no study");
    assert_eq!(store.aggregates_in_sql("orphan-only").unwrap(), None);
    assert!(!store.list_studies().unwrap().iter().any(|s| s.sweep_id == "orphan-only"));
    // Orphans of a DIFFERENT write under an id that has a committed study must not leak into it.
    let committed = plain_study("orphan-shadow", 100.0);
    store.put_study(&committed).unwrap();
    let crashed = plain_study("orphan-shadow", 5000.0);
    store.write_rows_for_tests(&crashed, false).expect("a second, crashed write of the same id");
    let got = store.get_study("orphan-shadow").unwrap().unwrap();
    assert_identical(&got, &committed, "committed study with an orphan write beside it");
    let moe = store.aggregates_in_sql("orphan-shadow").unwrap().unwrap();
    let want = aggregate(&committed.samples).unwrap();
    assert_eq!(moe.len(), want.len());
    assert!(f_same(moe[0].mean, want[0].mean) && f_same(moe[0].max, want[0].max), "the MoE query reads only the header's write: {moe:?} vs {want:?}");
    // And the committed study can still be re-put (idempotent) while the orphan sits there.
    store.put_study(&committed).unwrap();
    println!("  orphan rows present but invisible; the committed study unchanged");

    step("a writer that lost a race: the earlier header wins for readers, the loser's put is refused");
    let first_study = plain_study("race", 1.0);
    let second_study = plain_study("race", 2.0);
    store.put_study(&first_study).unwrap();
    store.write_rows_for_tests(&second_study, true).expect("a second committed write of the same id (the lost race)");
    assert_eq!(count(store.client(), "SELECT count() FROM `av_sweep_studies` WHERE sweep_id = 'race'"), 2, "two headers now exist");
    assert_identical(&store.get_study("race").unwrap().unwrap(), &first_study, "the earlier header wins");
    assert_eq!(store.list_studies().unwrap().iter().filter(|s| s.sweep_id == "race").count(), 1, "listed once");
    assert!(matches!(store.put_study(&second_study), Err(StoreError::RefusedWrite { reason: WriteRefusal::DifferentContent { .. }, .. })));
    store.put_study(&first_study).expect("the winner's content is still idempotent");

    step("tamper detection: a changed value, a deleted score row and a deleted sample row are Corrupt, never a quietly different study");
    for (label, mutation, want_in_detail) in [
        ("changed value", "ALTER TABLE `av_sweep_sample_scores` UPDATE `value` = `value` + 1 WHERE sweep_id = '{id}' SETTINGS mutations_sync = 1", "hash"),
        ("deleted score row", "ALTER TABLE `av_sweep_sample_scores` DELETE WHERE sweep_id = '{id}' AND `key` = 'm' AND sample_ordinal = 0 SETTINGS mutations_sync = 1", "hash"),
        ("deleted sample row", "ALTER TABLE `av_sweep_samples` DELETE WHERE sweep_id = '{id}' AND ordinal = 2 SETTINGS mutations_sync = 1", "sample"),
    ] {
        let id = format!("tamper-{}", label.replace(' ', "-"));
        store.put_study(&plain_study(&id, 7.0)).unwrap();
        assert!(store.get_study(&id).unwrap().is_some());
        store.client().command(&mutation.replace("{id}", &id), &[], "tampering").unwrap_or_else(|e| panic!("{label}: {e}"));
        match store.get_study(&id) {
            Err(StoreError::Corrupt { detail, .. }) => assert!(detail.contains(want_in_detail), "{label}: {detail}"),
            other => panic!("{label}: expected Corrupt, got {other:?}"),
        }
        println!("  {label}: Corrupt");
    }

    step("MoE query: ClickHouse's order-fixed fold equals av_sweep::aggregate bit for bit (sorted, shuffled, and many-draw studies)");
    for study in [moe_study(false), moe_study(true), moe_many_draws(), moe_huge()] {
        store.put_study(&study).unwrap();
        check_moe(&store, &study);
    }

    step("MoE measurement: ClickHouse's native avg()/sum() against the Rust left-to-right sum, same data");
    for study in [moe_study(true), moe_many_draws(), moe_huge()] {
        measure_native(&store, &study);
    }

    step("a mixed pass criterion is the same typed refusal aggregate() gives");
    let mixed = pb::SweepResults {
        sweep_id: "mixed".to_string(),
        samples: vec![sample_with(0, 0, vec![score("flaky", 1.0, Some(true))]), sample_with(0, 1, vec![score("flaky", 2.0, None)]), sample_with(1, 0, vec![score("aaa", 1.0, None)])],
        ..Default::default()
    };
    store.put_study(&mixed).unwrap();
    let rust_err = aggregate(&mixed.samples).unwrap_err();
    match store.aggregates_in_sql("mixed") {
        Err(StoreError::Aggregation(sql_err)) => assert_eq!(sql_err.to_string(), rust_err.to_string()),
        other => panic!("expected Aggregation(MixedPassCriterion), got {other:?}"),
    }

    step("live schema mismatches against the real server are typed and nothing is migrated over them");
    let admin = store.client();
    admin.command("ALTER TABLE `av_sweep_samples` MODIFY COLUMN `point_index` UInt64", &[], "alter").unwrap();
    let err = ClickHouseStudyStore::connect(&config).unwrap_err();
    match &err {
        StoreError::SchemaMismatch { table, column, expected, found } => {
            assert_eq!((table.as_str(), column.as_str()), ("av_sweep_samples", "point_index"));
            assert!(expected.contains("UInt32") && found.contains("UInt64"), "{expected} / {found}");
            println!("  mismatch: table {table}, column {column}: expected {expected}; found {found}");
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
    admin.command("ALTER TABLE `av_sweep_samples` MODIFY COLUMN `point_index` UInt32", &[], "restore").unwrap();
    admin.command("ALTER TABLE `av_sweep_aggregates` ADD COLUMN `surprise` String", &[], "alter").unwrap();
    assert!(matches!(ClickHouseStudyStore::connect(&config), Err(StoreError::SchemaMismatch { column, .. }) if column == "surprise"));
    admin.command("ALTER TABLE `av_sweep_aggregates` DROP COLUMN `surprise`", &[], "restore").unwrap();
    admin.command("ALTER TABLE `av_sweep_sample_seeds` RENAME COLUMN `seed` TO `seed_value`", &[], "alter").unwrap();
    assert!(matches!(ClickHouseStudyStore::connect(&config), Err(StoreError::SchemaMismatch { column, found, .. }) if column == "seed" && found.contains("seed_value")));
    admin.command("ALTER TABLE `av_sweep_sample_seeds` RENAME COLUMN `seed_value` TO `seed`", &[], "restore").unwrap();
    ClickHouseStudyStore::connect(&config).expect("restored schema verifies again");
    // A missing table is migrated back (the store never migrates over a DIFFERENT table, only into a missing one).
    admin.command("DROP TABLE `av_sweep_provenance_attributes`", &[], "drop").unwrap();
    ClickHouseStudyStore::connect(&config).expect("a missing table is recreated by the migration");
    assert_eq!(count(admin, "SELECT count() FROM system.tables WHERE database = currentDatabase() AND name = 'av_sweep_provenance_attributes'"), 1);

    step("real server exception codes: wrong password, unknown database, unknown table, syntax error");
    let wrong_var = format!("{}_WRONG", server.password_var);
    std::env::set_var(&wrong_var, "definitely-not-the-password");
    let mut bad_password = config.clone();
    bad_password.password_env = Some(wrong_var.clone());
    let e = expect_server(ClickHouseStudyStore::connect(&bad_password).unwrap_err(), 516);
    assert_eq!(e.name.as_deref(), Some("AUTHENTICATION_FAILED"));
    assert!(!format!("{e:?}").contains("definitely-not-the-password"));
    std::env::remove_var(&wrong_var);
    let mut bad_db = config.clone();
    bad_db.database = "no_such_database".to_string();
    let e = expect_server(ClickHouseStudyStore::connect(&bad_db).unwrap_err(), 81);
    assert_eq!(e.name.as_deref(), Some("UNKNOWN_DATABASE"));
    let e = expect_server(admin.query("SELECT * FROM `no_such_table` FORMAT RowBinary", &[], "probe").unwrap_err(), 60);
    assert_eq!(e.name.as_deref(), Some("UNKNOWN_TABLE"));
    let e = expect_server(admin.query("SELEC 1", &[], "probe").unwrap_err(), 62);
    assert_eq!(e.name.as_deref(), Some("SYNTAX_ERROR"));
    println!("  codes seen: 516 AUTHENTICATION_FAILED, 81 UNKNOWN_DATABASE, 60 UNKNOWN_TABLE, 62 SYNTAX_ERROR");
}

// ---------------------------------------------------------------------------------------------
// 2. Real read-only users.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_read_only_user_can_read_but_every_write_is_a_typed_refusal() {
    const NAME: &str = "a_read_only_user_can_read_but_every_write_is_a_typed_refusal";
    gate_or_skip!(NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let server = ChServer::start(NAME, StartOpts { access_management: true, ..Default::default() });
    let writer = ClickHouseStudyStore::connect(&server.config()).expect("the image-created user opens and migrates the store");

    let study = hard_float_study();
    let mut writer = writer;
    writer.put_study(&study).expect("seed one study as the writer");
    let moe = moe_study(false);
    writer.put_study(&moe).unwrap();

    // Two flavours of read-only: privileges (GRANT SELECT only) and the readonly=1 profile setting.
    let admin = server.admin();
    let db = &server.database;
    let mk = |user: &str, settings: &str| {
        let var = format!("AV_CH_TEST_RO_{}", user.to_ascii_uppercase());
        let pw = format!("pw-{user}-x");
        std::env::set_var(&var, &pw);
        admin.command(&format!("CREATE USER {user} IDENTIFIED BY '{pw}' {settings}"), &[], "create user").unwrap_or_else(|e| panic!("CREATE USER {user}: {e}"));
        admin.command(&format!("GRANT SELECT ON {db}.* TO {user}"), &[], "grant").unwrap_or_else(|e| panic!("GRANT to {user}: {e}"));
        let mut config = ClickHouseConfig::new(server.http_url(), db.clone(), user);
        config.password_env = Some(var);
        config.timeouts = av_http1::Timeouts::uniform(std::time::Duration::from_secs(20));
        config
    };
    let ro_grant = mk("ro_grant", "");
    let ro_flag = mk("ro_flag", "SETTINGS readonly = 1");
    // readonly = 1 WITH insert privileges: only the profile setting stands between this user and a write.
    let ro_flag_insert = mk("ro_flag_ins", "SETTINGS readonly = 1");
    admin.command(&format!("GRANT INSERT ON {db}.* TO ro_flag_ins"), &[], "grant").unwrap();

    for (label, config) in [("GRANT SELECT only", &ro_grant), ("readonly = 1", &ro_flag), ("readonly = 1 with INSERT granted", &ro_flag_insert)] {
        step(&format!("read-only user ({label}): opens an already-migrated store without DDL, reads exactly, answers the MoE query"));
        let mut ro = ClickHouseStudyStore::connect(config).unwrap_or_else(|e| panic!("{label}: open: {e}"));
        assert_identical(&ro.get_study("hard-floats").unwrap().unwrap(), &study, label);
        assert_eq!(ro.list_studies().unwrap().len(), 2, "{label}");
        let sql = ro.aggregates_in_sql("moe-sorted").unwrap().unwrap();
        let want = aggregate(&moe.samples).unwrap();
        assert!(sql.iter().zip(&want).all(|(a, b)| f_same(a.mean, b.mean) && f_same(a.std_dev, b.std_dev) && f_same(a.min, b.min) && f_same(a.max, b.max)), "{label}: MoE");

        step(&format!("read-only user ({label}): a write is a typed RefusedWrite carrying the server's code, and nothing lands"));
        let before = count(&admin, "SELECT count() FROM `av_sweep_samples`");
        let err = ro.put_study(&plain_study("ro-attempt", 1.0)).unwrap_err();
        match &err {
            StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::Server { code, name, http_status, message } } => {
                assert_eq!(sweep_id, "ro-attempt");
                println!("  {label}: HTTP {http_status}, code {code:?} {name:?}: {message}");
                assert!(code.is_some() && name.is_some(), "{err:?}");
                assert_ne!(*http_status, 200);
            }
            other => panic!("{label}: expected RefusedWrite/Server, got {other:?}"),
        }
        assert_eq!(count(&admin, "SELECT count() FROM `av_sweep_samples`"), before, "{label}: no row landed");
        assert_eq!(writer.get_study("ro-attempt").unwrap(), None, "{label}: no study appeared");
        // Re-putting an identical study is idempotent even for a read-only user: it writes nothing.
        ro.put_study(&study).unwrap_or_else(|e| panic!("{label}: an idempotent put needs no write privilege: {e}"));
    }

    step("a read-only user on a database with no tables: the migration is refused, typed, with the server's code");
    admin.command("CREATE DATABASE IF NOT EXISTS avempty", &[], "create db").unwrap();
    admin.command("GRANT SELECT ON avempty.* TO ro_grant", &[], "grant").unwrap();
    let mut on_empty = ro_grant.clone();
    on_empty.database = "avempty".to_string();
    match ClickHouseStudyStore::connect(&on_empty) {
        Err(StoreError::Server { exception, operation, .. }) => {
            println!("  migrate refused while {operation}: code {:?} {:?}", exception.code, exception.name);
            assert!(exception.code.is_some());
        }
        Err(other) => panic!("expected a typed server refusal, got {other:?}"),
        Ok(_) => panic!("a read-only user must not be able to migrate"),
    }
    assert_eq!(count(&admin, "SELECT count() FROM system.tables WHERE database = 'avempty'"), 0, "nothing was created");
}

// ---------------------------------------------------------------------------------------------
// 3. TLS against the real server.
// ---------------------------------------------------------------------------------------------

mod pki {
    use openssl::asn1::{Asn1Integer, Asn1Time};
    use openssl::bn::BigNum;
    use openssl::ec::{EcGroup, EcKey};
    use openssl::hash::MessageDigest;
    use openssl::nid::Nid;
    use openssl::pkey::{PKey, Private};
    use openssl::x509::extension::{AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName, SubjectKeyIdentifier};
    use openssl::x509::{X509, X509NameBuilder};

    pub struct Ca {
        pub cert: X509,
        pub key: PKey<Private>,
    }

    pub struct Leaf {
        pub cert: X509,
        pub key: PKey<Private>,
    }

    fn new_key() -> PKey<Private> {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
    }

    fn builder(cn: &str, key: &PKey<Private>, issuer: Option<&openssl::x509::X509NameRef>, serial: u32) -> openssl::x509::X509Builder {
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", cn).unwrap();
        let subject = name.build();
        let mut b = X509::builder().unwrap();
        b.set_version(2).unwrap();
        b.set_serial_number(&Asn1Integer::from_bn(&BigNum::from_u32(serial).unwrap()).unwrap()).unwrap();
        b.set_subject_name(&subject).unwrap();
        match issuer {
            Some(i) => b.set_issuer_name(i).unwrap(),
            None => b.set_issuer_name(&subject).unwrap(),
        }
        b.set_pubkey(key).unwrap();
        b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
        b.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
        b
    }

    pub fn make_ca(cn: &str, serial: u32) -> Ca {
        let key = new_key();
        let mut b = builder(cn, &key, None, serial);
        b.append_extension(BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
        b.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
        let ski = SubjectKeyIdentifier::new().build(&b.x509v3_context(None, None)).unwrap();
        b.append_extension(ski).unwrap();
        b.sign(&key, MessageDigest::sha256()).unwrap();
        Ca { cert: b.build(), key }
    }

    /// A server certificate valid for exactly the given IP addresses and DNS names.
    pub fn issue_server(ca: &Ca, dns: &[&str], ips: &[&str], serial: u32) -> Leaf {
        let key = new_key();
        let mut b = builder("clickhouse test server", &key, Some(ca.cert.subject_name()), serial);
        b.append_extension(BasicConstraints::new().build().unwrap()).unwrap();
        b.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
        let mut eku = ExtendedKeyUsage::new();
        eku.server_auth();
        b.append_extension(eku.build().unwrap()).unwrap();
        let mut san = SubjectAlternativeName::new();
        for d in dns {
            san.dns(d);
        }
        for i in ips {
            san.ip(i);
        }
        let ext = san.build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
        b.append_extension(ext).unwrap();
        let aki = AuthorityKeyIdentifier::new().keyid(false).build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
        b.append_extension(aki).unwrap();
        b.sign(&ca.key, MessageDigest::sha256()).unwrap();
        Leaf { cert: b.build(), key }
    }
}

const TLS_XML: &str = r#"<clickhouse>
  <https_port>8443</https_port>
  <openSSL>
    <server>
      <certificateFile>/etc/clickhouse-server/tls/server.crt</certificateFile>
      <privateKeyFile>/etc/clickhouse-server/tls/server.key</privateKeyFile>
      <verificationMode>none</verificationMode>
      <loadDefaultCAFile>true</loadDefaultCAFile>
      <cacheSessions>true</cacheSessions>
      <disableProtocols>sslv2,sslv3</disableProtocols>
      <preferServerCiphers>true</preferServerCiphers>
    </server>
  </openSSL>
</clickhouse>
"#;

// ---------------------------------------------------------------------------------------------
// 4. The up-front check and the two-phase write (question 254 item 2) against the real server.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_plan_check_and_the_two_phase_write_hold_against_the_real_server() {
    use av_sweep::plan::StudyPlan;
    use av_sweep::store::FileStudyStore;
    use av_sweep::study_write::{check_plan, write_study, NamedStore, StudyWriteError};

    const NAME: &str = "the_plan_check_and_the_two_phase_write_hold_against_the_real_server";
    gate_or_skip!(NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let server = ChServer::start(NAME, StartOpts::default());
    let config = server.config();
    let mut store = ClickHouseStudyStore::connect(&config).expect("connect + migrate");

    step("check_plan: a missing study and an identical one are accepted, a different one is refused naming the first differing field");
    let held = plain_study("planned", 1.0);
    assert!(store.check_plan(&StudyPlan::exact(&held)).is_ok(), "no study under the id: accepted");
    store.put_study(&held).unwrap();
    assert!(store.check_plan(&StudyPlan::exact(&held)).is_ok(), "the identical study: accepted");
    let mut other = held.clone();
    other.samples[0].run_id = "another-run".to_string();
    let rows = count(store.client(), "SELECT count() FROM `av_sweep_samples`");
    match store.check_plan(&StudyPlan::exact(&other)).unwrap_err() {
        StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::DifferentPlan { difference, existing_sha256, .. } } => {
            assert_eq!(sweep_id, "planned");
            let field = &difference.field;
            assert!(field.starts_with("samples[p") && field.ends_with("].run_id"), "{field}");
            assert_eq!(existing_sha256, av_sweep::clickhouse::content_sha256(&held));
        }
        other => panic!("expected RefusedWrite/DifferentPlan, got {other:?}"),
    }
    assert_eq!(count(store.client(), "SELECT count() FROM `av_sweep_samples`"), rows, "a check writes nothing");

    step("prepare: rows are inserted but no reader sees the study; abort leaves it unseen; prepare + commit publishes it");
    let staged = plain_study("staged", 2.0);
    let prepared = store.prepare(&staged).unwrap();
    assert_eq!(store.get_study("staged").unwrap(), None, "prepared, not committed: invisible");
    assert!(count(store.client(), "SELECT count() FROM `av_sweep_samples` WHERE sweep_id = 'staged'") > 0, "the rows are in the table");
    assert!(!store.list_studies().unwrap().iter().any(|s| s.sweep_id == "staged"));
    store.abort(prepared).unwrap();
    assert_eq!(store.get_study("staged").unwrap(), None);
    let prepared = store.prepare(&staged).unwrap();
    store.commit(prepared).unwrap();
    assert_identical(&store.get_study("staged").unwrap().unwrap(), &staged, "prepare + commit");

    step("write_study: the file store refuses at prepare, so ClickHouse's staged rows stay invisible; a fresh id lands in both");
    let root = std::env::temp_dir().join(format!("av-sweep-real-two-phase-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let stores = |root: &std::path::Path| -> Vec<NamedStore> {
        vec![
            NamedStore { label: "the file store".to_string(), store: Box::new(FileStudyStore::new(root)) },
            NamedStore { label: "the ClickHouse store".to_string(), store: Box::new(ClickHouseStudyStore::connect(&config).unwrap()) },
        ]
    };
    let fresh = plain_study("both", 3.0);
    FileStudyStore::new(&root).put_study(&plain_study("both", 99.0)).unwrap(); // the file store holds a different "both"
    let err = write_study(&mut stores(&root), &fresh).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PrepareFailed { store, .. } if store == "the file store"), "{err:?}");
    assert_eq!(store.get_study("both").unwrap(), None, "ClickHouse shows no study");
    assert!(fs::read_dir(&root).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".incoming-")), "no file stage left");
    let err = check_plan(&stores(&root), &StudyPlan::exact(&fresh)).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PlanRefused { store, .. } if store == "the file store"), "{err:?}");

    let both = plain_study("both-fresh", 4.0);
    let written = write_study(&mut stores(&root), &both).unwrap();
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [true, true]);
    assert_identical(&store.get_study("both-fresh").unwrap().unwrap(), &both, "ClickHouse");
    assert_eq!(FileStudyStore::new(&root).get_study("both-fresh").unwrap().unwrap(), both, "the file store");
    let again = write_study(&mut stores(&root), &both).unwrap();
    assert_eq!(again.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, false], "an identical rerun writes nothing");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn tls_to_the_real_server_verifies_the_certificate_and_the_host() {
    const NAME: &str = "tls_to_the_real_server_verifies_the_certificate_and_the_host";
    gate_or_skip!(NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);

    // The staged PKI: a CA, a server certificate valid for the IP 127.0.0.1 ONLY (no DNS name),
    // and a second, unrelated CA. Staged under $HOME because Colima mounts only $HOME; removed at the end.
    let stage = Stage::new("tls");
    let ca = pki::make_ca("clickhouse test CA", 1);
    let other_ca = pki::make_ca("an unrelated CA", 2);
    let leaf = pki::issue_server(&ca, &[], &["127.0.0.1"], 3);
    let tls_dir = stage.dir.join("tls");
    fs::create_dir_all(&tls_dir).unwrap();
    fs::write(tls_dir.join("server.crt"), leaf.cert.to_pem().unwrap()).unwrap();
    // The container's `clickhouse` user (uid 101) must read the key; docker cp keeps the host mode.
    fs::write(tls_dir.join("server.key"), leaf.key.private_key_to_pem_pkcs8().unwrap()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tls_dir.join("server.key"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(tls_dir.join("server.crt"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&tls_dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let tls_xml = stage.dir.join("tls.xml");
    fs::write(&tls_xml, TLS_XML).unwrap();
    let ca_pem = stage.dir.join("ca.pem");
    fs::write(&ca_pem, ca.cert.to_pem().unwrap()).unwrap();
    let other_ca_pem = stage.dir.join("other_ca.pem");
    fs::write(&other_ca_pem, other_ca.cert.to_pem().unwrap()).unwrap();

    let server = ChServer::start(
        NAME,
        StartOpts {
            https: true,
            copy_in: vec![
                CopyIn { host_path: tls_xml.clone(), container_path: "/etc/clickhouse-server/config.d/tls.xml".to_string() },
                CopyIn { host_path: tls_dir.clone(), container_path: "/etc/clickhouse-server/tls".to_string() },
            ],
            ..Default::default()
        },
    );
    let tls_config = |host: &str, ca_file: Option<&std::path::Path>| {
        let mut c = ClickHouseConfig::new(server.https_url(host), server.database.clone(), server.user.clone()).with_password_env(server.password_var.clone());
        c.timeouts = av_http1::Timeouts::uniform(std::time::Duration::from_secs(20));
        c.ca_file = ca_file.map(|p| p.to_path_buf());
        c
    };

    step("https://127.0.0.1 with the right CA: opens, migrates, writes and reads an exact study over TLS");
    let mut store = ClickHouseStudyStore::connect(&tls_config("127.0.0.1", Some(&ca_pem))).unwrap_or_else(|e| panic!("TLS connect: {e}"));
    let study = hard_float_study();
    store.put_study(&study).unwrap();
    assert_identical(&store.get_study("hard-floats").unwrap().unwrap(), &study, "over TLS");
    println!("  https://127.0.0.1:{} verified against the test CA; study round-tripped", server.https_port.unwrap());

    step("a wrong CA is a typed TLS failure");
    let err = ClickHouseStudyStore::connect(&tls_config("127.0.0.1", Some(&other_ca_pem))).unwrap_err();
    match &err {
        StoreError::Tls { endpoint, reason } => {
            assert!(endpoint.starts_with("https://127.0.0.1:"), "{endpoint}");
            assert!(reason.to_ascii_lowercase().contains("certificate"), "{reason}");
            println!("  wrong CA -> Tls: {reason}");
        }
        other => panic!("expected StoreError::Tls, got {other:?}"),
    }

    step("no CA given (system store only): the test CA is not trusted, typed TLS failure");
    assert!(matches!(ClickHouseStudyStore::connect(&tls_config("127.0.0.1", None)).unwrap_err(), StoreError::Tls { .. }));

    step("the same certificate and CA are refused by host: https://localhost is not in the certificate's SANs");
    let err = ClickHouseStudyStore::connect(&tls_config("localhost", Some(&ca_pem))).unwrap_err();
    match &err {
        StoreError::Tls { reason, .. } => println!("  localhost (same cert, same CA) -> Tls: {reason}"),
        other => panic!("expected a hostname-verification TLS failure, got {other:?}"),
    }

    step("plaintext is still only on loopback: http:// to the TLS port is not mistaken for TLS");
    let mut plain_to_tls_port = tls_config("127.0.0.1", None);
    plain_to_tls_port.url = format!("http://127.0.0.1:{}", server.https_port.unwrap());
    assert!(ClickHouseStudyStore::connect(&plain_to_tls_port).is_err());
}

// ---------------------------------------------------------------------------------------------
// MoE helpers.
// ---------------------------------------------------------------------------------------------

/// Many draws per point, all awkward: where the order of the additions visibly matters.
fn moe_many_draws() -> pb::SweepResults {
    let mut rng = Lcg(0xfeed_beef_0042);
    let mut samples = Vec::new();
    for p in 0..6u32 {
        for d in 0..3000u32 {
            samples.push(sample_with(p, d, vec![score("alpha", rng.awkward(), None), score("beta", rng.awkward(), Some(rng.unit() > 0.5))]));
        }
    }
    let mut keyed: Vec<(u64, pb::SweepSample)> = samples.into_iter().map(|s| (rng.next(), s)).collect();
    keyed.sort_by_key(|(k, _)| *k);
    pb::SweepResults { sweep_id: "moe-many-draws".to_string(), sweep_hash: "h".to_string(), drm_hash: "d".to_string(), samples: keyed.into_iter().map(|(_, s)| s).collect(), aggregates: Vec::new(), provenance: None }
}

/// The SQL aggregates of the stored study must equal `aggregate()` over the same sample list, bit
/// for bit (a NaN equals a NaN: its payload is whichever operand the hardware propagates).
fn check_moe(store: &ClickHouseStudyStore, study: &pb::SweepResults) {
    let want = aggregate(&study.samples).expect("no mixed criterion in the MoE studies");
    let got = store.aggregates_in_sql(&study.sweep_id).unwrap().expect("study present");
    assert_eq!(got.len(), want.len(), "row count");
    assert!(got.iter().all(|a| a.point_index != 200), "a point whose every draw failed has no row");
    let (mut fields, mut nan_fields, mut bit_exact) = (0usize, 0usize, 0usize);
    for (g, w) in got.iter().zip(&want) {
        assert_eq!((&g.name, g.point_index, g.draws), (&w.name, w.point_index, w.draws), "key/draws");
        for (field, a, b) in [("mean", g.mean, w.mean), ("std_dev", g.std_dev, w.std_dev), ("min", g.min, w.min), ("max", g.max, w.max)] {
            fields += 1;
            assert!(f_same(a, b), "point {} {:?} {field}: SQL {a:?} ({:#x}) vs Rust {b:?} ({:#x})", g.point_index, g.name, a.to_bits(), b.to_bits());
            if a.is_nan() {
                nan_fields += 1;
            } else {
                assert_eq!(a.to_bits(), b.to_bits());
                bit_exact += 1;
            }
        }
        assert_eq!(g.pass_fraction.map(f64::to_bits), w.pass_fraction.map(f64::to_bits), "pass_fraction at point {} {:?}", g.point_index, g.name);
    }
    println!("  MoE ({}): {} rows, {fields} f64 fields compared: {bit_exact} bit-identical, {nan_fields} NaN==NaN, 0 differing", study.sweep_id, got.len());
}

/// Prints how often ClickHouse's native `avg()` / `sum()` differ from the Rust mean / left-to-right
/// sum on the same rows (information, not an assertion: the claim under test is the fold's).
fn measure_native(store: &ClickHouseStudyStore, study: &pb::SweepResults) {
    let id = &study.sweep_id;
    let want = aggregate(&study.samples).unwrap();
    let wid_bytes = store.client().query("SELECT write_id FROM `av_sweep_studies` WHERE sweep_id = {sid:String} FORMAT RowBinary", &[("sid", id)], "write id").unwrap();
    let wid = av_sweep::clickhouse::Decoder::new(&wid_bytes).string().unwrap();
    // Same group key and the same rows the fold uses; only the aggregate function differs.
    let sql = "SELECT name, p, avg(v), sum(v) FROM (SELECT sc.`key` AS name, sa.point_index AS p, sc.value AS v FROM (SELECT sample_ordinal, `key`, value FROM `av_sweep_sample_scores` WHERE sweep_id = {sid:String} AND write_id = {wid:String}) AS sc INNER JOIN (SELECT ordinal, point_index FROM `av_sweep_samples` WHERE sweep_id = {sid:String} AND write_id = {wid:String} AND error = '') AS sa ON sc.sample_ordinal = sa.ordinal) GROUP BY name, p ORDER BY p, name FORMAT RowBinary";
    let bytes = store.client().query(sql, &[("sid", id), ("wid", &wid)], "native avg/sum").unwrap();
    let mut d = av_sweep::clickhouse::Decoder::new(&bytes);
    let (mut groups, mut avg_diff, mut sum_diff) = (0, 0, 0);
    let mut differing: Vec<String> = Vec::new();
    for w in &want {
        let (name, p, avg, sum) = (d.string().unwrap(), d.u32().unwrap(), d.f64().unwrap(), d.f64().unwrap());
        assert_eq!((name.as_str(), p), (w.name.as_str(), w.point_index), "native query rows align with the aggregates");
        // The Rust left-to-right sum for the same group, in sample (ordinal) order.
        let rust_sum: f64 = study.samples.iter().filter(|s| s.error.is_empty() && s.point_index == p).filter_map(|s| s.scores.get(&name)).map(|s| s.value).sum();
        groups += 1;
        if !f_same(avg, w.mean) || !f_same(sum, rust_sum) {
            differing.push(format!("{name}@p{p}: native avg {avg:?} sum {sum:?} vs Rust mean {:?} sum {rust_sum:?}", w.mean));
        }
        avg_diff += usize::from(!f_same(avg, w.mean));
        sum_diff += usize::from(!f_same(sum, rust_sum));
    }
    d.finish().unwrap();
    println!("  MEASURED ({id}): native avg() differs from aggregate()'s mean in {avg_diff} of {groups} groups; native sum() differs from the Rust sum in {sum_diff} of {groups} groups; differing groups: {differing:?}");
}

/// Two points of 150 000 awkward draws each: groups big enough that a parallel native sum would
/// have a reason to add in a different order.
fn moe_huge() -> pb::SweepResults {
    let mut rng = Lcg(0x0bad_cafe_0077);
    let mut samples = Vec::new();
    for p in 0..2u32 {
        for d in 0..150_000u32 {
            samples.push(sample_with(p, d, vec![score("alpha", rng.awkward(), None)]));
        }
    }
    pb::SweepResults { sweep_id: "moe-huge".to_string(), sweep_hash: "h".to_string(), drm_hash: "d".to_string(), samples, aggregates: Vec::new(), provenance: None }
}
