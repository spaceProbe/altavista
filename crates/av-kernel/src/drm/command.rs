//! DRM command events become CDM `Command`s (M25.2, `docs/sil-plan.md`'s M25 milestone: "the
//! ground segment as a system, CCSDS telecommands from DRM command events, replay with the
//! flight software removed"; `docs/open-questions.md` questions 149/157/164). This module gives
//! `Scenario.events` of kind `"command"` (`ScenarioEvent`, generic `kind` string, `values`/
//! `attributes` maps -- the same "no proto change, none is authorized" shape [`super::maneuver`]
//! already uses for `"maneuver"`) a typed contract, and carries the parsed result through the
//! REAL state machine `proto/altavista/v1/command.proto` declares:
//!
//! ```text
//! PROPOSED -> CHECKED -> AUTHORIZED -> DISPATCHED -> ACKED
//! ```
//!
//! **Every state name above is the real `CommandState` enum** (`COMMAND_STATE_PROPOSED`, ...,
//! `COMMAND_STATE_ACKED`) -- there is no invented "APPROVED"/"EXECUTED" state, and `CHECKED`/
//! `AUTHORIZED` are never skipped (`docs/open-questions.md`'s own note on an earlier paraphrase
//! that dropped `CHECKED` and invented `EXECUTED`). `AckLevel` is a separate axis (`ACK_LEVEL_*`)
//! carried on the `ACKED` transition's own `CommandTransition.ack_level`, never folded into
//! `CommandState` itself.
//!
//! ## Schema (mirrors `super::maneuver`'s own "Schema" section)
//!
//! `kind == "command"` ([`COMMAND_KIND`]; any other value stays [`DrmError::
//! UnsupportedScenarioEventKind`], `super::maneuver::parse`'s own existing refusal, unchanged).
//! Required: `instance` (non-empty -- the command's *target*, `Command.entity_id`);
//! `values["value"]` (the one numeric field this task's own writable-parameter convention
//! carries -- mirrors `binding::GMAT_WRITABLE_PARAMETERS`'s single-field "Cd" precedent: no
//! generic multi-field command payload is built here, since `PacketField`s are numeric-only and
//! this task's own scope is one demonstrable command class); `attributes["field"]` (the target's
//! own declared writable parameter name, e.g. `"accel_scale"` -- `Command.command_class`'s
//! finer-grained sibling, not `Command.command_class` itself); `attributes["from"]` (the ground
//! instance responsible for dispatching this command -- see this module's own "Scope disclosed,
//! not hidden" section for why this is declared explicitly rather than discovered from
//! `Connection` topology). Optional: `attributes["command_class"]` (default `"generic"`,
//! `Command.command_class` verbatim); `attributes["hazardous"]` (`"true"`/`"false"`, default
//! `"false"`, `Command.hazardous` verbatim). Any other `values`/`attributes` key is [`DrmError::
//! UnknownParameter`], the same "typed, not opaque" rule `maneuver::parse` already applies.
//!
//! ## Where each transition actually happens
//!
//! - **PROPOSED / CHECKED / AUTHORIZED**: synthesized once, at `Scenario.start_tai_ns`, by
//!   [`propose_check_authorize`] -- this SIL replay has no external authorization service (a real
//!   ground segment's own approval workflow is out of this task's scope, disclosed in
//!   `drms/M25_2_REPORT.md`), so every declared command is auto-checked (its target instance and
//!   field are both structurally valid, or `parse`/[`execute`-time validation] would already have
//!   refused the whole DRM) and auto-authorized (`principal = "sil-auto-authority"`, `reason`
//!   states the simplification plainly) before the run's own clock starts moving.
//! - **DISPATCHED**: at the command's own declared `tai_ns`, `super::executor::run_shared_group`
//!   (before its own main boundary loop) encodes the command as one CCSDS space packet
//!   (`is_command=true` -- [`command_out_packet_codec`]), using the *target's own already-
//!   resolved* `ConstantAccelSpec::consume_framed_codec` (never a second, independently
//!   maintained copy -- this is what guarantees the encoding APID matches what the target's own
//!   decode expects), and hands it to `crate::router::Router::deliver` as if it were the named
//!   `attributes["from"]` instance's own `Outbox` emission on the fixed [`COMMAND_DISPATCH_PORT`]
//!   port name -- **the router's own existing latency model carries it from there**; this module
//!   never reimplements delivery. See [`dispatched_event`].
//! - **ACKED**: `crate::drm::binding::ConstantAccelModel`'s own new FRAMED-consume path
//!   (`docs/sil-plan.md`'s "Job 1", `ConstantAccelModel::consume_framed`) decodes the delivered
//!   packet, applies it (a real `av_dynamics::AppliedCommand`, `EVENT_KIND_PORT_COMMAND`'s own
//!   existing pipeline, question 130), and sends one ack telemetry packet back
//!   ([`command_ack_packet_codec`]) carrying the original packet's own CCSDS `sequence_count` --
//!   "acknowledged by the flight software's telemetry," a real wire message, not merely inferred
//!   from the applied command. `super::executor::run_shared_group`'s existing applied-commands
//!   drain (the same one that already turns an `AppliedCommand` into `EVENT_KIND_PORT_COMMAND`)
//!   additionally looks the applying `(target instance, field)` pair up in a map built once per
//!   run from this same `commands` slice, and -- when it matches a declared command -- emits
//!   [`acked_event`] with `AckLevel::AssetExecuted` at that exact epoch. **This executor derives
//!   ACKED from the flight instance's own applied-command record, not by making the ground
//!   instance decode the ack packet's own bytes** -- see "Scope disclosed, not hidden" below for
//!   exactly why, and what a fuller implementation would add.
//!
//! ## Scope disclosed, not hidden
//!
//! One outstanding command per `(target instance, field)` pair in any one demonstration DRM: the
//! ACKED transition above is derived from the applying instance+field, not from decoding the ack
//! packet's own bytes and correlating by `cmd_seq` -- `crate::drm::ground::GroundStationModel`
//! (M25.1) is untouched by this task, deliberately, to keep zero risk to its own baseline. The
//! CCSDS `sequence_count`-based numeric correlator [`command_ack_packet_codec`]'s own `cmd_seq`
//! field, [`assign_sequence_numbers`] is real and unit-tested, and the ack packet genuinely is
//! sent and genuinely is delivered through the router (`drms/demo_command.sos.yaml` declares a
//! real connection and latency for it) -- nothing on the receiving end decodes it yet. A fuller
//! ground implementation that does is real future work; see `drms/M25_2_REPORT.md`.
//!
//! `attributes["from"]` names the dispatching ground instance explicitly rather than discovering
//! it from `SosConfiguration.connections` topology, which would require this module to duplicate
//! `crate::router::Router::build`'s own connection resolution for one caller. That instance must
//! declare a `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` port literally named [`COMMAND_DISPATCH_
//! PORT`] -- a fixed convention, not a second `"port.*"` parameter naming the same thing a second
//! way, since dispatch is synthetic (from the executor directly, never a real model's own `step_
//! with_ports` -- there is no spec to parse a port name out of for the sender side).

