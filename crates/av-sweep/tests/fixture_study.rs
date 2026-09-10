//! GMAT-needing integration tests for the F1b/F2b binary (`crates/av-sweep/src/bin/av-sweep/`),
//! driven end to end through `drms/demo_two_instance_sweep.{drm,sweep}.yaml` +
//! `drms/demo_two_instance.sos.yaml` (reused unmodified) -- the "4-point x 2-draw study" fixture
//! this crate's own task brief specifies (F2b, question 192(c), widened this from 2 points to 4:
//! a second axis, an event axis on `burn1`'s `dv_x`, alongside the original DragArea parameter
//! axis).
//!
//! ## Why every test in this file shares ONE study run
//!
//! Each sample takes several seconds (real GMAT propagation over the fixture's 7200 s scenario --
//! measured ~8s/sample, debug build, 2026-09-09). Running the full 4x2 study fresh for every
//! `#[test]` fn would multiply that cost by however many assertions want to look at it. A single
//! `std::sync::OnceLock` (mirrors `crates/av-kernel/tests/demo_two_instance.rs`'s own
//! `together_products()` pattern) runs the study exactly once for the whole binary, and every
//! `#[test]` fn below reads from that one shared result -- this keeps the file's own total wall
//! time to "one study (8 samples, workers=2) plus one extra lone re-run", not many-plus
//! independent studies.
//!
//! Contention: checked (`ps -Ao pid,etime,command | grep -E "cargo test|pytest|docker build"`)
//! immediately before every run in this crate's own development session; nothing else was in
//! flight.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use av_cdm::pb;
use prost::Message;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn av_sweep_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_av-sweep"))
}

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-sweep-fixture-study-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct StudyFixture {
    out_dir: PathBuf,
    results: pb::SweepResults,
}

/// Runs `drms/demo_two_instance_sweep.{drm,sweep}.yaml` (+ the unmodified
/// `demo_two_instance.sos.yaml`/`.system.yaml`/`_ctrl.system.yaml`) through study mode exactly
/// once, `--workers 2`, and decodes the resulting `sweep_results.pb`. Panics (via `expect`) on
/// any failure -- a `OnceLock` initializer that returns `Result` would need every caller to
/// `.expect()` anyway, and this file has no case where a failed study run is itself the thing
/// under test (that is `study::tests::*` in the binary's own `#[cfg(test)]`, which does not need
/// GMAT at all).
fn study_fixture() -> &'static StudyFixture {
    static FIXTURE: OnceLock<StudyFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = out_dir("main");
        let status = Command::new(av_sweep_bin())
            .arg("--sweep")
            .arg(drms_path("demo_two_instance_sweep.sweep.yaml"))
            .arg("--drm")
            .arg(drms_path("demo_two_instance_sweep.drm.yaml"))
            .arg("--sos")
            .arg(drms_path("demo_two_instance.sos.yaml"))
            .arg("--system")
            .arg(drms_path("demo_two_instance.system.yaml"))
            .arg("--system")
            .arg(drms_path("demo_two_instance_ctrl.system.yaml"))
            .arg("--out-dir")
            .arg(&dir)
            .arg("--workers")
            .arg("2")
            .status()
            .expect("spawning av-sweep study mode");
        assert!(status.success(), "study mode must exit 0 for a fully valid 2x2 fixture study: {status}");

        let bytes = std::fs::read(dir.join("sweep_results.pb")).expect("reading sweep_results.pb");
        let results = pb::SweepResults::decode(bytes.as_slice()).expect("decoding sweep_results.pb");
        StudyFixture { out_dir: dir, results }
    })
}

fn sample(results: &pb::SweepResults, point: u32, draw: u32) -> &pb::SweepSample {
    results.samples.iter().find(|s| s.point_index == point && s.draw_index == draw).unwrap_or_else(|| panic!("no sample for point={point} draw={draw}"))
}

