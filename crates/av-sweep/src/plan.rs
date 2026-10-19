//! What a study will contain, as far as it is known **before its first sample runs** (question 254
//! item 2), and the comparison of that plan with a study a store already holds.
//!
//! Study mode knows, before any sample runs: the `sweep_id`, `sweep_hash`, `drm_hash` and
//! `provenance`; per `(point, draw)` the `run_id`, `config_hash`, `axis_values` and `seeds`; the
//! complete record of every sample `build_samples` already failed; and the sample directory each
//! successful sample's `products_uri` would name. It does not know a sample's scores, whether it
//! will succeed, or the aggregates. [`StudyPlan::first_difference`] compares exactly that with an
//! existing study:
//!
//! - **no difference** means the existing study is compatible: an identical re-run would reproduce
//!   it (a sample whose run fails again has an empty `products_uri`, one that succeeds names its
//!   sample directory; both are allowed), so the store *provisionally accepts*. The final content
//!   is checked again at [`crate::store::StudyStore::prepare`], where scores and aggregates are
//!   known;
//! - **a difference** means no run of this plan can reproduce the existing study, so the store
//!   refuses with [`crate::store::WriteRefusal::DifferentPlan`] naming the field.
//!
//! The order of comparison, so "the first field that differs" is well defined: `sweep_id`,
//! `sweep_hash`, `drm_hash`, `provenance`, the number of samples, then each planned sample in
//! `(point, draw)` order (an absent sample, then its fields in the order of
//! [`PlannedSample`]'s variants below).

use std::collections::BTreeMap;
use std::fmt::Debug;

use av_cdm::pb;

/// A sample that will be run: everything of its record known before it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRun {
    pub point_index: u32,
    pub draw_index: u32,
    pub run_id: String,
    pub config_hash: String,
    pub axis_values: BTreeMap<String, f64>,
    pub seeds: BTreeMap<String, u64>,
    /// The canonical sample directory a successful run's `products_uri` names. A failed run's
    /// `products_uri` is empty, so an existing sample's must be empty or exactly this.
    pub products_dir: String,
}

/// One sample of the plan.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedSample {
    /// A sample that will run. Compared in the order `run_id`, `config_hash`, `axis_values`,
    /// `seeds`, `products_uri`.
    Runs(PlannedRun),
    /// A sample whose complete record is already known (one whose configuration could not even be
    /// built, or any sample of an imported study): an existing sample must equal it. Compared in
    /// the order `run_id`, `config_hash`, `axis_values`, `seeds`, `scores`, `products_uri`,
    /// `error`; floats by bits.
    Fixed(pb::SweepSample),
}

impl PlannedSample {
    fn key(&self) -> (u32, u32) {
        match self {
            PlannedSample::Runs(r) => (r.point_index, r.draw_index),
            PlannedSample::Fixed(s) => (s.point_index, s.draw_index),
        }
    }
}

/// The plan of one study. Build it with every sample of the grid, in any order.
#[derive(Debug, Clone, PartialEq)]
pub struct StudyPlan {
    pub sweep_id: String,
    pub sweep_hash: String,
    pub drm_hash: String,
    pub provenance: Option<pb::Provenance>,
    pub samples: Vec<PlannedSample>,
}

/// The first field in which a plan and an existing study differ, both sides rendered for a
/// message (long values are cut at 160 characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDifference {
    pub field: String,
    pub existing: String,
    pub planned: String,
}

fn show(value: &impl Debug) -> String {
    const CAP: usize = 160;
    let text = format!("{value:?}");
    if text.chars().count() <= CAP {
        return text;
    }
    let cut: String = text.chars().take(CAP).collect();
    format!("{cut}...")
}

/// `Some` when `existing` and `planned` differ. `same` decides equality (floats are compared by
/// bits by the callers that hold any).
fn differ<T: Debug>(field: impl Into<String>, existing: &T, planned: &T, same: impl Fn(&T, &T) -> bool) -> Option<PlanDifference> {
    (!same(existing, planned)).then(|| PlanDifference { field: field.into(), existing: show(existing), planned: show(planned) })
}

