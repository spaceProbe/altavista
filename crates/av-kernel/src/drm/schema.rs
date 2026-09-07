//! The DRM executor's authoring format: YAML, field-for-field mirroring
//! `proto/altavista/v1/system.proto` (and the `core.proto`/`envelope.proto` messages it
//! embeds), converted into the real `av_cdm::pb` types before anything downstream (hashing,
//! binding, running) ever touches it.
//!
//! **Why YAML, and why these particular `Raw*` structs (question 87's "authoring format" --
//! see `drms/README.md` for the fuller writeup).** `av_cdm::pb` types are plain
//! `prost::Message` structs with no `serde` derive (`crates/av-cdm/build.rs` does not ask
//! `prost-build` for one, and this task does not touch that build script -- it is shared by
//! every other crate depending on `av-cdm`). So a DRM author's file cannot be a `pb` type
//! deserialized directly; instead every struct in this module has exactly the same fields,
//! names and nesting as its `.proto` counterpart (this *is* the honest transcription the task
//! asks for, just written by hand once rather than derived), and `into_pb()` copies each
//! field across with no invented defaults beyond proto3's own zero values and no field ever
//! silently dropped. Enum fields are written as the proto enum's own name (e.g.
//! `"BINDING_KIND_MODEL"`) and parsed with the generated `from_str_name` -- the same string
//! form protobuf's own canonical JSON mapping uses, not a bespoke vocabulary.
//!
//! **What this loader does not yet model.** `Variant` (`SystemDefinition`) is read as opaque
//! YAML (`serde_yaml::Value`) rather than fully typed -- `Variant` (named parameter-override
//! sets) has no consumer yet. Rather than silently dropping whatever a DRM author put there
//! (exactly what question 87 exists to end), a non-empty `variants` list is a typed
//! [`crate::drm::DrmError::UnsupportedField`] at load time: the loader refuses to proceed
//! rather than hash and run a DRM that declared something it cannot honour.
//!
//! **`Scenario.frames` (question 124, M18.1).** Unlike `Variant` above, `frames` is now fully
//! typed -- [`RawFrameDefinition`] mirrors `FrameDefinition` (`core.proto`) field-for-field,
//! including its `origin` oneof (`body`/`platform_id`/`entity_id`) and the nested
//! `origin_geodetic`/`attitude_source` messages, matched by [`RawGeodetic`]/
//! [`RawAttitudeSource`]. A declared frame is realized through the same `FrameRegistry` path
//! the Python scenario uses (`crate::drm::executor::collect_frames`/`registry_default_frame`) --
//! this loader's own job stops at an honest transcription, same as every other typed field in
//! this module. Question 124 was decided by the lead with no ADR change and no proto change:
//! `Scenario.frames` already existed on the wire (`core.proto`), only this loader's own
//! placeholder refusal is removed.
//!
//! **`SystemDefinition.ports` (`docs/open-questions.md` question 108, M13.1).** Unlike
//! `Variant`/`FrameDefinition` above, `ports` is now fully typed: [`RawPort`]/[`RawPortTiming`]
//! mirror `Port`/`PortTiming` field-for-field, and `PortKind`/`PortDirection` are matched by
//! their proto enum names exactly like every other enum field in this module (`enum_from_name`).
//! Loading a `Port` does not by itself validate it against anything else -- that is
//! [`crate::router::Router::build`]'s job, once a `SosConfiguration.connections` entry actually
//! references it (an undeclared port, a direction mismatch, a kind mismatch, or an unsupported
//! `link_model` -- see that module's own doc comment for exactly what "phase one" means there).
//!
//! **`SystemDefinition.packet_codecs` (question 149, M22.3).** Typed the same way `ports` is:
//! [`RawPacketCodec`]/[`RawPacketField`] mirror `PacketCodec`/`PacketField` (`packet.proto`)
//! field-for-field, `type`/`unit` matched by their proto enum names like every other enum
//! field here. Unlike a bare transcription, though, converting the list also runs
//! `crate::codec::validate_system_packet_codecs` (a field extent past `user_data_bytes`, a
//! `bit_width` disagreeing with a fixed-width type, or a duplicate `apid` is a typed load
//! error here, not left for a runtime surprise) -- see that function's own doc comment and
//! [`packet_codecs`] below.
//!
//! **`Scenario.events` (question 97).** Unlike `frames`, `events` is now fully typed --
//! [`RawScenarioEvent`] mirrors `ScenarioEvent` field-for-field, and every event is additionally
//! run through [`super::maneuver::parse`] at load time (the same typed check
//! [`super::executor::execute`] applies again at run time -- see that module's own doc comment):
//! a `kind` other than `"maneuver"`, or a `"maneuver"` event whose `values`/`attributes` do not
//! match the declared contract, is a typed load-time error, never a silently-accepted opaque
//! blob.

use std::collections::BTreeMap;

use av_cdm::pb;
use serde::Deserialize;

use super::DrmError;

/// Parse a proto enum's own name (e.g. `"BINDING_KIND_MODEL"`, from `from_str_name`, the
/// same generated method protobuf's own canonical JSON mapping would use) into the `i32`
/// wire value every `av_cdm::pb` field actually stores. `prost`'s `Enumeration` derive gives
/// every enum `from_str_name`/`as_str_name` but no generic `From<E> for i32` (only a bare `as
/// i32` cast on the concrete type), so `to_i32` is a per-call-site closure (`|e| e as i32`)
/// rather than a trait bound -- monomorphized per enum type, not a real indirection.
fn enum_from_name<E>(field: &'static str, name: &str, from_str_name: impl Fn(&str) -> Option<E>, to_i32: impl Fn(E) -> i32) -> Result<i32, DrmError> {
    from_str_name(name).map(to_i32).ok_or_else(|| DrmError::InvalidEnumValue { field, value: name.to_string() })
}

fn refuse_if_nonempty(field: &'static str, values: &[serde_yaml::Value]) -> Result<(), DrmError> {
    if values.is_empty() {
        Ok(())
    } else {
        Err(DrmError::UnsupportedField { field })
    }
}