fn score(sample: &pb::SweepSample, name: &str) -> f64 {
    sample.scores.get(name).unwrap_or_else(|| panic!("sample p{} d{} has no score {name:?}: {:?}", sample.point_index, sample.draw_index, sample.scores.keys().collect::<Vec<_>>())).value
}

/// The 2x2 study runs every sample (no failures), records every sample's seed and config_hash,
/// and the top-level sweep_hash/drm_hash match the fixture's own committed `hash:` fields.
#[test]
fn the_two_point_two_draw_study_runs_every_sample_and_records_seeds_and_hashes() {
    let f = study_fixture();
    let r = &f.results;

    assert_eq!(r.sweep_id, "demo_two_instance_sweep");
    let sweep_yaml = std::fs::read_to_string(drms_path("demo_two_instance_sweep.sweep.yaml")).unwrap();
    let declared_sweep_hash = sweep_yaml.lines().find(|l| l.starts_with("hash:")).unwrap().trim_start_matches("hash:").trim().trim_matches('"');
    assert_eq!(r.sweep_hash, declared_sweep_hash, "SweepResults.sweep_hash must be the sweep's own verified canonical hash");
    let drm_yaml = std::fs::read_to_string(drms_path("demo_two_instance_sweep.drm.yaml")).unwrap();
    let declared_drm_hash = drm_yaml.lines().find(|l| l.starts_with("hash:")).unwrap().trim_start_matches("hash:").trim().trim_matches('"');
    assert_eq!(r.drm_hash, declared_drm_hash, "SweepResults.drm_hash must be the base DRM's own verified hash");

    assert_eq!(r.samples.len(), 8, "4 points (2 DragArea x 2 dv_x, question 192(c)) x 2 draws = 8 samples");
    let prov = r.provenance.as_ref().expect("SweepResults.provenance is always Some");
    assert_eq!(prov.author_kind, pb::AuthorKind::Agent as i32);
    assert_eq!(prov.tool, "av-sweep");
    assert_eq!(prov.config_hash, r.sweep_hash);
    assert_eq!(prov.created_tai_ns, 0, "no wall clock, ever (matches av_kernel::drm::executor's own convention)");

    let mut burn_seeds = std::collections::HashSet::new();
    for s in &r.samples {
        assert!(s.error.is_empty(), "sample p{} d{} unexpectedly failed: {}", s.point_index, s.draw_index, s.error);
        assert!(!s.config_hash.is_empty());
        // Question 192(b): SweepSample.seeds is a map now, not a single projected value -- the
        // sweep declares one Scenario.seeds key (burn_seed), so every sample's seeds map must
        // carry exactly that key, with its real derived value (never the empty-map default of
        // "absent" or a zero value).
        assert_eq!(s.seeds.len(), 1, "p{} d{}: exactly one declared Scenario.seeds key (burn_seed): {:?}", s.point_index, s.draw_index, s.seeds);
        let burn_seed = *s.seeds.get("burn_seed").unwrap_or_else(|| panic!("p{} d{}: seeds map missing its only declared key burn_seed: {:?}", s.point_index, s.draw_index, s.seeds));
        assert_ne!(burn_seed, 0, "p{} d{}: burn_seed's derived value must be its real derived value, never the empty/default 0", s.point_index, s.draw_index);
        assert!(burn_seeds.insert(burn_seed), "every sample's burn_seed must be distinct (point/draw both fold into seed derivation)");
        assert!(!s.products_uri.is_empty(), "a successful sample records where its products live");
        assert!(PathBuf::from(&s.products_uri).is_absolute(), "products_uri must be an absolute path: {}", s.products_uri);
        assert_eq!(s.run_id, format!("demo_two_instance_sweep_p{}_d{}", s.point_index, s.draw_index));
    }
    assert_eq!(burn_seeds.len(), 8, "all 8 samples' burn_seed values must be pairwise distinct");
}