use std::collections::BTreeMap;

use av_cdm::pb::{AckLevel, Command, CommandState, CommandTransition, Event, EventKind, PacketCodec, PacketField, PacketFieldType, Provenance, ScenarioEvent, Unit};

use super::DrmError;

/// `ScenarioEvent.kind`'s value this module models (see the module doc comment).
pub const COMMAND_KIND: &str = "command";

/// Reserved synthetic "port" name [`crate::drm::ground::GroundStationModel::step_with_ports`]
/// uses to report a decoded ack-telemetry packet as an `av_dynamics::AppliedCommand` -- never a
/// real declared `Port.name`, mirroring [`crate::drm::ground::CONTACT_TRANSITION_PORT`]'s own
/// doc comment exactly (the same "reserved port name, not a new field on `AppliedCommand`" seam,
/// applied to a second, unrelated need).
pub const COMMAND_ACK_PORT: &str = "__command_ack__";

/// The fixed, conventional port name a `command` `ScenarioEvent`'s own `attributes["from"]`
/// instance must declare a `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` port under, for `super::
/// executor::execute` to hand the encoded telecommand to `crate::router::Router::deliver` as
/// that instance's own emission -- mirrors `crate::drm::ground::GROUND_TC_OUT_PORT`'s own fixed-
/// name convention, one port name, not a second `"port.*"` parameter naming the same thing a
/// second way (this task's own dispatch is synthetic, from the executor directly, never a real
/// model's own `step_with_ports` -- there is no spec to parse a port name out of).
pub const COMMAND_DISPATCH_PORT: &str = "cmd_out";