// --------------------------------------------------------------------------------------
// Label / Provenance (core.proto / envelope.proto)
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawLabel {
    pub marking: String,
    pub caveats: Vec<String>,
}
impl RawLabel {
    fn into_pb(self) -> pb::Label {
        pb::Label { marking: self.marking, caveats: self.caveats }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawProvenance {
    pub author_kind: String,
    pub principal: String,
    pub tool: String,
    pub config_hash: String,
    pub data_pack_hash: String,
    pub dataset_hash: String,
    pub created_tai_ns: i64,
    pub run_id: String,
    pub attributes: BTreeMap<String, String>,
}
impl RawProvenance {
    fn into_pb(self) -> Result<pb::Provenance, DrmError> {
        let author_kind = if self.author_kind.is_empty() {
            pb::AuthorKind::Unspecified as i32
        } else {
            enum_from_name("provenance.author_kind", &self.author_kind, pb::AuthorKind::from_str_name, |e| e as i32)?
        };
        Ok(pb::Provenance {
            author_kind,
            principal: self.principal,
            tool: self.tool,
            config_hash: self.config_hash,
            data_pack_hash: self.data_pack_hash,
            dataset_hash: self.dataset_hash,
            created_tai_ns: self.created_tai_ns,
            run_id: self.run_id,
            attributes: self.attributes,
        })
    }
}

fn opt_label(l: Option<RawLabel>) -> Option<pb::Label> {
    l.map(RawLabel::into_pb)
}
fn opt_provenance(p: Option<RawProvenance>) -> Result<Option<pb::Provenance>, DrmError> {
    p.map(RawProvenance::into_pb).transpose()
}

// --------------------------------------------------------------------------------------
// Parameter (system.proto)
// --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawParameter {
    pub name: String,
    pub unit: String,
    pub value: f64,
    pub string_value: String,
    pub min: f64,
    pub max: f64,
    pub description: String,
}
impl RawParameter {
    fn into_pb(self) -> Result<pb::Parameter, DrmError> {
        let unit = if self.unit.is_empty() { pb::Unit::Unspecified as i32 } else { enum_from_name("parameter.unit", &self.unit, pb::Unit::from_str_name, |e| e as i32)? };
        Ok(pb::Parameter { name: self.name, unit, value: self.value, string_value: self.string_value, min: self.min, max: self.max, description: self.description })
    }
}
fn parameters(v: Vec<RawParameter>) -> Result<Vec<pb::Parameter>, DrmError> {
    v.into_iter().map(RawParameter::into_pb).collect()
}

// --------------------------------------------------------------------------------------
// StateSpace (question 94: SystemDefinition.state_space, core.proto's StateSpace/
// StateComponent messages). See crates/av-kernel/src/trajectory.rs::resolve_state_space
// for what a declared state_space means once it is parsed: authoritative over
// state_space_id (id equality checked), every component checked against ADR-005 sec 3's
// interpolation classes. This loader's own job stops at an honest transcription -- neither
// check happens here.
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawStateComponent {
    pub label: String,
    pub unit: String,
}
impl RawStateComponent {
    fn into_pb(self) -> Result<pb::StateComponent, DrmError> {
        let unit = if self.unit.is_empty() { pb::Unit::Unspecified as i32 } else { enum_from_name("state_space.components[].unit", &self.unit, pb::Unit::from_str_name, |e| e as i32)? };
        Ok(pb::StateComponent { label: self.label, unit })
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawStateSpace {
    pub id: String,
    pub components: Vec<RawStateComponent>,
    pub frame_id: String,
}
impl RawStateSpace {
    fn into_pb(self) -> Result<pb::StateSpace, DrmError> {
        let components = self.components.into_iter().map(RawStateComponent::into_pb).collect::<Result<Vec<_>, _>>()?;
        Ok(pb::StateSpace { id: self.id, components, frame_id: self.frame_id })
    }
}

// --------------------------------------------------------------------------------------
// Port / PortTiming (system.proto, question 108)
// --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawPortTiming {
    pub rate_hz: f64,
    pub latency_ns: i64,
    pub jitter_ns: i64,
    pub deterministic: bool,
}
impl RawPortTiming {
    fn into_pb(self) -> pb::PortTiming {
        pb::PortTiming { rate_hz: self.rate_hz, latency_ns: self.latency_ns, jitter_ns: self.jitter_ns, deterministic: self.deterministic }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawPort {
    pub name: String,
    pub kind: String,
    pub direction: String,
    pub schema: String,
    pub timing: Option<RawPortTiming>,
    pub interface_class: String,
}
impl RawPort {
    fn into_pb(self) -> Result<pb::Port, DrmError> {
        let kind = if self.kind.is_empty() { pb::PortKind::Unspecified as i32 } else { enum_from_name("port.kind", &self.kind, pb::PortKind::from_str_name, |e| e as i32)? };
        let direction =
            if self.direction.is_empty() { pb::PortDirection::Unspecified as i32 } else { enum_from_name("port.direction", &self.direction, pb::PortDirection::from_str_name, |e| e as i32)? };
        Ok(pb::Port { name: self.name, kind, direction, schema: self.schema, timing: self.timing.map(RawPortTiming::into_pb), interface_class: self.interface_class })
    }
}
fn ports(v: Vec<RawPort>) -> Result<Vec<pb::Port>, DrmError> {
    v.into_iter().map(RawPort::into_pb).collect()
}

// --------------------------------------------------------------------------------------
// PacketCodec / PacketField (packet.proto, question 149, M22.3)
// --------------------------------------------------------------------------------------

/// `PacketField` (`packet.proto`), field-for-field. `type` is written as `#[serde(rename =
/// "type")]` (an explicit rename rather than relying on `r#type`'s raw-identifier stripping to
/// happen to match `serde`'s default field-name inference) so a DRM author writes the same
/// `type:` key the proto field is actually named, exactly like every other enum field in this
/// module (`enum_from_name`, matched against `PacketFieldType::from_str_name`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawPacketField {
    pub name: String,
    pub bit_offset: u32,
    pub bit_width: u32,
    #[serde(rename = "type")]
    pub r#type: String,
    pub unit: String,
    pub scale: f64,
    pub offset: f64,
    pub target: String,
}
impl RawPacketField {
    fn into_pb(self) -> Result<pb::PacketField, DrmError> {
        let r#type =
            if self.r#type.is_empty() { pb::PacketFieldType::Unspecified as i32 } else { enum_from_name("packet_field.type", &self.r#type, pb::PacketFieldType::from_str_name, |e| e as i32)? };
        let unit = if self.unit.is_empty() { pb::Unit::Unspecified as i32 } else { enum_from_name("packet_field.unit", &self.unit, pb::Unit::from_str_name, |e| e as i32)? };
        Ok(pb::PacketField { name: self.name, bit_offset: self.bit_offset, bit_width: self.bit_width, r#type, unit, scale: self.scale, offset: self.offset, target: self.target })
    }
}

/// `PacketCodec` (`packet.proto`), field-for-field -- question 149's typed (not opaque)
/// `SystemDefinition.packet_codecs`. Unlike every other `Raw*` struct's `into_pb` in this
/// module, converting a *list* of these (see [`packet_codecs`] below) also runs
/// `crate::codec::validate_system_packet_codecs` -- the same load-time field-extent,
/// fixed-width and duplicate-`apid` checks that function's own doc comment describes, shared
/// with any future runtime decode path so the two can never quietly disagree about what a
/// valid codec set is.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawPacketCodec {
    pub id: String,
    pub apid: u32,
    pub is_command: bool,
    pub secondary_header_bytes: u32,
    pub fields: Vec<RawPacketField>,
    pub user_data_bytes: u32,
    pub description: String,
}
impl RawPacketCodec {
    fn into_pb(self) -> Result<pb::PacketCodec, DrmError> {
        let fields = self.fields.into_iter().map(RawPacketField::into_pb).collect::<Result<Vec<_>, _>>()?;
        Ok(pb::PacketCodec { id: self.id, apid: self.apid, is_command: self.is_command, secondary_header_bytes: self.secondary_header_bytes, fields, user_data_bytes: self.user_data_bytes, description: self.description })
    }
}
fn packet_codecs(v: Vec<RawPacketCodec>) -> Result<Vec<pb::PacketCodec>, DrmError> {
    let codecs = v.into_iter().map(RawPacketCodec::into_pb).collect::<Result<Vec<_>, _>>()?;
    // Question 149: a field extent past user_data_bytes, a bit_width disagreeing with a
    // fixed-width type, or a duplicate apid within this SystemDefinition is a typed load
    // error, not a runtime surprise -- see crate::codec::validate_system_packet_codecs's own
    // doc comment. The resulting ApidMap itself is discarded here (this loader's own job stops
    // at an honest, validated transcription, same as every other typed field in this module --
    // e.g. RawFrameDefinition above); a future runtime decode path rebuilds it from
    // pb::SystemDefinition.packet_codecs via the same function.
    crate::codec::validate_system_packet_codecs(&codecs).map_err(DrmError::Codec)?;
    Ok(codecs)
}

// --------------------------------------------------------------------------------------
// SystemDefinition
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawSystemDefinition {
    pub id: String,
    pub version: String,
    pub name: String,
    pub subsystem_ids: Vec<String>,
    /// Typed as of question 108 -- see the module doc comment and [`RawPort`].
    pub ports: Vec<RawPort>,
    pub parameters: Vec<RawParameter>,
    /// Not yet modeled -- see the module doc comment. Must be empty.
    pub variants: Vec<serde_yaml::Value>,
    pub dynamics_model: String,
    pub state_space_id: String,
    /// Question 94: the declared StateSpace, additive to and authoritative over
    /// `state_space_id` when present (`crate::trajectory::resolve_state_space` is where
    /// that authority and the ADR-005 sec 3 component check are enforced -- this loader only
    /// transcribes the field). Absent (`None`) means "resolve `state_space_id` against the
    /// kernel's built-in registry", exactly as before this field existed.
    pub state_space: Option<RawStateSpace>,
    /// Typed as of question 149 (M22.3) -- see the module doc comment and [`RawPacketCodec`].
    pub packet_codecs: Vec<RawPacketCodec>,
    pub sensor_models: Vec<String>,
    pub actuator_models: Vec<String>,
    pub label: Option<RawLabel>,
    pub provenance: Option<RawProvenance>,
    pub hash: String,
}
impl RawSystemDefinition {
    pub fn into_pb(self) -> Result<pb::SystemDefinition, DrmError> {
        refuse_if_nonempty("SystemDefinition.variants", &self.variants)?;
        Ok(pb::SystemDefinition {
            id: self.id,
            version: self.version,
            name: self.name,
            subsystem_ids: self.subsystem_ids,
            ports: ports(self.ports)?,
            parameters: parameters(self.parameters)?,
            variants: vec![],
            dynamics_model: self.dynamics_model,
            state_space_id: self.state_space_id,
            state_space: self.state_space.map(RawStateSpace::into_pb).transpose()?,
            packet_codecs: packet_codecs(self.packet_codecs)?,
            sensor_models: self.sensor_models,
            actuator_models: self.actuator_models,
            label: opt_label(self.label),
            provenance: opt_provenance(self.provenance)?,
            hash: self.hash,
        })
    }
}

// --------------------------------------------------------------------------------------
// Bindings
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawModelBinding {
    pub model_id: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawContainerBinding {
    pub image: String,
    pub image_digest: String,
    pub command: Vec<String>,
    pub port_endpoints: BTreeMap<String, String>,
    pub lockstep_capable: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawRenodeBinding {
    pub platform: String,
    pub binary_uri: String,
    pub binary_sha256: String,
    pub script: String,
    pub port_bridges: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawBoardBinding {
    pub edge_node_id: String,
    pub port_devices: BTreeMap<String, String>,
    pub power_control: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawBinding {
    pub kind: String,
    #[serde(default)]
    pub model: Option<RawModelBinding>,
    #[serde(default)]
    pub container: Option<RawContainerBinding>,
    #[serde(default)]
    pub renode: Option<RawRenodeBinding>,
    #[serde(default)]
    pub board: Option<RawBoardBinding>,
}
impl RawBinding {
    fn into_pb(self) -> Result<pb::Binding, DrmError> {
        let kind = enum_from_name("binding.kind", &self.kind, pb::BindingKind::from_str_name, |e| e as i32)?;
        let config = match (self.model, self.container, self.renode, self.board) {
            (Some(m), None, None, None) => Some(pb::binding::Config::Model(pb::ModelBinding { model_id: m.model_id })),
            (None, Some(c), None, None) => Some(pb::binding::Config::Container(pb::ContainerBinding {
                image: c.image,
                image_digest: c.image_digest,
                command: c.command,
                port_endpoints: c.port_endpoints,
                lockstep_capable: c.lockstep_capable,
            })),
            (None, None, Some(r), None) => Some(pb::binding::Config::Renode(pb::RenodeBinding {
                platform: r.platform,
                binary_uri: r.binary_uri,
                binary_sha256: r.binary_sha256,
                script: r.script,
                port_bridges: r.port_bridges,
            })),
            (None, None, None, Some(b)) => {
                Some(pb::binding::Config::Board(pb::BoardBinding { edge_node_id: b.edge_node_id, port_devices: b.port_devices, power_control: b.power_control }))
            }
            (None, None, None, None) => None,
            _ => return Err(DrmError::InvalidBinding { reason: "at most one of model/container/renode/board may be set (oneof config)".to_string() }),
        };
        Ok(pb::Binding { kind, config })
    }
}

// --------------------------------------------------------------------------------------
// SosConfiguration
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawConnection {
    pub from_instance: String,
    pub from_port: String,
    pub to_instance: String,
    pub to_port: String,
    pub link_model: String,
}
impl RawConnection {
    fn into_pb(self) -> pb::Connection {
        pb::Connection { from_instance: self.from_instance, from_port: self.from_port, to_instance: self.to_instance, to_port: self.to_port, link_model: self.link_model }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawSystemInstance {
    pub name: String,
    pub system_id: String,
    pub variant: String,
    pub binding: Option<RawBinding>,
    pub step_rate_hz: f64,
    pub entity_id: String,
    pub parameter_overrides: Vec<RawParameter>,
    /// Question 89: row-major n x n covariance (SI) seeding this instance when the DRM
    /// requests covariance. Empty = none. The executor reads this field directly
    /// (`executor::load_initial_covariance`) and SPD-checks it at load; the former
    /// `covariance.p0_row_major` parameter-string convention has been removed entirely.
    pub initial_covariance: Vec<f64>,
}
impl RawSystemInstance {
    fn into_pb(self) -> Result<pb::SystemInstance, DrmError> {
        Ok(pb::SystemInstance {
            name: self.name,
            system_id: self.system_id,
            variant: self.variant,
            binding: self.binding.map(RawBinding::into_pb).transpose()?,
            step_rate_hz: self.step_rate_hz,
            entity_id: self.entity_id,
            parameter_overrides: parameters(self.parameter_overrides)?,
            initial_covariance: self.initial_covariance,
        })
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawSosConfiguration {
    pub id: String,
    pub version: String,
    pub name: String,
    pub instances: Vec<RawSystemInstance>,
    pub connections: Vec<RawConnection>,
    pub label: Option<RawLabel>,
    pub provenance: Option<RawProvenance>,
    pub hash: String,
}
impl RawSosConfiguration {
    pub fn into_pb(self) -> Result<pb::SosConfiguration, DrmError> {
        let instances = self.instances.into_iter().map(RawSystemInstance::into_pb).collect::<Result<Vec<_>, _>>()?;
        Ok(pb::SosConfiguration {
            id: self.id,
            version: self.version,
            name: self.name,
            instances,
            connections: self.connections.into_iter().map(RawConnection::into_pb).collect(),
            label: opt_label(self.label),
            provenance: opt_provenance(self.provenance)?,
            hash: self.hash,
        })
    }
}

// --------------------------------------------------------------------------------------
// DesignReferenceMission
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawFault {
    pub id: String,
    pub tai_ns: i64,
    pub duration_ns: i64,
    pub target_kind: String,
    pub instance: String,
    pub target: String,
    pub kind: String,
    pub params: BTreeMap<String, f64>,
    pub clear: bool,
}
impl RawFault {
    fn into_pb(self) -> Result<pb::Fault, DrmError> {
        let target_kind =
            if self.target_kind.is_empty() { pb::FaultTargetKind::Unspecified as i32 } else { enum_from_name("fault.target_kind", &self.target_kind, pb::FaultTargetKind::from_str_name, |e| e as i32)? };
        Ok(pb::Fault {
            id: self.id,
            tai_ns: self.tai_ns,
            duration_ns: self.duration_ns,
            target_kind,
            instance: self.instance,
            target: self.target,
            kind: self.kind,
            params: self.params,
            clear: self.clear,
        })
    }
}

/// `ManeuverExecutionError` (`proto/altavista/v1/system.proto`), field-for-field -- question
/// 100's Gates maneuver execution error model. All four sigmas required and checked finite
/// when this block is present (`super::maneuver::parse_execution_error`, called from
/// [`RawScenarioEvent::into_pb`] below); `seed` must name a real `Scenario.seeds` key, checked
/// separately in [`RawScenario::into_pb`] (which has `Scenario.seeds` in scope, unlike this
/// struct's own conversion). This block being present at all -- even with every sigma `0.0` --
/// is an *explicit* zero, never a default; `RawScenarioEvent.execution_error` is `Option`, and
/// `None` (the field simply absent from the YAML) is the only way to declare a perfect burn.
///
/// **Deliberately no `#[serde(default)]` container attribute here**, unlike every other `Raw*`
/// struct in this loader: those structs default a missing field to proto3's own zero value
/// because that field's absence is itself a meaningful declaration for them (an unset
/// `Parameter.min`, say). Here it would not be -- "all four sigmas required ... when the block
/// is present" (question 100) means an author who writes `execution_error:` at all but leaves
/// out, say, `sigma_pointing_fixed_mps` must get a hard YAML error naming the missing field
/// (`DrmError::Yaml`), not a silently-zeroed sigma that quietly disables half the declared
/// model. `seed` is mandatory here for the same reason, though its own absence would in
/// practice already be caught downstream too (an empty string can never match a real
/// `Scenario.seeds` key, so [`super::maneuver::validate_execution_error_seed`] would refuse it
/// regardless) -- this is the earlier, more direct refusal.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawManeuverExecutionError {
    pub sigma_magnitude_fixed_mps: f64,
    pub sigma_magnitude_proportional: f64,
    pub sigma_pointing_fixed_mps: f64,
    pub sigma_pointing_proportional_rad: f64,
    pub seed: String,
}
impl RawManeuverExecutionError {
    fn into_pb(self) -> pb::ManeuverExecutionError {
        pb::ManeuverExecutionError {
            sigma_magnitude_fixed_mps: self.sigma_magnitude_fixed_mps,
            sigma_magnitude_proportional: self.sigma_magnitude_proportional,
            sigma_pointing_fixed_mps: self.sigma_pointing_fixed_mps,
            sigma_pointing_proportional_rad: self.sigma_pointing_proportional_rad,
            seed: self.seed,
        }
    }
}

/// `Geodetic` (`core.proto`), field-for-field -- the origin of an `AXES_KIND_ENU`/`_NED`
/// [`RawFrameDefinition`].
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawGeodetic {
    pub body: String,
    pub latitude_rad: f64,
    pub longitude_rad: f64,
    pub height_m: f64,
}
impl RawGeodetic {
    fn into_pb(self) -> pb::Geodetic {
        pb::Geodetic { body: self.body, latitude_rad: self.latitude_rad, longitude_rad: self.longitude_rad, height_m: self.height_m }
    }
}

/// `AttitudeSource` (`core.proto`), field-for-field -- question 72's additive `FrameDefinition.
/// attitude_source`. `source` is a oneof (`entity_attitude_stream`/`gmat_attitude_model`),
/// mirrored the same way [`RawBinding::into_pb`] mirrors `Binding.config`'s oneof: two `Option`
/// fields, matched exhaustively, at most one `Some`.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawAttitudeSource {
    pub entity_attitude_stream: Option<String>,
    pub gmat_attitude_model: Option<String>,
    pub reference_frame_id: String,
    pub state_space_id: String,
}
impl RawAttitudeSource {
    fn into_pb(self, frame_id: &str) -> Result<pb::AttitudeSource, DrmError> {
        let source = match (self.entity_attitude_stream, self.gmat_attitude_model) {
            (Some(s), None) => Some(pb::attitude_source::Source::EntityAttitudeStream(s)),
            (None, Some(g)) => Some(pb::attitude_source::Source::GmatAttitudeModel(g)),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(DrmError::InvalidFrameDefinition {
                    id: frame_id.to_string(),
                    reason: "at most one of attitude_source.entity_attitude_stream/gmat_attitude_model may be set (oneof source)".to_string(),
                })
            }
        };
        Ok(pb::AttitudeSource { source, reference_frame_id: self.reference_frame_id, state_space_id: self.state_space_id })
    }
}

/// `FrameDefinition` (`core.proto`), field-for-field -- question 124's typed (not opaque)
/// `Scenario.frames`: a declared frame is realized through the same `FrameRegistry` path the
/// Python scenario uses (`crate::drm::executor::collect_frames`/`registry_default_frame`; see
/// this module's own doc comment). `origin` is a oneof (`body`/`platform_id`/`entity_id`),
/// mirrored the same way [`RawAttitudeSource::source`] and [`RawBinding::config`] are: three
/// `Option` fields, matched exhaustively, at most one `Some`. `axes` is matched against
/// `AxesKind::from_str_name`, the same proto-enum-name convention every other enum field in
/// this module uses.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawFrameDefinition {
    pub id: String,
    pub body: Option<String>,
    pub platform_id: Option<String>,
    pub entity_id: Option<String>,
    pub axes: String,
    pub origin_geodetic: Option<RawGeodetic>,
    pub reference_entity_id: String,
    pub reference_body: String,
    pub gmat_name: String,
    pub description: String,
    pub attitude_source: Option<RawAttitudeSource>,
    pub parent_frame_id: String,
}
impl RawFrameDefinition {
    fn into_pb(self) -> Result<pb::FrameDefinition, DrmError> {
        let axes = if self.axes.is_empty() { pb::AxesKind::Unspecified as i32 } else { enum_from_name("frame.axes", &self.axes, pb::AxesKind::from_str_name, |e| e as i32)? };
        let origin = match (self.body, self.platform_id, self.entity_id) {
            (Some(b), None, None) => Some(pb::frame_definition::Origin::Body(b)),
            (None, Some(p), None) => Some(pb::frame_definition::Origin::PlatformId(p)),
            (None, None, Some(e)) => Some(pb::frame_definition::Origin::EntityId(e)),
            (None, None, None) => None,
            _ => {
                return Err(DrmError::InvalidFrameDefinition {
                    id: self.id.clone(),
                    reason: "at most one of body/platform_id/entity_id may be set (oneof origin)".to_string(),
                })
            }
        };
        let attitude_source = self.attitude_source.map(|a| a.into_pb(&self.id)).transpose()?;
        Ok(pb::FrameDefinition {
            id: self.id,
            origin,
            axes,
            origin_geodetic: self.origin_geodetic.map(RawGeodetic::into_pb),
            reference_entity_id: self.reference_entity_id,
            reference_body: self.reference_body,
            gmat_name: self.gmat_name,
            description: self.description,
            attitude_source,
            parent_frame_id: self.parent_frame_id,
            // Question 129: filled by the producer through the dynamics contract's `convert`
            // (M19.2), never authored in a DRM.
            fixed_rotation_q: Vec::new(),
        })
    }
}
fn frames(v: Vec<RawFrameDefinition>) -> Result<Vec<pb::FrameDefinition>, DrmError> {
    v.into_iter().map(RawFrameDefinition::into_pb).collect()
}

/// `ScenarioEvent` (`proto/altavista/v1/system.proto`), field-for-field -- question 97's typed
/// (not opaque) `Scenario.events`. Every event is run through [`super::maneuver::parse`] at
/// load time in [`into_pb`](RawScenarioEvent::into_pb): today that means `kind` must be
/// `"maneuver"` (the only kind this loader models -- see the module doc comment), but the field
/// shape itself already mirrors the full generic `ScenarioEvent` message, so a future kind needs
/// no schema change here, only a new branch in `maneuver`/wherever that kind's own typed
/// contract lives. `execution_error` (question 100) is additive: absent (the field simply
/// missing from the YAML) means a perfect burn and is never defaulted.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawScenarioEvent {
    pub id: String,
    pub tai_ns: i64,
    pub kind: String,
    pub instance: String,
    pub values: BTreeMap<String, f64>,
    pub attributes: BTreeMap<String, String>,
    pub execution_error: Option<RawManeuverExecutionError>,
}
impl RawScenarioEvent {
    fn into_pb(self) -> Result<pb::ScenarioEvent, DrmError> {
        let event = pb::ScenarioEvent {
            id: self.id,
            tai_ns: self.tai_ns,
            kind: self.kind,
            instance: self.instance,
            values: self.values,
            attributes: self.attributes,
            execution_error: self.execution_error.map(RawManeuverExecutionError::into_pb),
        };
        // Typed validation, not just an honest field transcription (question 97's own rule:
        // "anything else about the event is a typed load error, not a silently ignored
        // field") -- the same check `executor::execute` re-applies at run time against the
        // real Scenario; see `maneuver::parse`'s own doc comment for why running it twice is
        // deliberate, not redundant (mirrors `executor`'s load-time expression validation).
        // This also checks a present `execution_error`'s four sigmas are finite (question 100);
        // its `seed` is checked separately in `RawScenario::into_pb`, which alone has
        // `Scenario.seeds` in scope.
        //
        // M25.2 (`docs/sil-plan.md`'s M25 milestone): `kind == "command"` is a second typed
        // contract (`super::command::parse`), not `super::maneuver::parse`'s -- dispatched by
        // `kind` alone, mirroring how `executor::execute`'s own run-time loop dispatches the
        // identical way.
        if event.kind == super::command::COMMAND_KIND {
            super::command::parse(&event)?;
        } else {
            super::maneuver::parse(&event)?;
        }
        Ok(event)
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawScenario {
    pub start_tai_ns: i64,
    pub end_tai_ns: i64,
    pub data_pack_hash: String,
    /// Typed as of question 124 (M18.1) -- see the module doc comment and
    /// [`RawFrameDefinition`].
    pub frames: Vec<RawFrameDefinition>,
    /// Typed as of question 97 -- see the module doc comment and [`RawScenarioEvent`].
    pub events: Vec<RawScenarioEvent>,
    pub faults: Vec<RawFault>,
    pub seeds: BTreeMap<String, u64>,
}
impl RawScenario {
    fn into_pb(self) -> Result<pb::Scenario, DrmError> {
        let frames = frames(self.frames)?;
        let events = self.events.into_iter().map(RawScenarioEvent::into_pb).collect::<Result<Vec<_>, _>>()?;
        // Question 100: a declared execution_error.seed must name a real Scenario.seeds key --
        // checked here (not in RawScenarioEvent::into_pb above) because this is the one place
        // in this loader that has Scenario.seeds in scope. super::maneuver::parse is cheap and
        // pure, so re-parsing each event here (rather than threading its already-parsed
        // ParsedManeuver back out of RawScenarioEvent::into_pb) keeps that function's own
        // signature the honest field-for-field transcription it already is everywhere else in
        // this module.
        for event in &events {
            // M25.2: a `command` event carries no `ManeuverExecutionError`/seed at all -- see
            // `RawScenarioEvent::into_pb`'s own identical `kind`-dispatch just above.
            if event.kind == super::command::COMMAND_KIND {
                super::command::parse(event)?;
                continue;
            }
            let m = super::maneuver::parse(event)?;
            super::maneuver::validate_execution_error_seed(&m, &self.seeds)?;
        }
        let faults = self.faults.into_iter().map(RawFault::into_pb).collect::<Result<Vec<_>, _>>()?;
        Ok(pb::Scenario { start_tai_ns: self.start_tai_ns, end_tai_ns: self.end_tai_ns, data_pack_hash: self.data_pack_hash, frames, events, faults, seeds: self.seeds })
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawObjective {
    pub name: String,
    pub expression: String,
    pub target: f64,
    pub tolerance: f64,
    pub unit: String,
}
impl RawObjective {
    fn into_pb(self) -> Result<pb::Objective, DrmError> {
        let unit = if self.unit.is_empty() { pb::Unit::Unspecified as i32 } else { enum_from_name("objective.unit", &self.unit, pb::Unit::from_str_name, |e| e as i32)? };
        Ok(pb::Objective { name: self.name, expression: self.expression, target: self.target, tolerance: self.tolerance, unit })
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawMeasureOfEffectiveness {
    pub name: String,
    pub expression: String,
    pub unit: String,
}
impl RawMeasureOfEffectiveness {
    fn into_pb(self) -> Result<pb::MeasureOfEffectiveness, DrmError> {
        let unit = if self.unit.is_empty() { pb::Unit::Unspecified as i32 } else { enum_from_name("measure.unit", &self.unit, pb::Unit::from_str_name, |e| e as i32)? };
        Ok(pb::MeasureOfEffectiveness { name: self.name, expression: self.expression, unit })
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawDrmOptions {
    pub covariance: bool,
    pub default_step_rate_hz: f64,
    pub sample_interval_s: f64,
    pub real_time: bool,
    pub nearest_spd_projection: bool,
    pub accept_missing_stm_terms: bool,
}
impl RawDrmOptions {
    fn into_pb(self) -> pb::DrmOptions {
        pb::DrmOptions {
            covariance: self.covariance,
            default_step_rate_hz: self.default_step_rate_hz,
            sample_interval_s: self.sample_interval_s,
            real_time: self.real_time,
            nearest_spd_projection: self.nearest_spd_projection,
            accept_missing_stm_terms: self.accept_missing_stm_terms,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawDesignReferenceMission {
    pub id: String,
    pub version: String,
    pub name: String,
    pub sos_configuration_id: String,
    pub scenario: Option<RawScenario>,
    pub objectives: Vec<RawObjective>,
    pub measures: Vec<RawMeasureOfEffectiveness>,
    pub options: Option<RawDrmOptions>,
    pub label: Option<RawLabel>,
    pub provenance: Option<RawProvenance>,
    pub hash: String,
}
impl RawDesignReferenceMission {
    pub fn into_pb(self) -> Result<pb::DesignReferenceMission, DrmError> {
        let scenario = self.scenario.map(RawScenario::into_pb).transpose()?;
        let objectives = self.objectives.into_iter().map(RawObjective::into_pb).collect::<Result<Vec<_>, _>>()?;
        let measures = self.measures.into_iter().map(RawMeasureOfEffectiveness::into_pb).collect::<Result<Vec<_>, _>>()?;
        Ok(pb::DesignReferenceMission {
            id: self.id,
            version: self.version,
            name: self.name,
            sos_configuration_id: self.sos_configuration_id,
            scenario,
            objectives,
            measures,
            options: self.options.map(RawDrmOptions::into_pb),
            label: opt_label(self.label),
            provenance: opt_provenance(self.provenance)?,
            hash: self.hash,
        })
    }
}

/// Parse a YAML document as a [`pb::DesignReferenceMission`] (see the module doc comment for
/// the authoring-format rationale).
pub fn parse_drm_yaml(yaml: &str) -> Result<pb::DesignReferenceMission, DrmError> {
    let raw: RawDesignReferenceMission = serde_yaml::from_str(yaml).map_err(|e| DrmError::Yaml(e.to_string()))?;
    raw.into_pb()
}

/// Parse a YAML document as a [`pb::SosConfiguration`].
pub fn parse_sos_yaml(yaml: &str) -> Result<pb::SosConfiguration, DrmError> {
    let raw: RawSosConfiguration = serde_yaml::from_str(yaml).map_err(|e| DrmError::Yaml(e.to_string()))?;
    raw.into_pb()
}

/// Parse a YAML document as a [`pb::SystemDefinition`].
pub fn parse_system_definition_yaml(yaml: &str) -> Result<pb::SystemDefinition, DrmError> {
    let raw: RawSystemDefinition = serde_yaml::from_str(yaml).map_err(|e| DrmError::Yaml(e.to_string()))?;
    raw.into_pb()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_drm_and_maps_enums_by_their_proto_names() {
        let yaml = r#"
id: drm_test
version: "1"
name: Test DRM
sos_configuration_id: sos_test
scenario:
  start_tai_ns: 1000
  end_tai_ns: 2000
  faults:
    - id: f1
      tai_ns: 1500
      target_kind: FAULT_TARGET_KIND_DYNAMICS
      instance: leo
      target: "spacecraft.Cd"
      kind: parameter
      params:
        value: 3.5
options:
  covariance: true
  sample_interval_s: 0.1
  accept_missing_stm_terms: true
hash: ""
"#;
        let drm = parse_drm_yaml(yaml).expect("parses");
        assert_eq!(drm.id, "drm_test");
        let scenario = drm.scenario.expect("scenario");
        assert_eq!(scenario.faults.len(), 1);
        assert_eq!(scenario.faults[0].target_kind, pb::FaultTargetKind::Dynamics as i32);
        assert!(drm.options.unwrap().accept_missing_stm_terms);
    }

    #[test]
    fn parses_a_typed_maneuver_scenario_event() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
hash: ""
"#;
        let drm = parse_drm_yaml(yaml).expect("parses");
        let scenario = drm.scenario.expect("scenario");
        assert_eq!(scenario.events.len(), 1);
        assert_eq!(scenario.events[0].kind, "maneuver");
        assert_eq!(scenario.events[0].values.get("dv_x"), Some(&20.0));
        assert_eq!(scenario.events[0].attributes.get("frame_id").map(String::as_str), Some("AXES_KIND_VNB"));
    }

    #[test]
    fn a_scenario_event_of_an_unmodeled_kind_is_a_typed_load_error() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  events:
    - id: e1
      tai_ns: 5000
      kind: mode
      instance: leo
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::UnsupportedScenarioEventKind { .. }), "{err:?}");
    }

    #[test]
    fn parses_a_typed_maneuver_execution_error_block() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  seeds:
    burn_seed: 42
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
      execution_error:
        sigma_magnitude_fixed_mps: 0.01
        sigma_magnitude_proportional: 0.001
        sigma_pointing_fixed_mps: 0.02
        sigma_pointing_proportional_rad: 0.0005
        seed: burn_seed
hash: ""
"#;
        let drm = parse_drm_yaml(yaml).expect("parses");
        let scenario = drm.scenario.expect("scenario");
        let ee = scenario.events[0].execution_error.as_ref().expect("execution_error present");
        assert_eq!(ee.sigma_magnitude_fixed_mps, 0.01);
        assert_eq!(ee.seed, "burn_seed");
    }

    #[test]
    fn an_execution_error_block_missing_one_sigma_field_is_a_hard_yaml_error_not_a_silent_zero() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  seeds:
    burn_seed: 42
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
      execution_error:
        sigma_magnitude_fixed_mps: 0.01
        sigma_magnitude_proportional: 0.001
        sigma_pointing_fixed_mps: 0.02
        # sigma_pointing_proportional_rad deliberately omitted
        seed: burn_seed
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::Yaml(ref msg) if msg.contains("sigma_pointing_proportional_rad")), "{err:?}");
    }

    #[test]
    fn an_absent_execution_error_block_stays_none_not_defaulted() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
hash: ""
"#;
        let drm = parse_drm_yaml(yaml).expect("parses");
        let scenario = drm.scenario.expect("scenario");
        assert!(scenario.events[0].execution_error.is_none());
    }

    #[test]
    fn a_maneuver_execution_error_seed_absent_from_scenario_seeds_is_a_typed_load_error() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
      execution_error:
        sigma_magnitude_fixed_mps: 0.01
        sigma_magnitude_proportional: 0.0
        sigma_pointing_fixed_mps: 0.0
        sigma_pointing_proportional_rad: 0.0
        seed: no_such_seed
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::UnknownManeuverSeed { ref id, ref seed } if id == "burn1" && seed == "no_such_seed"), "{err:?}");
    }

    #[test]
    fn a_maneuver_execution_error_with_a_non_finite_sigma_is_a_typed_load_error() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  seeds:
    burn_seed: 42
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
      execution_error:
        sigma_magnitude_fixed_mps: .nan
        sigma_magnitude_proportional: 0.0
        sigma_pointing_fixed_mps: 0.0
        sigma_pointing_proportional_rad: 0.0
        seed: burn_seed
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidManeuverExecutionError { ref id, .. } if id == "burn1"), "{err:?}");
    }