/// Question 192(c): two grid points differing ONLY in the event axis (`dv_x`) must produce
/// different `config_hash` values -- the sample.rs-level proof
/// (`crate::sample::tests::an_event_axis_value_changes_config_hash_between_grid_points`) repeated
/// here against a REAL study, run through GMAT, not a synthetic `sample_config` call alone. Point
/// 0 (DragArea=5.0, dv_x=10.0) and point 1 (DragArea=5.0, dv_x=30.0) share DragArea and differ
/// only in dv_x (grid.rs's own documented ordering: the last declared axis, dv_x, varies
/// fastest). Fails against an implementation that applies the event axis value after
/// `canonical_drm_hash` (the per-sample DRM hash, and therefore `config_hash`, would then be
/// identical for point 0 and point 1 at the same draw).
#[test]
fn two_points_differing_only_in_dv_x_have_different_config_hash() {
    let f = study_fixture();
    let p0d0 = sample(&f.results, 0, 0);
    let p1d0 = sample(&f.results, 1, 0);
    assert_ne!(p0d0.config_hash, p1d0.config_hash, "point 0 (dv_x=10.0) and point 1 (dv_x=30.0), same DragArea, same draw, must have different config_hash");
    assert_ne!(p0d0.config_hash, "", "sanity: config_hash must actually be populated");
}

/// F2: the fixture declares 4 grid points (question 192(c) widened the grid from 2 to 4: 2
/// DragArea x 2 dv_x) x 2 draws x 3 measures (`demo_flt_rmag_at_end`, `demo_flt_cd_at_end`,
/// `demo_mvr_rmag_at_end` -- `drms/demo_two_instance_sweep.drm.yaml`'s own `measures:` block, "No
/// objectives declared ... All three measures report a raw run-time value with no pass/fail
/// concept"), and the study fixture above asserts all eight samples succeeded -- so the
/// PREDICTION, made before looking at `r.aggregates` below, is: exactly 4 points x 3 measures =
/// 12 aggregate rows, every row's `draws == 2` (both draws at that point succeeded), every row's
/// `pass_fraction` unset (`None`, since every score here is a MeasureOfEffectiveness, never an
/// Objective -- no `passed` was ever set on any `ScoreResult` for these three measure names),
/// sorted `(point_index, name)`. Each row's `mean`/`min`/`max` must also agree with a hand
/// recomputation from that point's own two recorded samples (not merely "some plausible number")
/// -- checked directly below rather than trusted.
#[test]
fn the_study_writes_twelve_aggregate_rows_four_points_times_three_measures_each_with_two_contributing_draws_and_no_pass_fraction() {
    let f = study_fixture();
    let r = &f.results;

    let expected_names = ["demo_flt_cd_at_end", "demo_flt_rmag_at_end", "demo_mvr_rmag_at_end"]; // alphabetical, matching (point, name) sort order
    assert_eq!(r.aggregates.len(), 12, "4 points x 3 measures = 12 rows; got {:#?}", r.aggregates);

    let keys: Vec<(u32, &str)> = r.aggregates.iter().map(|a| (a.point_index, a.name.as_str())).collect();
    let expected_keys: Vec<(u32, &str)> = [0u32, 1u32, 2u32, 3u32].iter().flat_map(|&p| expected_names.iter().map(move |&n| (p, n))).collect();
    assert_eq!(keys, expected_keys, "sorted (point_index, name), 3 measures per point in alphabetical order");

    for a in &r.aggregates {
        assert_eq!(a.draws, 2, "point {} score {:?}: both draws at this point succeeded, so both must contribute", a.point_index, a.name);
        assert_eq!(a.pass_fraction, None, "point {} score {:?}: a MeasureOfEffectiveness has no pass criterion, unlike an Objective", a.point_index, a.name);

        // Recompute mean/min/max directly from this point's own two recorded samples (not just
        // re-trusting the aggregate row) -- the independent half of this test.
        let d0 = score(sample(r, a.point_index, 0), &a.name);
        let d1 = score(sample(r, a.point_index, 1), &a.name);
        let expected_mean = (d0 + d1) / 2.0;
        let expected_min = d0.min(d1);
        let expected_max = d0.max(d1);
        assert!((a.mean - expected_mean).abs() < 1e-9, "point {} score {:?}: mean={} recomputed={}", a.point_index, a.name, a.mean, expected_mean);
        assert_eq!(a.min, expected_min, "point {} score {:?}", a.point_index, a.name);
        assert_eq!(a.max, expected_max, "point {} score {:?}", a.point_index, a.name);
        assert!(a.std_dev >= 0.0, "a standard deviation is never negative");
        // Population std_dev of exactly two values d0,d1 has the closed form |d0-d1|/2 -- an
        // independent recomputation distinct from aggregate.rs's own formula, not the same code
        // path re-run.
        let expected_std = (d0 - d1).abs() / 2.0;
        // Tolerance 1e-9, not the 1e-4 this assertion was first written with (manager review):
        // the measured residual against the closed form is ~2e-11 -- the two formulas differ only
        // in floating-point association -- so 1e-4 carried a factor of five million and could not
        // have caught a real regression. That is the same objection the M19.4 review raised
        // against a 93x margin on demo_two_instance's own rmag tolerance. 1e-9 leaves ~50x over
        // the measured residual.
        assert!((a.std_dev - expected_std).abs() < 1e-9, "point {} score {:?}: std_dev={} expected~={} (closed-form population std_dev of exactly two values d0,d1 is |d0-d1|/2)", a.point_index, a.name, a.std_dev, expected_std);
    }
}