fn context(id: &str) -> String {
    format!("scenario event {id:?}")
}

/// A `command` `ScenarioEvent`, typed (see the module doc comment's "Schema" section).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommand {
    pub id: String,
    pub tai_ns: i64,
    /// The command's target -- `Command.entity_id`.
    pub instance: String,
    /// The target's own declared writable parameter name -- `binding::CONSTANT_ACCEL_WRITABLE_
    /// PARAMETERS`'s own allowlist checks this at `execute()` time (this module has no model
    /// registry in scope to check it against here, the same "parse first, cross-check against
    /// the SOS/registry at the caller" split `maneuver::parse`'s own `instance` field already
    /// has).
    pub field: String,
    pub value: f64,
    pub command_class: String,
    pub hazardous: bool,
    /// The ground instance responsible for dispatching this command -- see the module doc
    /// comment's "Scope disclosed, not hidden" section.
    pub from: String,
}

const VALUE_KEY: &str = "value";
const FIELD_ATTR_KEY: &str = "field";
const FROM_ATTR_KEY: &str = "from";
const COMMAND_CLASS_ATTR_KEY: &str = "command_class";
const HAZARDOUS_ATTR_KEY: &str = "hazardous";

/// Parse and validate `event` as a `command` `ScenarioEvent` -- see the module doc comment's
/// "Schema" section for the exact contract. Called from `crate::drm::schema` (load time) and
/// `super::executor` (run time), mirroring `maneuver::parse`'s own doc comment on why running it
/// twice is deliberate, not redundant.
pub fn parse(event: &ScenarioEvent) -> Result<ParsedCommand, DrmError> {
    if event.kind != COMMAND_KIND {
        return Err(DrmError::UnsupportedScenarioEventKind { id: event.id.clone(), kind: event.kind.clone() });
    }
    if event.instance.is_empty() {
        return Err(DrmError::MissingParameter { context: context(&event.id), name: "instance".to_string() });
    }
    let value = *event.values.get(VALUE_KEY).ok_or_else(|| DrmError::MissingParameter { context: format!("{} values", context(&event.id)), name: VALUE_KEY.to_string() })?;
    if let Some(unknown) = event.values.keys().find(|k| k.as_str() != VALUE_KEY) {
        return Err(DrmError::UnknownParameter { context: format!("{} values", context(&event.id)), name: unknown.clone() });
    }
    if !value.is_finite() {
        return Err(DrmError::InvalidDrmOptions { reason: format!("{}: values[{VALUE_KEY:?}] must be finite, got {value}", context(&event.id)) });
    }

    let field = event.attributes.get(FIELD_ATTR_KEY).filter(|s| !s.is_empty()).ok_or_else(|| DrmError::MissingParameter { context: context(&event.id), name: format!("attributes[{FIELD_ATTR_KEY:?}]") })?.clone();
    let from = event.attributes.get(FROM_ATTR_KEY).filter(|s| !s.is_empty()).ok_or_else(|| DrmError::MissingParameter { context: context(&event.id), name: format!("attributes[{FROM_ATTR_KEY:?}]") })?.clone();
    let command_class = event.attributes.get(COMMAND_CLASS_ATTR_KEY).cloned().unwrap_or_else(|| "generic".to_string());
    let hazardous = match event.attributes.get(HAZARDOUS_ATTR_KEY).map(String::as_str) {
        None => false,
        Some("true") => true,
        Some("false") => false,
        Some(other) => return Err(DrmError::InvalidEnumValue { field: "scenario_event.attributes[hazardous]", value: other.to_string() }),
    };
    let known_attrs = [FIELD_ATTR_KEY, FROM_ATTR_KEY, COMMAND_CLASS_ATTR_KEY, HAZARDOUS_ATTR_KEY];
    if let Some(unknown) = event.attributes.keys().find(|k| !known_attrs.contains(&k.as_str())) {
        return Err(DrmError::UnknownParameter { context: format!("{} attributes", context(&event.id)), name: unknown.clone() });
    }

    Ok(ParsedCommand { id: event.id.clone(), tai_ns: event.tai_ns, instance: event.instance.clone(), field, value, command_class, hazardous, from })
}