    #[test]
    fn a_maneuver_event_with_an_unrecognized_frame_is_a_typed_load_error() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  events:
    - id: burn1
      tai_ns: 5000
      kind: maneuver
      instance: leo
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: NOT_A_REAL_FRAME
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { .. }), "{err:?}");
    }

    #[test]
    fn refuses_an_unmodeled_nonempty_field_rather_than_dropping_it() {
        let yaml = r#"
id: drm_test
variants:
  - name: some_variant
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::UnsupportedField { field: "SystemDefinition.variants" }), "{err:?}");
    }

    // -----------------------------------------------------------------------------------
    // Scenario.frames (question 124, M18.1).
    // -----------------------------------------------------------------------------------

    /// A declared `Scenario.frames` entry parses into a real `pb::FrameDefinition`, `origin`
    /// oneof included -- proof this loader no longer refuses the field at all. Fails against
    /// the pre-M18.1 loader (`DrmError::UnsupportedField { field: "Scenario.frames" }` on any
    /// non-empty list) and against an implementation that parses the list but drops `origin`,
    /// `axes`, or `parent_frame_id` on the floor.
    #[test]
    fn parses_a_declared_scenario_frame_with_a_body_origin() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  frames:
    - id: EarthICRF
      body: Earth
      axes: AXES_KIND_ICRF
      description: "author-declared ICRF"
      parent_frame_id: ""