fn eq<T: PartialEq>(a: &T, b: &T) -> bool {
    a == b
}

/// Maps compare by the bit patterns of their floats, like the canonical encoding does.
fn same_axis_values(a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|((ka, va), (kb, vb))| ka == kb && va.to_bits() == vb.to_bits())
}

fn same_scores(a: &BTreeMap<String, pb::ScoreResult>, b: &BTreeMap<String, pb::ScoreResult>) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|((ka, sa), (kb, sb))| ka == kb && sa.name == sb.name && sa.value.to_bits() == sb.value.to_bits() && sa.unit == sb.unit && sa.passed == sb.passed)
}

impl StudyPlan {
    /// The plan of a study that is already finished (an import): every field is known, so the
    /// plan is satisfied only by a study with the same samples and provenance. The aggregates are
    /// not part of a plan; the final-content check at `prepare` covers them.
    pub fn exact(results: &pb::SweepResults) -> StudyPlan {
        StudyPlan {
            sweep_id: results.sweep_id.clone(),
            sweep_hash: results.sweep_hash.clone(),
            drm_hash: results.drm_hash.clone(),
            provenance: results.provenance.clone(),
            samples: results.samples.iter().cloned().map(PlannedSample::Fixed).collect(),
        }
    }

    /// The first difference between this plan and `existing`, or `None` when `existing` is a study
    /// this plan's run could reproduce (see the module doc for the order and the rules).
    pub fn first_difference(&self, existing: &pb::SweepResults) -> Option<PlanDifference> {
        differ("sweep_id", &existing.sweep_id, &self.sweep_id, eq)
            .or_else(|| differ("sweep_hash", &existing.sweep_hash, &self.sweep_hash, eq))
            .or_else(|| differ("drm_hash", &existing.drm_hash, &self.drm_hash, eq))
            .or_else(|| differ("provenance", &existing.provenance, &self.provenance, eq))
            .or_else(|| differ("samples (count)", &existing.samples.len(), &self.samples.len(), eq))
            .or_else(|| self.first_sample_difference(existing))
    }