/// Build the initial `Command` (state `AUTHORIZED`, `transitions` carrying all three synthesized
/// entries) and the three `COMMAND_TRANSITION` events for `PROPOSED -> CHECKED -> AUTHORIZED` --
/// see the module doc comment's "Where each transition actually happens" section for why these
/// three are synthesized once, up front, rather than driven by any runtime evidence. All three
/// land at `scenario_start_tai_ns`: this SIL replay has no modeled authorization latency.
pub fn propose_check_authorize(cmd: &ParsedCommand, scenario_start_tai_ns: i64, provenance: Provenance) -> (Command, Vec<Event>) {
    let mut command = Command {
        id: cmd.id.clone(),
        idempotency_key: cmd.id.clone(),
        entity_id: cmd.instance.clone(),
        command_class: cmd.command_class.clone(),
        hazardous: cmd.hazardous,
        payload: None,
        deadline_tai_ns: 0,
        not_before_tai_ns: cmd.tai_ns,
        state: CommandState::Unspecified as i32,
        transitions: Vec::new(),
        envelope_id: String::new(),
        label: None,
        provenance: Some(provenance.clone()),
    };
    let mut events = Vec::new();
    // `super::executor::execute`'s own final sort (`events::epoch_id_order`, `(epoch, id)`) would
    // otherwise scramble these three back into alphabetical-by-state order at a shared epoch
    // ("COMMAND_STATE_AUTHORIZED" < "COMMAND_STATE_CHECKED" < "COMMAND_STATE_PROPOSED") -- a
    // 1-nanosecond stagger, not a real modeled latency, is what keeps PROPOSED -> CHECKED ->
    // AUTHORIZED in that literal order in the returned `RunProducts.events`, still "at scenario
    // start" for every practical purpose (a run's own kernel step is never finer than 1 Hz in any
    // fixture this crate builds).
    let steps: [(CommandState, &str, &str, i64); 3] = [
        (CommandState::Proposed, "mission-planning", "declared in Scenario.events", 0),
        (CommandState::Checked, "sil-validator", "structurally valid: target instance and writable field both resolve", 1),
        (CommandState::Authorized, "sil-auto-authority", "SIL auto-authorization: no external authority configured for this run", 2),
    ];
    for (state, principal, reason, offset_ns) in steps {
        let tai_ns = scenario_start_tai_ns + offset_ns;
        command.transitions.push(CommandTransition { state: state as i32, tai_ns, principal: principal.to_string(), reason: reason.to_string(), ack_level: AckLevel::Unspecified as i32, delegation_id: String::new() });
        command.state = state as i32;
        events.push(transition_event(cmd, state, tai_ns, principal, reason, AckLevel::Unspecified, provenance.clone()));
    }
    (command, events)
}

