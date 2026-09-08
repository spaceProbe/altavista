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
#[derive(Deserialize)]
struct Golden {
    drm_hash: String,
    objectives: Vec<PinnedObjective>,
    measures: Vec<PinnedMoe>,
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

    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: format!("golden-check-{}", case.name), error_mode: Default::default() , products_dir: None, replay: None };
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
        assert!((result.value - pinned.value).abs() < 1e-6, "case {:?}, objective {:?}: got {}, pinned {}", case.name, obj.name, result.value, pinned.value);
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
        assert!((result.value - pinned.value).abs() < 1e-6, "case {:?}, measure {:?}: got {}, pinned {}", case.name, moe.name, result.value, pinned.value);
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
