//! Verifies `goldens/expr_straight_accel.json` / `goldens/expr_fault_split_accel.json` (pinned
//! by `cargo run -p av-kernel --example gen_expr_goldens -- --reason "..."`, never by a test --
//! see that example's module doc comment) still match what this crate's current
//! `av_kernel::drm::execute` + `av_kernel::expr::objective` pipeline actually produces for the
//! same two DRMs, rebuilt here from `tests/expr_common/mod.rs` (the same fixtures the generator
//! itself uses). A value or pass/fail drifting from the pinned golden without the golden being
//! regenerated (through the generator, with a recorded `--reason`) is exactly the regression
//! this test exists to catch -- this file only ever reads the golden JSON, it never writes it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_kernel::drm::{execute, RunConfig};
use av_kernel::expr::objective::{evaluate_moe, evaluate_objective};
use av_kernel::expr::ExprRunProducts;
use gmat_sys::Gmat;
use serde::Deserialize;

#[path = "expr_common/mod.rs"]
mod expr_common;
use expr_common::{fault_split_accel_case, range_duration_case, GoldenCase};

#[derive(Deserialize)]
struct PinnedObjective {
    name: String,
    value: f64,
    pass: bool,
}
#[derive(Deserialize)]
struct PinnedMoe {
    name: String,
    value: f64,
}
/// Question 230: the golden-comparison tolerance this file actually checks pinned objective/
/// measure `value`s against (the `1e-6` bound below), moved into the golden itself by
/// `examples/gen_expr_goldens.rs` -- NOT the same thing as `PinnedObjective::tolerance` above,
/// which is each `Objective`'s own DRM-declared pass/fail acceptance band. This struct's `unit`/
/// `source` fields are read only to prove the field parses; the check itself only needs `value`.
#[derive(Deserialize)]
struct GoldenComparisonTolerance {
    value: f64,
    unit: String,
    source: String,
}
#[derive(Deserialize)]
struct Golden {
    drm_hash: String,
    objectives: Vec<PinnedObjective>,
    measures: Vec<PinnedMoe>,
    golden_comparison_tolerance: Option<GoldenComparisonTolerance>,
}

fn goldens_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

fn load_golden(name: &str) -> Golden {
    let text = std::fs::read_to_string(goldens_path(name)).unwrap_or_else(|e| panic!("reading goldens/{name}.json: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing goldens/{name}.json: {e}"))
}