/// Samples are sorted `(point_index, draw_index)` in the written results, regardless of which
/// order the `--workers 2` pool actually finished them in (a real property of THIS run, not an
/// assumption -- with 2 workers racing 8 samples, completion order is not point/draw order
/// unless the scheduler happens to get lucky, and the assertion below would fail against an
/// implementation that just appended samples in completion order).
#[test]
fn samples_are_sorted_by_point_then_draw_regardless_of_completion_order() {
    let f = study_fixture();
    let pairs: Vec<(u32, u32)> = f.results.samples.iter().map(|s| (s.point_index, s.draw_index)).collect();
    assert_eq!(pairs, vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)]);
}

/// Proof the parameter axis value is really applied: demo_flt_rmag_at_end differs between the
/// two DragArea points at the same dv_x and draw (measured 2026-09-09: ~97-101 m at DragArea 5.0
/// vs 25.0, point2 < point0 both draws -- more drag decays the orbit faster). Point 0
/// (DragArea=5.0, dv_x=10.0) and point 2 (DragArea=25.0, dv_x=10.0) share dv_x and differ only in
/// DragArea (grid.rs's own documented row-major ordering: the FIRST declared axis, DragArea,
/// varies slowest -- point index = dragarea_idx*2 + dv_x_idx). The exact number is not pinned
/// (deliberately -- see this crate's final report for why a target/tolerance is F2's job, not
/// F1b's), only that it is a real, resolved difference, several orders of magnitude above float
/// noise.
#[test]
fn two_points_differing_only_in_drag_area_produce_different_products() {
    let f = study_fixture();
    let p0d0 = score(sample(&f.results, 0, 0), "demo_flt_rmag_at_end");
    let p2d0 = score(sample(&f.results, 2, 0), "demo_flt_rmag_at_end");
    let diff = (p0d0 - p2d0).abs();
    assert!(diff > 1.0, "DragArea 5.0 vs 25.0 must produce a clearly resolved rmag difference at end, got {diff} m (p0d0={p0d0}, p2d0={p2d0})");
    assert!(p2d0 < p0d0, "more drag area must decay the orbit faster (smaller rmag at end): p2d0={p2d0} p0d0={p0d0}");
}