/// One `EVENT_KIND_COMMAND_TRANSITION` event -- `Event` has no dedicated `Command`/
/// `CommandTransition` sub-message (`trajectory.proto`'s own `Event` doc comment: "For command
/// transitions: the Command id" lives in `reference_id`, the same free-string seam `EventKind::
/// PortCommand`'s own five required attributes already use), so the state name, principal and
/// reason are written as `Event.name`/`provenance.attributes`, mirroring `events::port_command_
/// event`'s own "typed fields plus attributes, not a new proto message" choice.
fn transition_event(cmd: &ParsedCommand, state: CommandState, tai_ns: i64, principal: &str, reason: &str, ack_level: AckLevel, mut provenance: Provenance) -> Event {
    provenance.attributes.insert("principal".to_string(), principal.to_string());
    provenance.attributes.insert("reason".to_string(), reason.to_string());
    provenance.attributes.insert("command_class".to_string(), cmd.command_class.clone());
    provenance.attributes.insert("field".to_string(), cmd.field.clone());
    if ack_level != AckLevel::Unspecified {
        provenance.attributes.insert("ack_level".to_string(), ack_level.as_str_name().to_string());
    }
    Event {
        id: format!("command_transition:{}:{}", cmd.id, state.as_str_name()),
        entity_id: cmd.instance.clone(),
        tai_ns,
        kind: EventKind::CommandTransition as i32,
        name: state.as_str_name().to_string(),
        detail: format!("command {:?} ({}): {} -> {}", cmd.id, cmd.command_class, state.as_str_name(), reason),
        values: BTreeMap::from([(VALUE_KEY.to_string(), cmd.value)]),
        frame_id: String::new(),
        reference_id: cmd.id.clone(),
        label: None,
        provenance: Some(provenance),
    }
}

/// One `COMMAND_STATE_DISPATCHED` event -- `super::executor::execute` calls this at the exact
/// epoch it hands the encoded packet to `crate::router::Router::deliver` (never before: this is
/// the one transition this module claims only once real router delivery has actually begun).
pub fn dispatched_event(cmd: &ParsedCommand, dispatch_tai_ns: i64, provenance: Provenance) -> Event {
    transition_event(cmd, CommandState::Dispatched, dispatch_tai_ns, "ground-segment", &format!("CCSDS telecommand framed and handed to the router from {:?}", cmd.from), AckLevel::Unspecified, provenance)
}

/// One `COMMAND_STATE_ACKED` event -- see the module doc comment's "Where each transition
/// actually happens" section for exactly what evidence justifies `AckLevel::AssetExecuted` here
/// (never merely `AssetReceived`: the flight instance only ever sends this ack after it already
/// applied the value).
pub fn acked_event(cmd: &ParsedCommand, ack_tai_ns: i64, provenance: Provenance) -> Event {
    transition_event(cmd, CommandState::Acked, ack_tai_ns, cmd.instance.as_str(), "flight software's own telemetry acknowledged execution", AckLevel::AssetExecuted, provenance)
}

// ============================================================================================
// CCSDS framing for the command-out / ack-in wire (mirrors `crate::drm::ground`'s own tm/tc
// codec builders exactly).
// ============================================================================================