fn check_case(case: &GoldenCase) {
    let golden = load_golden(case.name);
    assert_eq!(golden.drm_hash, case.drm.hash, "case {:?}: DRM hash drifted from the pinned golden's own recorded hash", case.name);
    // Question 230: read from the golden itself, never a bare Rust constant -- a reader that
    // silently fell back to a default tolerance here would pin nothing (round 1's own review
    // lesson, restated in this task). Does NOT cover the separate 1e-9 RunProducts.scores-vs-
    // evaluate_objective/evaluate_moe self-consistency checks below, which compare two values
    // this test computes fresh in the same run and never touch the golden at all.
    let recorded_tol = golden
        .golden_comparison_tolerance
        .as_ref()
        .unwrap_or_else(|| panic!("case {:?}: goldens/{}.json is missing golden_comparison_tolerance -- regenerate it with `cargo run -p av-kernel --example gen_expr_goldens -- --reason \"...\"` (question 230: this field must be present, never defaulted)", case.name, case.name));
    // `unit` and `source` are asserted rather than carried as dead fields (no `#[allow]` in this
    // repository to silence a lint): a tolerance whose provenance is blank records nothing, and
    // recording where the number came from is half of what question 230 asked for.
    assert!(!recorded_tol.unit.is_empty(), "case {:?}: golden_comparison_tolerance.unit must name the unit the bound is in", case.name);
    assert!(!recorded_tol.source.is_empty(), "case {:?}: golden_comparison_tolerance.source must say where the number came from", case.name);
    eprintln!("[expr_goldens] case {:?}: tolerance {} ({}) -- {}", case.name, recorded_tol.value, recorded_tol.unit, recorded_tol.source);
    let tol = recorded_tol.value;

    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: format!("golden-check-{}", case.name), error_mode: Default::default() , products_dir: None, replay: None, command_source: None };
    let products = execute(cfg).unwrap_or_else(|e| panic!("case {:?}: DRM execution failed: {e}", case.name));
    let scenario = case.drm.scenario.as_ref().unwrap();
    // Built from execute()'s own real RunProducts.events (question 95, M9.3) -- not a
    // caller-supplied stand-in.
    let run = ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &products.trajectories, &products.events);

    // execute() itself scored exactly what DesignReferenceMission.objectives/.measures declare
    // -- no more, no fewer (question 93).
    assert_eq!(
        products.scores.len(),
        case.drm.objectives.len() + case.drm.measures.len(),
        "case {:?}: RunProducts.scores must have exactly one entry per declared objective/measure",
        case.name
    );

    assert_eq!(golden.objectives.len(), case.objectives.len(), "case {:?}: objective count drifted from the pinned golden", case.name);
    for (obj, pinned) in case.objectives.iter().zip(&golden.objectives) {
        assert_eq!(obj.name, pinned.name, "case {:?}: objective ordering drifted from the pinned golden", case.name);
        let result = evaluate_objective(obj, &run).unwrap_or_else(|e| panic!("case {:?}, objective {:?}: {e}", case.name, obj.name));
        assert!((result.value - pinned.value).abs() < tol, "case {:?}, objective {:?}: got {}, pinned {}", case.name, obj.name, result.value, pinned.value);
        assert_eq!(result.pass, pinned.pass, "case {:?}, objective {:?}: pass/fail drifted from the pinned golden", case.name, obj.name);
        // execute()'s own RunProducts.scores (question 93) must agree exactly with evaluating
        // the same objective independently against execute()'s trajectories/events -- proves
        // execute() itself, not just this test's own re-derived ExprRunProducts, computes real
        // scores. Every objective (including an event.*-referencing one, since M9.3 -- question
        // 95) is declared on `DesignReferenceMission.objectives` for every case this file
        // checks, so this is unconditional now.
        let score = &products.scores[&obj.name];
        assert!((score.value - result.value).abs() < 1e-9, "case {:?}, objective {:?}: RunProducts.scores disagrees with evaluate_objective", case.name, obj.name);
        assert_eq!(score.passed, Some(result.pass), "case {:?}, objective {:?}: RunProducts.scores.passed disagrees", case.name, obj.name);
    }

    assert_eq!(golden.measures.len(), case.measures.len(), "case {:?}: measure count drifted from the pinned golden", case.name);
    for (moe, pinned) in case.measures.iter().zip(&golden.measures) {
        assert_eq!(moe.name, pinned.name, "case {:?}: measure ordering drifted from the pinned golden", case.name);
        let result = evaluate_moe(moe, &run).unwrap_or_else(|e| panic!("case {:?}, measure {:?}: {e}", case.name, moe.name));
        assert!((result.value - pinned.value).abs() < tol, "case {:?}, measure {:?}: got {}, pinned {}", case.name, moe.name, result.value, pinned.value);
        // See the objectives loop above -- every measure is declared on the DRM itself now too.
        let score = &products.scores[&moe.name];
        assert!((score.value - result.value).abs() < 1e-9, "case {:?}, measure {:?}: RunProducts.scores disagrees with evaluate_moe", case.name, moe.name);
        assert_eq!(score.passed, None, "case {:?}, measure {:?}: a MeasureOfEffectiveness must never carry pass/fail", case.name, moe.name);
    }
}

#[test]
fn straight_accel_scores_match_the_pinned_golden() {
    check_case(&expr_common::straight_accel_case());
}

#[test]
fn fault_split_accel_scores_match_the_pinned_golden() {
    check_case(&fault_split_accel_case());
}

/// The amended grammar's worked example (`duration(range(a, b) < 100 m)`) run against real
/// products and re-pinned -- `docs/adr/005-simulation-kernel.md`'s amendment 2026-09-02, task
/// item "What to build" 6.
#[test]
fn range_duration_scores_match_the_pinned_golden() {
    check_case(&range_duration_case());
}

#[test]
fn all_goldens_declare_at_least_one_failing_objective_and_one_passing_objective() {
    // Guards against a future edit quietly making every golden all-pass (which would stop
    // catching a regression that broke `pass` computation itself -- see
    // `examples/gen_expr_goldens.rs`'s module doc comment).
    let mut sos_map = BTreeMap::new();
    for name in ["expr_straight_accel", "expr_fault_split_accel", "expr_range_duration"] {
        let g = load_golden(name);
        sos_map.insert(name, g.objectives.iter().map(|o| o.pass).collect::<Vec<_>>());
    }
    let all: Vec<bool> = sos_map.values().flatten().copied().collect();
    assert!(all.iter().any(|&p| p), "at least one pinned objective across every golden must pass");
    assert!(all.iter().any(|&p| !p), "at least one pinned objective across every golden must fail (expr_straight_accel's final_vx_near_5, by design)");
}
