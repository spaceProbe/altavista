//! The `ParameterSweep` YAML authoring format (F1a, `docs/feasibility-plan.md`): field-for-field
//! mirroring `proto/altavista/v1/system.proto`'s `ParameterSweep`/`SweepAxis`, converted into
//! the real `av_cdm::pb` types before anything downstream (hashing, grid expansion) touches it --
//! the exact same pattern `av_kernel::drm::schema` uses for `DesignReferenceMission`/
//! `SosConfiguration`/`SystemDefinition` (see that module's own doc comment for the full
//! rationale: `av_cdm::pb` types have no `serde` derive, so a `Raw*` struct with
//! `#[serde(deny_unknown_fields, default)]` is this platform's honest, once-written-by-hand
//! transcription of the wire shape, never a bespoke vocabulary).
//!
//! ## Reusing `av_kernel::drm::schema::RawProvenance` rather than duplicating it
//!
//! `ParameterSweep.provenance` is a `Provenance` (`proto/altavista/v1/envelope.proto`), the
//! identical message every other artifact in this platform carries. `ParameterSweep` itself has
//! no `label` field (unlike `DesignReferenceMission`/`SosConfiguration`/`SystemDefinition`, each
//! of which does) -- see `proto/altavista/v1/system.proto`'s own `ParameterSweep` message, six
//! fields, `label` not among them -- so only `RawProvenance` is needed here, not `RawLabel` too.
//! `av_kernel::drm::schema` already declares `RawProvenance` for exactly this shape, and both
//! the struct and its fields are `pub` -- so this module reuses that *type* directly (`use
//! av_kernel::drm::schema::RawProvenance;`) rather than re-declaring the same field list a
//! second time, satisfying this task's "prefer reuse over duplication" instruction as far as it
//! can be honoured.
//!
//! It stops short of full reuse for one reason: `RawProvenance::into_pb` is **not** `pub` on
//! `av_kernel::drm::schema` (only the struct type itself is) -- `crates/av-kernel/src/drm/
//! schema.rs`'s own `impl RawProvenance { fn into_pb(...) }` block has no `pub` keyword, so
//! `av-kernel`'s current `pub` surface does not actually let another crate finish that
//! conversion. This task's hard boundary forbids editing `av-kernel` to add one
//! (`crates/av-kernel/**` is not in this task's editable set). So [`provenance_into_pb`] below
//! is a small, local, honest re-transcription of exactly what `RawProvenance::into_pb` already
//! does (there is nothing to invent -- every field copies straight across, `author_kind`
//! matched against `AuthorKind::from_str_name` the same way every enum field in
//! `av_kernel::drm::schema` is) -- this crate's own equivalent of
//! `av_kernel::drm::schema::enum_from_name`, not a new policy.

use av_cdm::pb;
use av_kernel::drm::schema::RawProvenance;
use serde::Deserialize;

use crate::error::SweepError;

fn provenance_into_pb(p: RawProvenance) -> Result<pb::Provenance, SweepError> {
    let author_kind = if p.author_kind.is_empty() {
        pb::AuthorKind::Unspecified as i32
    } else {
        pb::AuthorKind::from_str_name(&p.author_kind)
            .map(|e| e as i32)
            .ok_or_else(|| SweepError::InvalidEnumValue { field: "provenance.author_kind", value: p.author_kind.clone() })?
    };
    Ok(pb::Provenance {
        author_kind,
        principal: p.principal,
        tool: p.tool,
        config_hash: p.config_hash,
        data_pack_hash: p.data_pack_hash,
        dataset_hash: p.dataset_hash,
        created_tai_ns: p.created_tai_ns,
        run_id: p.run_id,
        attributes: p.attributes,
    })
}

/// `SweepAxis` (`proto/altavista/v1/system.proto`), field-for-field. Grid-expansion semantics
/// (ambiguous declarations, step counts, ordering) live in [`crate::grid`], not here -- this
/// struct's own job stops at an honest transcription, exactly like every `Raw*` struct in
/// `av_kernel::drm::schema`. Question 192(c) adds `event_id`/`value_key` (an axis targets EITHER
/// an instance parameter OR a scenario event value; see `crate::grid`'s own module doc comment
/// for the "exactly one target" rule and the key-collision analysis) -- [`parse_sweep_yaml`]
/// enforces that rule via `crate::grid::validate_axis_target`, not this struct.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawSweepAxis {
    pub instance: String,
    pub parameter: String,
    pub values: Vec<f64>,
    pub min: f64,
    pub max: f64,
    pub steps: u32,
    pub event_id: String,
    pub value_key: String,
}
impl RawSweepAxis {
    fn into_pb(self) -> pb::SweepAxis {
        pb::SweepAxis {
            instance: self.instance,
            parameter: self.parameter,
            values: self.values,
            min: self.min,
            max: self.max,
            steps: self.steps,
            event_id: self.event_id,
            value_key: self.value_key,
        }
    }
}