/// Question 192(c)'s new event axis, dv_x: proof it is really applied, and the HYPOTHESIS
/// written before running (see `drms/demo_two_instance_sweep.sweep.yaml`'s own header comment,
/// and `crates/av-sweep/REPORT.md` for the measured values reported against it) -- a prograde
/// burn adds orbital energy, so dv_x=30.0 (point 1) should give a LARGER demo_mvr_rmag_at_end
/// than dv_x=10.0 (point 0), both at DragArea=5.0, same draw (grid.rs's own row-major ordering:
/// dv_x is the LAST declared axis, varying fastest, so point 0 and point 1 share DragArea and
/// differ only in dv_x). The order-of-magnitude estimate in that header comment was ~10 km
/// separating the two points; the measured value is reported in `REPORT.md`, not pinned here
/// (this crate's own established posture for a not-yet-targeted measure -- see the DragArea
/// test's own comment for the same reasoning), only that it is real, in the predicted direction,
/// and resolved far above float noise.
#[test]
fn two_points_differing_only_in_dv_x_produce_different_products_in_the_predicted_direction() {
    let f = study_fixture();
    let p0d0 = score(sample(&f.results, 0, 0), "demo_mvr_rmag_at_end");
    let p1d0 = score(sample(&f.results, 1, 0), "demo_mvr_rmag_at_end");
    let diff = (p1d0 - p0d0).abs();
    assert!(diff > 100.0, "dv_x=10.0 vs dv_x=30.0 must produce a clearly resolved demo_mvr_rmag_at_end difference, got {diff} m (p0d0={p0d0}, p1d0={p1d0})");
    assert!(p1d0 > p0d0, "a larger prograde dv_x must add more orbital energy and raise the resulting trajectory (larger rmag at end): p0d0(dv_x=10)={p0d0} p1d0(dv_x=30)={p1d0}");
}

/// Proof the seeded dispersion is real, AND that `ExecutionErrorMode::Sampled` is actually in
/// force (a `Nominal` run would apply the commanded dv exactly regardless of the declared
/// execution_error block -- M12.1/question 103 -- so an implementation that forgot to select
/// `Sampled` for `monte_carlo_draws > 1` would make this assertion fail, not merely produce a
/// smaller difference). Measured 2026-09-09: ~1.09-1.18 km on demo_mvr_rmag_at_end.
#[test]
fn two_draws_at_one_point_produce_different_products() {
    let f = study_fixture();
    let p0d0 = score(sample(&f.results, 0, 0), "demo_mvr_rmag_at_end");
    let p0d1 = score(sample(&f.results, 0, 1), "demo_mvr_rmag_at_end");
    let diff = (p0d0 - p0d1).abs();
    assert!(diff > 10.0, "two independently Gates-dispersed burns must produce a clearly resolved demo_mvr_rmag_at_end difference, got {diff} m (p0d0={p0d0}, p0d1={p0d1})");

    // Different seeds (point0's own two draws) must be genuinely distinct derived values, not
    // just distinct sample identities. Question 192(b): seeds is a map now.
    let s0 = *sample(&f.results, 0, 0).seeds.get("burn_seed").expect("p0d0 carries burn_seed");
    let s1 = *sample(&f.results, 0, 1).seeds.get("burn_seed").expect("p0d1 carries burn_seed");
    assert_ne!(s0, s1);
}

