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
/// `av_kernel::drm::schema`.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawSweepAxis {
    pub instance: String,
    pub parameter: String,
    pub values: Vec<f64>,
    pub min: f64,
    pub max: f64,
    pub steps: u32,
}
impl RawSweepAxis {
    fn into_pb(self) -> pb::SweepAxis {
        pb::SweepAxis { instance: self.instance, parameter: self.parameter, values: self.values, min: self.min, max: self.max, steps: self.steps }
    }
}

/// `ParameterSweep` (`proto/altavista/v1/system.proto`), field-for-field.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawParameterSweep {
    pub id: String,
    pub drm_id: String,
    pub axes: Vec<RawSweepAxis>,
    pub monte_carlo_draws: u32,
    pub provenance: Option<RawProvenance>,
    pub hash: String,
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
        })
    }
}

/// Parse a YAML document as a [`pb::ParameterSweep`] (see the module doc comment for the
/// authoring-format rationale). Parse only -- no hash verification, mirroring
/// `av_kernel::drm::schema::parse_drm_yaml`; call [`crate::hash::verify_sweep_hash`] separately.
pub fn parse_sweep_yaml(yaml: &str) -> Result<pb::ParameterSweep, SweepError> {
    let raw: RawParameterSweep = serde_yaml::from_str(yaml).map_err(|e| SweepError::Yaml(e.to_string()))?;
    raw.into_pb()
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
}