hash: ""
"#;
        let drm = parse_drm_yaml(yaml).expect("parses");
        let scenario = drm.scenario.expect("scenario");
        assert_eq!(scenario.frames.len(), 1);
        let f = &scenario.frames[0];
        assert_eq!(f.id, "EarthICRF");
        assert_eq!(f.origin, Some(pb::frame_definition::Origin::Body("Earth".to_string())));
        assert_eq!(f.axes, pb::AxesKind::Icrf as i32);
        assert_eq!(f.description, "author-declared ICRF");
    }

    /// `origin`'s three fields (`body`/`platform_id`/`entity_id`) are a proto3 oneof: at most
    /// one may be set. Fails against an implementation that ignores the extra field (silently
    /// keeping whichever it processed last) instead of refusing.
    #[test]
    fn a_frame_declaring_two_origin_fields_at_once_is_a_typed_load_error() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  frames:
    - id: bad_frame
      body: Earth
      entity_id: sat1
      axes: AXES_KIND_ICRF
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidFrameDefinition { ref id, .. } if id == "bad_frame"), "{err:?}");
    }

    /// A frame naming an entity as its own reference (RIC-shaped), with an unrecognized `axes`
    /// name, is refused the same way every other enum field in this module is -- proves `axes`
    /// is actually matched against `AxesKind::from_str_name`, not merely stored as a string.
    #[test]
    fn a_frame_with_an_unrecognized_axes_name_is_a_typed_load_error() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 10000
  frames:
    - id: bad_frame
      entity_id: sat1
      reference_entity_id: sat1
      reference_body: Earth
      axes: AXES_KIND_NOT_A_REAL_KIND