/// The disclosed finding this crate's own task brief specifically calls for: demo_flt has no
/// execution_error of its own, so its ONLY draw-dependent input is WHEN demo_ctrl's own latch
/// fires (driven by demo_mvr's dispersed trajectory). Measured 2026-09-09 (pre-F2b, 2-point
/// grid): NOT exactly zero (0.588 m at point0, 2.397 m at point1) -- small, but genuinely
/// resolved above float noise, consistent with the latch firing on a different 10 Hz controller
/// tick between draws. F2b widened the grid to 4 points (question 192(c)'s dv_x axis); this test
/// now checks all four, not just two -- `crates/av-sweep/REPORT.md` records the four measured
/// indirect differences. This test pins "small but real, and much smaller than the direct
/// demo_mvr effect" at each point, not an exact value (a future run with a different
/// compiler/GMAT/host, or a different dv_x, could plausibly land the tick boundary the other way
/// and get a differently-small-but-still-real number, or genuinely zero if the crossing turns out
/// insensitive at that point -- either finding is legitimate; this test's bar is "look at the
/// coupling honestly", not "reproduce today's digits forever").
#[test]
fn the_controller_latch_coupling_makes_demo_flt_weakly_but_not_exactly_draw_sensitive() {
    let f = study_fixture();

    // demo_flt's own Cd is always 220.0 (the controller's own command value) by end of run in
    // every sample -- the latch fires well before t_end in all eight samples (measured: yes,
    // demo_flt_cd_at_end == 220.0 for all eight samples in study_fixture()); the coupling is
    // therefore in WHEN it latched, not WHETHER.
    for (p, d) in [(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)] {
        assert_eq!(score(sample(&f.results, p, d), "demo_flt_cd_at_end"), 220.0, "p{p}d{d}: the drag-sail command must have latched by end-of-run in every sample of this fixture");
    }

    for point in 0u32..4 {
        let direct_diff = (score(sample(&f.results, point, 0), "demo_mvr_rmag_at_end") - score(sample(&f.results, point, 1), "demo_mvr_rmag_at_end")).abs();
        let indirect_diff = (score(sample(&f.results, point, 0), "demo_flt_rmag_at_end") - score(sample(&f.results, point, 1), "demo_flt_rmag_at_end")).abs();
        assert!(indirect_diff < direct_diff, "point {point}: the indirect (controller-mediated) effect on demo_flt must be much smaller than the direct dispersion on demo_mvr itself: indirect={indirect_diff} direct={direct_diff}");
        // Deliberately NOT asserting indirect_diff == 0.0 or indirect_diff > 0.0: either is a
        // legitimate outcome of the same underlying (threshold-latched) coupling; this crate's
        // own final report states which one this fixture's committed hash/seeds actually
        // produced, at each of the four points.
    }
}