    fn first_sample_difference(&self, existing: &pb::SweepResults) -> Option<PlanDifference> {
        let mut by_key: BTreeMap<(u32, u32), &pb::SweepSample> = BTreeMap::new();
        for s in &existing.samples {
            by_key.entry((s.point_index, s.draw_index)).or_insert(s);
        }
        let mut planned: Vec<&PlannedSample> = self.samples.iter().collect();
        planned.sort_by_key(|p| p.key());
        for p in planned {
            let (point, draw) = p.key();
            let at = format!("samples[p{point}_d{draw}]");
            let Some(found) = by_key.get(&(point, draw)) else {
                return Some(PlanDifference { field: at, existing: "absent".to_string(), planned: "a sample".to_string() });
            };
            let difference = match p {
                PlannedSample::Runs(run) => differ(format!("{at}.run_id"), &found.run_id, &run.run_id, eq)
                    .or_else(|| differ(format!("{at}.config_hash"), &found.config_hash, &run.config_hash, eq))
                    .or_else(|| differ(format!("{at}.axis_values"), &found.axis_values, &run.axis_values, same_axis_values))
                    .or_else(|| differ(format!("{at}.seeds"), &found.seeds, &run.seeds, eq))
                    .or_else(|| {
                        // A run that fails again has no products_uri; one that succeeds names its directory.
                        (!found.products_uri.is_empty() && found.products_uri != run.products_dir).then(|| PlanDifference {
                            field: format!("{at}.products_uri"),
                            existing: show(&found.products_uri),
                            planned: format!("{} (or empty if the run fails)", show(&run.products_dir)),
                        })
                    }),
                PlannedSample::Fixed(record) => differ(format!("{at}.run_id"), &found.run_id, &record.run_id, eq)
                    .or_else(|| differ(format!("{at}.config_hash"), &found.config_hash, &record.config_hash, eq))
                    .or_else(|| differ(format!("{at}.axis_values"), &found.axis_values, &record.axis_values, same_axis_values))
                    .or_else(|| differ(format!("{at}.seeds"), &found.seeds, &record.seeds, eq))
                    .or_else(|| differ(format!("{at}.scores"), &found.scores, &record.scores, same_scores))
                    .or_else(|| differ(format!("{at}.products_uri"), &found.products_uri, &record.products_uri, eq))
                    .or_else(|| differ(format!("{at}.error"), &found.error, &record.error, eq)),
            };
            if difference.is_some() {
                return difference;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(point: u32, draw: u32) -> PlannedRun {
        PlannedRun {
            point_index: point,
            draw_index: draw,
            run_id: format!("s_p{point}_d{draw}"),
            config_hash: format!("cfg{point}{draw}"),
            axis_values: BTreeMap::from([("a.b".to_string(), point as f64 * 0.5)]),
            seeds: BTreeMap::from([("k".to_string(), 10 + draw as u64)]),
            products_dir: format!("/out/sample_p{point}_d{draw}"),
        }
    }

    fn failed(point: u32, draw: u32) -> pb::SweepSample {
        pb::SweepSample { point_index: point, draw_index: draw, run_id: format!("s_p{point}_d{draw}"), error: "outside its declared bound".to_string(), ..Default::default() }
    }

    fn plan() -> StudyPlan {
        StudyPlan {
            sweep_id: "s".to_string(),
            sweep_hash: "sh".to_string(),
            drm_hash: "dh".to_string(),
            provenance: Some(pb::Provenance { tool: "av-sweep".to_string(), ..Default::default() }),
            samples: vec![PlannedSample::Runs(run(0, 0)), PlannedSample::Runs(run(0, 1)), PlannedSample::Fixed(failed(1, 0))],
        }
    }

    /// The study `plan()` would produce if every run succeeded.
    fn outcome(plan: &StudyPlan, succeed: bool) -> pb::SweepResults {
        let samples = plan
            .samples
            .iter()
            .map(|p| match p {
                PlannedSample::Runs(r) => pb::SweepSample {
                    point_index: r.point_index,
                    draw_index: r.draw_index,
                    run_id: r.run_id.clone(),
                    config_hash: r.config_hash.clone(),
                    axis_values: r.axis_values.clone(),
                    seeds: r.seeds.clone(),
                    products_uri: if succeed { r.products_dir.clone() } else { String::new() },
                    error: if succeed { String::new() } else { "child exited 7".to_string() },
                    ..Default::default()
                },
                PlannedSample::Fixed(s) => s.clone(),
            })
            .collect();
        pb::SweepResults { sweep_id: plan.sweep_id.clone(), sweep_hash: plan.sweep_hash.clone(), drm_hash: plan.drm_hash.clone(), samples, aggregates: Vec::new(), provenance: plan.provenance.clone() }
    }

    #[test]
    fn a_study_the_plan_can_reproduce_has_no_difference_whether_its_runs_succeeded_or_not() {
        let plan = plan();
        assert_eq!(plan.first_difference(&outcome(&plan, true)), None);
        assert_eq!(plan.first_difference(&outcome(&plan, false)), None);
        // Scores and aggregates are not known up front: they are not compared.
        let mut with_scores = outcome(&plan, true);
        with_scores.samples[0].scores.insert("m".to_string(), pb::ScoreResult { name: "m".to_string(), value: 1.0, ..Default::default() });
        with_scores.aggregates.push(pb::ScoreAggregate { name: "m".to_string(), ..Default::default() });
        assert_eq!(plan.first_difference(&with_scores), None);
        // Nor is the order samples are stored in.
        let mut shuffled = outcome(&plan, true);
        shuffled.samples.reverse();
        assert_eq!(plan.first_difference(&shuffled), None);
    }

    #[test]
    fn each_known_field_that_differs_is_named_with_both_values() {
        let plan = plan();
        let base = outcome(&plan, true);
        type Mutation = Box<dyn Fn(&mut pb::SweepResults)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("sweep_hash", Box::new(|s| s.sweep_hash = "other".to_string())),
            ("drm_hash", Box::new(|s| s.drm_hash = "other".to_string())),
            ("provenance", Box::new(|s| s.provenance = None)),
            ("samples (count)", Box::new(|s| drop(s.samples.pop()))),
            ("samples[p0_d1].run_id", Box::new(|s| s.samples[1].run_id = "other".to_string())),
            ("samples[p0_d0].config_hash", Box::new(|s| s.samples[0].config_hash = "other".to_string())),
            ("samples[p0_d0].axis_values", Box::new(|s| {
                s.samples[0].axis_values.insert("a.b".to_string(), 99.0);
            })),
            ("samples[p0_d1].seeds", Box::new(|s| {
                s.samples[1].seeds.insert("k".to_string(), 99);
            })),
            ("samples[p0_d0].products_uri", Box::new(|s| s.samples[0].products_uri = "/elsewhere/sample_p0_d0".to_string())),
            ("samples[p1_d0].error", Box::new(|s| s.samples[2].error = "another error".to_string())),
            ("samples[p1_d0].products_uri", Box::new(|s| s.samples[2].products_uri = "/out/sample_p1_d0".to_string())),
        ];
        for (field, mutate) in cases {
            let mut existing = base.clone();
            mutate(&mut existing);
            let d = plan.first_difference(&existing).unwrap_or_else(|| panic!("{field}: no difference found"));
            assert_eq!(d.field, field, "{d:?}");
            assert_ne!(d.existing, d.planned, "{field}");
        }
        // A sample absent from the existing study (same count, different grid point).
        let mut moved = base.clone();
        moved.samples[1].draw_index = 7;
        let d = plan.first_difference(&moved).unwrap();
        assert_eq!((d.field.as_str(), d.existing.as_str()), ("samples[p0_d1]", "absent"));
    }

    #[test]
    fn the_first_differing_field_wins_and_floats_compare_by_bits() {
        let plan = plan();
        let mut existing = outcome(&plan, true);
        existing.drm_hash = "x".to_string();
        existing.samples[0].run_id = "y".to_string();
        assert_eq!(plan.first_difference(&existing).unwrap().field, "drm_hash");

        let mut nan_plan = plan.clone();
        let PlannedSample::Runs(r) = &mut nan_plan.samples[0] else { unreachable!() };
        r.axis_values.insert("a.b".to_string(), f64::NAN);
        assert_eq!(nan_plan.first_difference(&outcome(&nan_plan, true)), None, "the same NaN is the same value");
        let mut other_nan = outcome(&nan_plan, true);
        other_nan.samples[0].axis_values.insert("a.b".to_string(), f64::from_bits(f64::NAN.to_bits() ^ 1));
        assert_eq!(nan_plan.first_difference(&other_nan).unwrap().field, "samples[p0_d0].axis_values");
    }

    /// An import knows everything: its exact plan accepts the same study (NaN scores included) and
    /// names the first score or sample field of any other.
    #[test]
    fn the_exact_plan_of_a_finished_study_accepts_only_that_study() {
        let mut study = outcome(&plan(), true);
        study.samples[0].scores.insert("m".to_string(), pb::ScoreResult { name: "m".to_string(), value: f64::NAN, ..Default::default() });
        let exact = StudyPlan::exact(&study);
        assert_eq!(exact.first_difference(&study), None, "identical, a NaN score included");
        let mut changed = study.clone();
        changed.samples[0].scores.get_mut("m").unwrap().value = 1.0;
        assert_eq!(exact.first_difference(&changed).unwrap().field, "samples[p0_d0].scores");
        let mut other_run = study.clone();
        other_run.samples[1].products_uri = "/elsewhere".to_string();
        assert_eq!(exact.first_difference(&other_run).unwrap().field, "samples[p0_d1].products_uri");
    }

    #[test]
    fn long_values_are_cut_in_the_message() {
        let plan = plan();
        let mut existing = outcome(&plan, true);
        existing.samples[0].seeds = (0..200).map(|n| (format!("key{n}"), n as u64)).collect();
        let d = plan.first_difference(&existing).unwrap();
        assert_eq!(d.field, "samples[p0_d0].seeds");
        assert!(d.existing.ends_with("...") && d.existing.chars().count() == 163, "{}", d.existing);
    }
}