hash: ""
"#;
        let err = parse_drm_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { field: "frame.axes", .. }), "{err:?}");
    }

    /// An empty `frames` list still parses exactly like before this field was typed -- no
    /// regression for every existing fixture that declares no frames at all.
    #[test]
    fn an_empty_frames_list_still_parses_exactly_like_before_this_field_was_typed() {
        let yaml = r#"
id: drm_test
scenario:
  start_tai_ns: 0
  end_tai_ns: 1
hash: ""
"#;
        let drm = parse_drm_yaml(yaml).expect("parses");
        assert!(drm.scenario.unwrap().frames.is_empty());
    }

    #[test]
    fn container_binding_round_trips_through_the_oneof() {
        let yaml = r#"
id: sos_test
instances:
  - name: gnc
    system_id: gnc_sys
    binding:
      kind: BINDING_KIND_CONTAINER
      container:
        image: "ghcr.io/x/y:1"
        lockstep_capable: true
hash: ""
"#;
        let sos = parse_sos_yaml(yaml).expect("parses");
        let binding = sos.instances[0].binding.clone().unwrap();
        assert_eq!(binding.kind, pb::BindingKind::Container as i32);
        assert!(matches!(binding.config, Some(pb::binding::Config::Container(_))));
    }

    #[test]
    fn unknown_enum_name_is_a_typed_error_not_a_silent_zero() {
        let yaml = r#"
id: sos_test
instances:
  - name: gnc
    system_id: gnc_sys
    binding:
      kind: BINDING_KIND_NOT_A_REAL_KIND
hash: ""
"#;
        let err = parse_sos_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { field: "binding.kind", .. }), "{err:?}");
    }

    // -----------------------------------------------------------------------------------
    // SystemDefinition.state_space (question 94, M9.2).
    // -----------------------------------------------------------------------------------

    #[test]
    fn parses_a_declared_state_space_with_id_equal_to_state_space_id() {
        let yaml = r#"
id: sys_test
dynamics_model: gmat.earth.jgm2_8x8.sun_moon
state_space_id: gmat.orbital.cartesian6
state_space:
  id: gmat.orbital.cartesian6
  components:
    - label: pos_x
      unit: UNIT_METER
    - label: pos_y
      unit: UNIT_METER
    - label: pos_z
      unit: UNIT_METER
    - label: vel_x
      unit: UNIT_METER_PER_SECOND
    - label: vel_y
      unit: UNIT_METER_PER_SECOND
    - label: vel_z
      unit: UNIT_METER_PER_SECOND
hash: ""
"#;
        let sys = parse_system_definition_yaml(yaml).expect("parses");
        let space = sys.state_space.expect("state_space present");
        assert_eq!(space.id, "gmat.orbital.cartesian6");
        assert_eq!(space.components.len(), 6);
        assert_eq!(space.components[0].label, "pos_x");
        assert_eq!(space.components[0].unit, pb::Unit::Meter as i32);
        assert_eq!(space.components[3].unit, pb::Unit::MeterPerSecond as i32);
        // The loader itself does not check id equality against state_space_id or classify
        // components -- that is crate::trajectory::resolve_state_space's job (M9.2's
        // handoff). This test only pins the honest transcription.
        assert_eq!(sys.state_space_id, "gmat.orbital.cartesian6");
    }

    #[test]
    fn a_system_definition_with_no_state_space_leaves_it_unset() {
        let yaml = r#"
id: sys_test
state_space_id: gmat.orbital.cartesian6
hash: ""
"#;
        let sys = parse_system_definition_yaml(yaml).expect("parses");
        assert!(sys.state_space.is_none());
    }

    #[test]
    fn state_space_component_unknown_unit_name_is_a_typed_error() {
        let yaml = r#"
id: sys_test
state_space_id: custom
state_space:
  id: custom
  components:
    - label: pos_x
      unit: UNIT_NOT_A_REAL_UNIT
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { field: "state_space.components[].unit", .. }), "{err:?}");
    }

    // -----------------------------------------------------------------------------------
    // SystemDefinition.ports (question 108, M13.1).
    // -----------------------------------------------------------------------------------

    #[test]
    fn parses_a_typed_port_list_with_timing_and_matches_enums_by_their_proto_names() {
        let yaml = r#"
id: sys_test
ports:
  - name: telemetry_out
    kind: PORT_KIND_SIGNAL
    direction: PORT_DIRECTION_OUT
    schema: "f64"
    timing:
      rate_hz: 10.0
      latency_ns: 50000000
      jitter_ns: 1000
      deterministic: true
    interface_class: uart
  - name: command_in
    kind: PORT_KIND_SIGNAL
    direction: PORT_DIRECTION_IN
    schema: "f64"
hash: ""
"#;
        let sys = parse_system_definition_yaml(yaml).expect("parses");
        assert_eq!(sys.ports.len(), 2);
        let out = &sys.ports[0];
        assert_eq!(out.name, "telemetry_out");
        assert_eq!(out.kind, pb::PortKind::Signal as i32);
        assert_eq!(out.direction, pb::PortDirection::Out as i32);
        assert_eq!(out.interface_class, "uart");
        let timing = out.timing.as_ref().expect("timing declared");
        assert_eq!(timing.rate_hz, 10.0);
        assert_eq!(timing.latency_ns, 50_000_000);
        assert_eq!(timing.jitter_ns, 1_000);
        assert!(timing.deterministic);

        let inp = &sys.ports[1];
        assert_eq!(inp.name, "command_in");
        assert_eq!(inp.direction, pb::PortDirection::In as i32);
        assert!(inp.timing.is_none(), "an undeclared timing block stays None, not a zeroed default block");
    }

    #[test]
    fn an_empty_port_list_still_parses_exactly_like_before_this_field_was_typed() {
        let yaml = r#"
id: sys_test
hash: ""
"#;
        let sys = parse_system_definition_yaml(yaml).expect("parses");
        assert!(sys.ports.is_empty());
    }

    #[test]
    fn a_port_with_an_unknown_kind_name_is_a_typed_error() {
        let yaml = r#"
id: sys_test
ports:
  - name: p
    kind: PORT_KIND_NOT_A_REAL_KIND
    direction: PORT_DIRECTION_OUT
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { field: "port.kind", .. }), "{err:?}");
    }

    #[test]
    fn a_port_with_an_unknown_direction_name_is_a_typed_error() {
        let yaml = r#"
id: sys_test
ports:
  - name: p
    kind: PORT_KIND_SIGNAL
    direction: PORT_DIRECTION_NOT_A_REAL_DIRECTION
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { field: "port.direction", .. }), "{err:?}");
    }

    // -----------------------------------------------------------------------------------
    // SystemDefinition.packet_codecs (question 149, M22.3).
    // -----------------------------------------------------------------------------------

    /// A declared `packet_codecs` entry loads and round-trips through the typed loader --
    /// proof this loader no longer refuses the field at all (the pre-M22.3 loader always
    /// produced an empty `Vec`, so this also fails against that placeholder). Fails against an
    /// implementation that drops `fields`, mismatches `type`/`unit` enum values, or leaves
    /// `scale`/`offset`/`target` on the floor.
    #[test]
    fn parses_a_declared_packet_codec_and_round_trips_through_the_typed_loader() {
        let yaml = r#"
id: sys_test
packet_codecs:
  - id: tm_adcs
    apid: 291
    is_command: false
    secondary_header_bytes: 0
    user_data_bytes: 4
    description: "ADCS telemetry"
    fields:
      - name: wheel_speed
        bit_offset: 0
        bit_width: 32
        type: PACKET_FIELD_TYPE_UINT
        unit: UNIT_METER_PER_SECOND
        scale: 0.5
        offset: 10.0
        target: "altavista.attitude/wheel_speed"
hash: ""
"#;
        let sys = parse_system_definition_yaml(yaml).expect("parses");
        assert_eq!(sys.packet_codecs.len(), 1);
        let c = &sys.packet_codecs[0];
        assert_eq!(c.id, "tm_adcs");
        assert_eq!(c.apid, 291);
        assert!(!c.is_command);
        assert_eq!(c.user_data_bytes, 4);
        assert_eq!(c.fields.len(), 1);
        let f = &c.fields[0];
        assert_eq!(f.name, "wheel_speed");
        assert_eq!(f.bit_offset, 0);
        assert_eq!(f.bit_width, 32);
        assert_eq!(f.r#type, pb::PacketFieldType::Uint as i32);
        assert_eq!(f.unit, pb::Unit::MeterPerSecond as i32);
        assert_eq!(f.scale, 0.5);
        assert_eq!(f.offset, 10.0);
        assert_eq!(f.target, "altavista.attitude/wheel_speed");
    }

    #[test]
    fn an_empty_packet_codecs_list_still_parses_exactly_like_before_this_field_was_typed() {
        let yaml = r#"
id: sys_test
hash: ""
"#;
        let sys = parse_system_definition_yaml(yaml).expect("parses");
        assert!(sys.packet_codecs.is_empty());
    }

    /// Fails against a loader that accepts a field whose `bit_offset + bit_width` runs past
    /// `user_data_bytes` -- question 149's own named example of a load-time-only check.
    #[test]
    fn a_packet_field_extent_past_user_data_bytes_is_a_typed_load_error() {
        let yaml = r#"
id: sys_test
packet_codecs:
  - id: tm_bad
    apid: 1
    user_data_bytes: 2
    fields:
      - name: too_long
        bit_offset: 0
        bit_width: 32
        type: PACKET_FIELD_TYPE_UINT
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::Codec(crate::codec::CodecError::FieldExtentExceedsUserData { ref field, .. }) if field == "too_long"), "{err:?}");
    }

    /// Fails against a loader that accepts a `FLOAT64` field declared with a `bit_width` other
    /// than 64.
    #[test]
    fn a_packet_field_bit_width_disagreeing_with_a_fixed_width_type_is_a_typed_load_error() {
        let yaml = r#"
id: sys_test
packet_codecs:
  - id: tm_bad
    apid: 1
    user_data_bytes: 8
    fields:
      - name: bad_float
        bit_offset: 0
        bit_width: 32
        type: PACKET_FIELD_TYPE_FLOAT64
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::Codec(crate::codec::CodecError::InvalidFieldWidth { ref field, bit_width: 32, .. }) if field == "bad_float"), "{err:?}");
    }

    /// Fails against a loader that never cross-checks `apid` across the whole
    /// `packet_codecs` list (only validating each codec in isolation).
    #[test]
    fn duplicate_apids_within_one_system_definition_are_a_typed_load_error() {
        let yaml = r#"
id: sys_test
packet_codecs:
  - id: tm_a
    apid: 100
    user_data_bytes: 1
  - id: tm_b
    apid: 100
    user_data_bytes: 1
hash: ""
"#;
        let err = parse_system_definition_yaml(yaml).unwrap_err();
        assert!(matches!(err, DrmError::Codec(crate::codec::CodecError::DuplicateApid { apid: 100, .. })), "{err:?}");
    }
}