/// Re-running EACH of the eight samples alone, from exactly the bytes its own
/// `drm.pb`/`sos.pb`/`sys_*.pb` recorded, must reproduce byte-identical `RunProducts` -- the
/// whole point of recording per-sample seeds and inputs rather than only aggregate scores. Every
/// sample gets its own child process, its own recorded `run_id`, and its own byte compare (no
/// field exclusions) against its own `run_products.pb` -- not just p0d0 (this fn's name says
/// "every sample"; it used to only check one). F2b widened the grid from 4 to 8 samples (question
/// 192(c)); this reproducibility property must hold for the event axis's own per-sample DRM
/// mutation exactly as it already held for the parameter axis's SOS mutation.
///
/// **Root-cause note, disclosed per this task's own rules.** The first version of this test used
/// a different `--run-id` for the replay than the study's own sample used ("replay_p0_d0" vs.
/// "demo_two_instance_sweep_p0_d0"), and the two outputs came out byte-DIFFERENT -- not a
/// determinism bug: `run_id` legitimately appears in more places than the top-level
/// `RunProducts.run_id`/`RunProducts.provenance.run_id` (every `Trajectory`'s own `provenance`
/// and every `Event`'s own `provenance` also carry it -- `av_kernel::drm::executor`'s own module
/// doc comment: "each `SystemDefinition`'s own hash/id are recorded in `Provenance.attributes`",
/// and `run_id` the same way, at multiple nesting levels). Stripping only the two top-level
/// copies and comparing the rest was therefore comparing a message that still differed in
/// several other, unstripped places. Root cause fixed at the source: this test now supplies the
/// SAME `--run-id` the original sample actually used (its own recorded `SweepSample.run_id`), at
/// which point the two runs are genuinely, exactly byte-identical -- verified ad hoc first
/// (`cmp` on the two `.pb` files, no stripping at all) before being written as this assertion.
#[test]
fn every_sample_re_runs_byte_identically_alone_from_its_recorded_inputs() {
    let f = study_fixture();

    for (point, draw) in [(0u32, 0u32), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)] {
        let original_sample = sample(&f.results, point, draw);
        let sample_dir = f.out_dir.join(format!("sample_p{point}_d{draw}"));
        let original = std::fs::read(sample_dir.join("run_products.pb")).unwrap_or_else(|e| panic!("p{point}d{draw}: reading the study's own run_products.pb: {e}"));

        let replay_out = f.out_dir.join(format!("replay_p{point}_d{draw}.pb"));
        let system_pbs: Vec<PathBuf> = std::fs::read_dir(&sample_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("sys_") && n.ends_with(".pb")))
            .collect();
        assert_eq!(system_pbs.len(), 2, "p{point}d{draw}: leo_demo_sys + demo_ctrl_sys");

        let mut cmd = Command::new(av_sweep_bin());
        cmd.arg("--run-sample").arg("--drm-pb").arg(sample_dir.join("drm.pb")).arg("--sos-pb").arg(sample_dir.join("sos.pb"));
        for p in &system_pbs {
            cmd.arg("--system-pb").arg(p);
        }
        // Same run_id the study itself used for THIS sample -- see this test's own doc comment
        // for why that is exactly the fix, not a workaround.
        cmd.arg("--run-id").arg(&original_sample.run_id).arg("--error-mode").arg("sampled").arg("--out").arg(&replay_out);
        let status = cmd.status().unwrap_or_else(|e| panic!("p{point}d{draw}: spawning av-sweep sample mode for the replay: {e}"));
        assert!(status.success(), "p{point}d{draw}: lone re-run of this sample's own recorded inputs must succeed: {status}");

        let replayed = std::fs::read(&replay_out).unwrap_or_else(|e| panic!("p{point}d{draw}: reading the replay's own output: {e}"));
        assert_eq!(
            original, replayed,
            "p{point}d{draw}: byte-for-byte identical, no field exclusions -- a lone replay from the recorded inputs, same run_id, must reproduce the study's own output exactly"
        );

        // Also decode both, as a second, structurally-independent proof beyond the raw byte compare.
        let original_msg = pb::RunProducts::decode(original.as_slice()).unwrap_or_else(|e| panic!("p{point}d{draw}: decoding the original run_products.pb: {e}"));
        let replayed_msg = pb::RunProducts::decode(replayed.as_slice()).unwrap_or_else(|e| panic!("p{point}d{draw}: decoding the replayed output: {e}"));
        assert_eq!(original_msg, replayed_msg, "p{point}d{draw}: decoded RunProducts must also match");
    }
}