/// `ParameterSweep` (`proto/altavista/v1/system.proto`), field-for-field. Question 192(d) adds
/// `dispersed` -- see `pb::ParameterSweep.dispersed`'s own proto field comment and
/// `src/bin/av-sweep/study.rs::run_study`'s error-mode selection for what it changes.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawParameterSweep {
    pub id: String,
    pub drm_id: String,
    pub axes: Vec<RawSweepAxis>,
    pub monte_carlo_draws: u32,
    pub provenance: Option<RawProvenance>,
    pub hash: String,
    pub dispersed: bool,
}
impl RawParameterSweep {
    pub fn into_pb(self) -> Result<pb::ParameterSweep, SweepError> {
        Ok(pb::ParameterSweep {
            id: self.id,
            drm_id: self.drm_id,
            axes: self.axes.into_iter().map(RawSweepAxis::into_pb).collect(),
            monte_carlo_draws: self.monte_carlo_draws,
            provenance: self.provenance.map(provenance_into_pb).transpose()?,
            hash: self.hash,
            dispersed: self.dispersed,
        })
    }
}

/// Parse a YAML document as a [`pb::ParameterSweep`] (see the module doc comment for the
/// authoring-format rationale). Parse only -- no hash verification, mirroring
/// `av_kernel::drm::schema::parse_drm_yaml`; call [`crate::hash::verify_sweep_hash`] separately.
///
/// Also enforces question 192(c)'s "exactly one target per axis" rule (`crate::grid::
/// validate_axis_target`, run over every declared axis) -- this is what makes that rule refused
/// "at load", per this crate's own task brief, for a YAML-authored sweep; [`crate::grid::
/// expand_grid`] runs the identical check again so a programmatically-built `pb::ParameterSweep`
/// that never went through this function cannot bypass it either (see that function's own doc
/// comment for why this is deliberately one function, not two copies).
pub fn parse_sweep_yaml(yaml: &str) -> Result<pb::ParameterSweep, SweepError> {
    let raw: RawParameterSweep = serde_yaml::from_str(yaml).map_err(|e| SweepError::Yaml(e.to_string()))?;
    let sweep = raw.into_pb()?;
    for axis in &sweep.axes {
        crate::grid::validate_axis_target(axis)?;
    }
    Ok(sweep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_sweep_and_refuses_an_unknown_field() {
        let yaml = r#"
id: sweep_test
drm_id: demo_two_instance_drm
axes:
  - instance: demo_flt
    parameter: spacecraft.Cd
    values: [2.0, 2.2, 2.4]
monte_carlo_draws: 1
provenance:
  author_kind: AUTHOR_KIND_AGENT
  tool: "test"
hash: ""
"#;
        let sweep = parse_sweep_yaml(yaml).expect("parses");
        assert_eq!(sweep.id, "sweep_test");
        assert_eq!(sweep.drm_id, "demo_two_instance_drm");
        assert_eq!(sweep.axes.len(), 1);
        assert_eq!(sweep.axes[0].instance, "demo_flt");
        assert_eq!(sweep.axes[0].values, vec![2.0, 2.2, 2.4]);
        assert_eq!(sweep.monte_carlo_draws, 1);
        assert_eq!(sweep.provenance.unwrap().author_kind, pb::AuthorKind::Agent as i32);

        let bad_yaml = r#"
id: sweep_test
not_a_real_field: true
hash: ""
"#;
        let err = parse_sweep_yaml(bad_yaml).unwrap_err();
        assert!(matches!(err, SweepError::Yaml(ref msg) if msg.contains("not_a_real_field")), "{err:?}");
    }

    /// Question 192(c): an event axis (`event_id` + `value_key`, no `instance`/`parameter`)
    /// parses, and question 192(d): `dispersed: true` parses too.
    #[test]
    fn parses_an_event_axis_and_the_dispersed_flag() {
        let yaml = r#"
id: sweep_test
drm_id: demo_two_instance_drm
axes:
  - event_id: burn1
    value_key: dv_x
    values: [15.0, 25.0]
monte_carlo_draws: 1
dispersed: true
hash: ""
"#;
        let sweep = parse_sweep_yaml(yaml).expect("parses");
        assert_eq!(sweep.axes[0].event_id, "burn1");
        assert_eq!(sweep.axes[0].value_key, "dv_x");
        assert_eq!(sweep.axes[0].instance, "", "an event axis leaves instance at its zero default");
        assert!(sweep.dispersed);
    }

    /// Question 192(c)'s "exactly one target per axis" rule is enforced "at load" -- a YAML axis
    /// declaring neither target is refused by [`parse_sweep_yaml`] itself, not merely by a later
    /// call to `crate::grid::expand_grid`. Fails against an implementation that forgot to call
    /// `crate::grid::validate_axis_target` from here (only from `expand_grid`): such an
    /// implementation would return `Ok` here, and this test's `unwrap_err()` would panic.
    #[test]
    fn refuses_an_axis_with_no_target_at_load() {
        let yaml = r#"
id: sweep_test
drm_id: demo_two_instance_drm
axes:
  - values: [1.0]
monte_carlo_draws: 1
hash: ""
"#;
        let err = parse_sweep_yaml(yaml).unwrap_err();
        assert!(matches!(err, SweepError::AxisMissingTarget { .. }), "{err:?}");
    }
}