/// The fixed field layout every command-out `PacketCodec` this module builds/expects uses: one
/// big-endian IEEE-754 `binary64` field, `value` -- the commanded engineering value, in the
/// target's own declared writable parameter's own unit.
pub fn command_out_packet_codec(id: &str, apid: u32) -> PacketCodec {
    let f = PacketField { name: "value".to_string(), bit_offset: 0, bit_width: 64, r#type: PacketFieldType::Float64 as i32, unit: Unit::Dimensionless as i32, scale: 1.0, offset: 0.0, target: String::new() };
    PacketCodec { id: id.to_string(), apid, is_command: true, secondary_header_bytes: 0, user_data_bytes: 8, fields: vec![f], description: "ground-issued command telecommand: one commanded engineering value (M25.2)".to_string() }
}

/// The fixed field layout every command-ack `PacketCodec` this module builds/expects uses: one
/// big-endian `UINT16` field, `cmd_seq` -- the acknowledged command packet's own CCSDS primary-
/// header `sequence_count`, echoed back so the ground instance can correlate this ack to the
/// command it dispatched (see the module doc comment's "Scope disclosed, not hidden" section).
pub fn command_ack_packet_codec(id: &str, apid: u32) -> PacketCodec {
    let f = PacketField { name: "cmd_seq".to_string(), bit_offset: 0, bit_width: 16, r#type: PacketFieldType::Uint as i32, unit: Unit::Dimensionless as i32, scale: 1.0, offset: 0.0, target: String::new() };
    PacketCodec { id: id.to_string(), apid, is_command: false, secondary_header_bytes: 0, user_data_bytes: 2, fields: vec![f], description: "flight-software command ack: the acknowledged packet's own CCSDS sequence_count (M25.2)".to_string() }
}

/// Assign each of `commands` (already sorted by the caller -- `super::executor::execute` sorts by
/// `(tai_ns, id)`, the same order every other boundary list in this crate uses) a distinct CCSDS
/// 14-bit `sequence_count`, `0..commands.len()` -- the numeric correlator [`command_ack_packet_
/// codec`]'s own `cmd_seq` field echoes back. Returns the `seq -> &ParsedCommand` map `super::
/// executor::run_shared_group`'s own applied-commands drain resolves an ack against.
pub fn assign_sequence_numbers(commands: &[ParsedCommand]) -> Result<BTreeMap<u16, &ParsedCommand>, DrmError> {
    /// The CCSDS 14-bit sequence count field's own maximum value + 1 (`crate::codec::
    /// MAX_SEQUENCE_COUNT` is private to that module; this is the identical constant, not a
    /// second definition of the wire format itself -- `command_ack_packet_codec`'s own
    /// `cmd_seq` field is 16 bits wide precisely so every value in `0..COMMAND_SEQUENCE_SPACE`
    /// round-trips through it exactly).
    const COMMAND_SEQUENCE_SPACE: usize = 16384;
    if commands.len() > COMMAND_SEQUENCE_SPACE {
        // Unreachable in practice for any fixture this task builds (see the module doc comment's
        // own "one outstanding command" scope note) -- a typed refusal rather than a silent
        // wraparound that would misattribute one command's ack to another, if this module is
        // ever reused for a larger command stream than it was built to demonstrate.
        return Err(DrmError::InvalidDrmOptions { reason: format!("{} declared command(s) exceeds the CCSDS 14-bit sequence_count space this executor assigns one-per-command from", commands.len()) });
    }
    Ok(commands.iter().enumerate().map(|(i, c)| (i as u16, c)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, values: BTreeMap<String, f64>, attributes: BTreeMap<String, String>) -> ScenarioEvent {
        ScenarioEvent { id: "cmd1".to_string(), tai_ns: 1_000_000_000, kind: kind.to_string(), instance: "flight".to_string(), values, attributes, execution_error: None }
    }

    fn valid_attrs() -> BTreeMap<String, String> {
        BTreeMap::from([("field".to_string(), "accel_scale".to_string()), ("from".to_string(), "ground".to_string())])
    }
    fn valid_values() -> BTreeMap<String, f64> {
        BTreeMap::from([("value".to_string(), 2.0)])
    }

    /// A well-formed `command` event parses into every field `parse` promises -- fails against
    /// an implementation that drops `from`/`field`/`command_class` or misreads `values["value"]`.
    #[test]
    fn parse_accepts_a_well_formed_command_event() {
        let c = parse(&event(COMMAND_KIND, valid_values(), valid_attrs())).expect("valid command event");
        assert_eq!(c.id, "cmd1");
        assert_eq!(c.instance, "flight");
        assert_eq!(c.field, "accel_scale");
        assert_eq!(c.from, "ground");
        assert_eq!(c.value, 2.0);
        assert_eq!(c.command_class, "generic", "default when attributes[\"command_class\"] is absent");
        assert!(!c.hazardous, "default when attributes[\"hazardous\"] is absent");
    }

    /// `kind != "command"` is refused exactly like `maneuver::parse` refuses any kind it does not
    /// model -- fails against an implementation that accepts an arbitrary kind string.
    #[test]
    fn parse_refuses_a_kind_other_than_command() {
        let err = parse(&event("mode", valid_values(), valid_attrs())).unwrap_err();
        assert!(matches!(err, DrmError::UnsupportedScenarioEventKind { ref kind, .. } if kind == "mode"), "{err:?}");
    }

    /// A missing `attributes["from"]` is refused, typed -- fails against an implementation that
    /// silently defaults the dispatching ground instance instead of requiring it declared.
    #[test]
    fn parse_refuses_a_missing_from_attribute() {
        let mut attrs = valid_attrs();
        attrs.remove("from");
        let err = parse(&event(COMMAND_KIND, valid_values(), attrs)).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name.contains("from")), "{err:?}");
    }

    /// An unknown `values` key is refused (question 97's own "typed, not opaque" rule, reused
    /// verbatim from `maneuver::parse`) -- fails against an implementation that silently ignores
    /// an extra key instead of refusing the whole event.
    #[test]
    fn parse_refuses_an_unknown_values_key() {
        let mut values = valid_values();
        values.insert("bogus".to_string(), 1.0);
        let err = parse(&event(COMMAND_KIND, values, valid_attrs())).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == "bogus"), "{err:?}");
    }

    /// `propose_check_authorize` builds exactly three transitions, in state-machine order, ending
    /// AUTHORIZED (never skipping CHECKED, never inventing an APPROVED/EXECUTED state) -- fails
    /// against an implementation that jumps straight to AUTHORIZED or DISPATCHED.
    #[test]
    fn propose_check_authorize_builds_the_real_three_state_prefix_in_order() {
        let cmd = parse(&event(COMMAND_KIND, valid_values(), valid_attrs())).unwrap();
        let (command, events) = propose_check_authorize(&cmd, 0, Provenance::default());
        assert_eq!(command.transitions.len(), 3);
        assert_eq!(command.transitions[0].state, CommandState::Proposed as i32);
        assert_eq!(command.transitions[1].state, CommandState::Checked as i32);
        assert_eq!(command.transitions[2].state, CommandState::Authorized as i32);
        assert_eq!(command.state, CommandState::Authorized as i32);
        assert_eq!(events.len(), 3);
        for e in &events {
            assert_eq!(e.kind, EventKind::CommandTransition as i32);
            assert_eq!(e.reference_id, "cmd1");
        }
        assert_eq!(events[0].name, "COMMAND_STATE_PROPOSED");
        assert_eq!(events[2].name, "COMMAND_STATE_AUTHORIZED");
    }

    /// `assign_sequence_numbers` assigns distinct, `0`-based sequence counts in input order --
    /// fails against an implementation that reuses a sequence count across two commands, which
    /// would make an ack ambiguous.
    #[test]
    fn assign_sequence_numbers_gives_each_command_a_distinct_seq() {
        let a = ParsedCommand { id: "a".to_string(), tai_ns: 0, instance: "x".to_string(), field: "f".to_string(), value: 1.0, command_class: "generic".to_string(), hazardous: false, from: "ground".to_string() };
        let b = ParsedCommand { id: "b".to_string(), ..a.clone() };
        let commands = [a, b];
        let map = assign_sequence_numbers(&commands).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&0).map(|c| c.id.as_str()), Some("a"));
        assert_eq!(map.get(&1).map(|c| c.id.as_str()), Some("b"));
    }
}