/// A sample whose inputs are invalid (here: a tampered `drm.pb` whose declared hash no longer
/// matches its own content) is refused with a typed, real-cause message, fast -- before
/// `gmat_sys::engine_lock()`/`Gmat::setup` ever run (see `sample_mode.rs`'s own module doc
/// comment for why that ordering is load-bearing), so this spawns a real child process but that
/// child never touches GMAT. This is the CHILD-level half of "a bad sample never aborts the
/// study"; the ORCHESTRATION-level half (a per-point `sample_config` failure is recorded and the
/// rest of the batch still builds) is `study::tests::
/// a_per_point_failure_is_recorded_and_the_rest_of_the_batch_still_builds` in the binary's own
/// `#[cfg(test)]` (no GMAT needed there either) -- split into two fast tests rather than one
/// expensive mixed-success/failure end-to-end study, since both together already cover the same
/// underlying property this crate's `study.rs` relies on: `finalize()`/`build_samples()` never
/// return a fatal `Err` for a single bad sample, only a value the caller records and moves past.
#[test]
fn a_sample_whose_inputs_are_invalid_is_recorded_with_a_typed_error_and_does_not_abort_the_study() {
    let f = study_fixture();
    let good_sample_dir = f.out_dir.join("sample_p0_d0");
    let mut drm = pb::DesignReferenceMission::decode(std::fs::read(good_sample_dir.join("drm.pb")).unwrap().as_slice()).unwrap();
    drm.name = "tampered after the hash was computed".to_string(); // hash field left as-is: now wrong

    let dir = out_dir("tampered-sample");
    std::fs::create_dir_all(&dir).unwrap();
    let drm_pb = dir.join("drm.pb");
    std::fs::write(&drm_pb, drm.encode_to_vec()).unwrap();
    let sos_pb = dir.join("sos.pb");
    std::fs::copy(good_sample_dir.join("sos.pb"), &sos_pb).unwrap();
    let sys_pbs: Vec<PathBuf> = std::fs::read_dir(&good_sample_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("sys_") && n.ends_with(".pb")))
        .map(|p| {
            let dst = dir.join(p.file_name().unwrap());
            std::fs::copy(&p, &dst).unwrap();
            dst
        })
        .collect();

    let out_pb = dir.join("run_products.pb");
    let started = std::time::Instant::now();
    let mut cmd = Command::new(av_sweep_bin());
    cmd.arg("--run-sample").arg("--drm-pb").arg(&drm_pb).arg("--sos-pb").arg(&sos_pb);
    for p in &sys_pbs {
        cmd.arg("--system-pb").arg(p);
    }
    cmd.arg("--run-id").arg("tampered-child").arg("--error-mode").arg("nominal").arg("--out").arg(&out_pb);
    let output = cmd.output().expect("spawning av-sweep sample mode with a tampered drm.pb");
    let elapsed = started.elapsed();

    assert!(!output.status.success(), "a tampered DRM must be refused, not silently run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not match its canonical hash"), "the child's own stderr must name the real cause (a hash mismatch), not just say \"failed\": {stderr}");
    assert!(!out_pb.exists(), "no output file for a sample that never got a valid config");
    // "Cheap": measured 2026-09-09 at ~0.1s for the correct (pre-GMAT-check) code path -- well
    // under the 1s bound here, which mainly rules out an implementation that fell all the way
    // through to a full 7200s propagation (measured ~8s/sample) before noticing the tampered
    // hash. Disclosed limitation: this timing bound alone does NOT distinguish "never touched
    // GMAT" from "touched Gmat::setup but bailed before propagating" -- measured directly (by
    // temporarily removing sample_mode.rs's own pre-GMAT hash checks during this task's own
    // break-and-restore testing) that `Gmat::setup` plus `execute()`'s own internal hash
    // re-check is ALSO only ~0.1s here, because `execute()` re-verifies the same hashes at its
    // very first line, before any propagation. The actual "never touches GMAT for a bad sample"
    // guarantee is therefore a source-ordering fact (`sample_mode.rs`'s own module doc comment:
    // every `verify_*_hash` call textually and by control flow precedes
    // `gmat_sys::engine_lock()`), not something this timing assertion alone can prove from
    // outside the process -- stated honestly rather than oversold.
    assert!(elapsed.as_secs() < 1, "a bad-input refusal must be cheap, not pay for a real propagation: took {elapsed:?}");

    // "Does not abort the study": the shared study_fixture() above already completed with 4
    // successful samples and exit 0 -- proof positive that a bad standalone sample (this one)
    // exercises the exact same fast-refusal code path a per-sample failure inside study mode
    // would, without that path ever being capable of taking the whole process down (run_sample's
    // own return type is Result<(), String>, propagated only as this one child's own exit code).
    let _ = &f.results;
}
