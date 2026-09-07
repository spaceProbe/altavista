//! CDM `Event`s the DRM executor emits (`docs/open-questions.md` question 95). Every event this
//! module builds is real -- derived directly from something that actually happened during a run
//! -- never fabricated just to cover `EventKind`'s full vocabulary. See "Emitted" below for what
//! this executor actually produces, and "Not emitted" for the two kinds this task's own list
//! named that this crate has no honest way to produce today, and why.
//!
//! ## Emitted: `EVENT_KIND_LIFECYCLE` (run start / run end)
//!
//! One pair per `SystemInstance`, at that instance's own actual propagation start/end
//! (`t0_actual`/`run_end_tai_ns` -- `executor::run_plain_instance`'s and
//! `run_covariance_instance`'s own values, not the bare declared `Scenario.start_tai_ns`/
//! `end_tai_ns`, which a GMAT-bound instance's own epoch reconciliation can differ from by the
//! documented A1MJD round-trip tolerance -- see `binding`'s module doc comment). `entity_id` is
//! the instance name: `EventKind::EVENT_KIND_LIFECYCLE`'s own doc comment ("Track lifecycle...
//! projected onto an entity") ties a LIFECYCLE event to one entity, so a run with several
//! instances gets one pair per instance rather than one run-wide pair with no entity to record
//! it against (`Event` has no run-level, non-entity concept).
//!
//! ## Emitted: `EVENT_KIND_FAULT` (every `FAULT_TARGET_KIND_DYNAMICS` fault actually applied)
//!
//! One event per fault whose boundary actually falls inside the executed span
//! (`executor::run_plain_instance`'s own in-range check -- a fault outside
//! `[t0_actual, run_end_tai_ns)` has no effect on the run and gets no event, exactly like it gets
//! no segment split). `values` is the fault's own `Fault.params` map verbatim: `fault`'s own
//! module doc comment states a DYNAMICS/`"parameter"` fault draws no random element ("there is
//! nothing to draw" -- unlike PORT/SENSOR faults, which are seeded but never actually applied by
//! this executor at all, see `fault::realize_unapplied_fault`), so there is no distinction
//! between "declared nominal" and "realized" for this fault kind: the number this event carries
//! is exactly the number `fault::apply_dynamics_fault` used to reconfigure the model, not a
//! re-derivation of it. `reference_id` is `Fault.id` (`trajectory.proto`'s own `Event.
//! reference_id` doc comment: "for faults: the Fault id in the DRM"); `name` is also `Fault.id`
//! (the only human-facing identifier a `Fault` carries -- no separate `Fault.name` field exists),
//! so `event.<fault id>.t` resolves an applied fault's own epoch.
//!
//! ## Emitted: `EVENT_KIND_MANEUVER` (question 97, every applied `"maneuver"` `ScenarioEvent`)
//!
//! One event per `ScenarioEvent` of kind `"maneuver"` (`super::maneuver::parse`) whose epoch
//! actually falls inside the executed span -- the same in-range treatment `EVENT_KIND_FAULT`
//! already gets, see `executor::run_plain_instance`'s boundary loop. `values` carries the
//! *declared* delta-v components verbatim (`dv_x`/`dv_y`/`dv_z`, SI m/s, in the declared frame's
//! own basis order -- `maneuver::ParsedManeuver.dv`) plus `dv_mps`, the applied vector's
//! magnitude (frame-independent, so a reader does not have to know the frame to sanity-check the
//! burn size) -- `trajectory.proto`'s own `Event.values` doc comment gives `{"dv_mps": 20.0,
//! "dv_v": 20.0, "dv_n": 0, "dv_b": 0}` as an *example*, not a literal per-frame-name contract;
//! this executor uses the same generic `dv_x`/`dv_y`/`dv_z` names `ScenarioEvent.values` itself
//! declares them under (RIC/VVLH would otherwise need their own `dv_r`/`dv_i`/`dv_c` etc. key
//! set for no benefit -- the frame is already named by `frame_id`). `frame_id` is the declared
//! `AxesKind`'s own proto name (`"AXES_KIND_VNB"`, ...). `reference_id`/`name` are the
//! `ScenarioEvent.id` (mirrors `EVENT_KIND_FAULT`'s use of `Fault.id`).
//!
//! ## Emitted: `EVENT_KIND_MANEUVER`'s Gates-sampled record (question 100, M11.4)
//!
//! `executor::run_plain_instance` (the non-covariance path) samples a Gates-model realization
//! of any maneuver whose `execution_error` is declared (`maneuver::sample_execution_error`) --
//! see `maneuver`'s own module doc comment's "Burn execution error" section for the model
//! itself. [`maneuver_event`]'s `sampled` parameter, when `Some`, adds `applied_dv_x`/`_y`/`_z`
//! (the perturbed dv actually applied, declared-frame basis, same order as `dv_x`/`_y`/`_z`),
//! `applied_dv_mps` (its magnitude), and `draw_magnitude`/`draw_pointing_1`/`draw_pointing_2`
//! (the three raw standard normals `maneuver::sample_execution_error` drew) to `values` --
//! `trajectory.proto`'s `Event.values` doc comment names its `dv_mps`/`dv_v`/`dv_n`/`dv_b`
//! example non-binding, so this executor extends the same map with these additional keys rather
//! than inventing a second event kind. `None` (the covariance path, or a perfect burn) leaves
//! `values` exactly as it was before this task: the commanded vector and its magnitude only.
//!
//! ## Emitted: `EVENT_KIND_LIFECYCLE` (M14.4, in-flight messages dropped at run end)
//!
//! `crate::router::Router`'s own module doc comment: a message still queued (delivered or not
//! yet available) when the run's last span finishes is lost, not delivered late -- nothing calls
//! `Router::take_inbox` again once `run_shared_group` returns. Through M14.3 this was silent:
//! `crate::router::Router::has_pending` existed only for a test to assert against, and
//! `execute()` never read it at all. M14.4 closes that: `execute()` reads `Router::pending_count`
//! once the shared run finishes and, when it is non-zero, emits exactly one
//! `EVENT_KIND_LIFECYCLE` event naming it ([`dropped_messages_event`]) in addition to recording
//! the count unconditionally on `RunProducts.provenance.attributes["dropped_in_flight_messages"]`
//! (`executor::build_run_provenance`) -- so a caller reading only `RunProducts.provenance` can
//! tell a clean run (`"0"`, no event) from a lossy one without scanning every instance's own
//! router wiring by hand, and a caller scanning `RunProducts.events` for `EVENT_KIND_LIFECYCLE`
//! sees it too. `entity_id` is deliberately empty: unlike every other `EVENT_KIND_LIFECYCLE`
//! event this module emits (`lifecycle_pair`, one instance's own run start/end), a dropped
//! message is not about any single instance's own lifecycle -- it is the run's own outcome, and a
//! message dropped for one receiver was still emitted by some other instance, so there is no
//! single entity this event could honestly be tied to instead.
//!
//! ## Emitted: `EVENT_KIND_PORT_COMMAND` (question 130, M19.3)
//!
//! `gmat_sys::model::GmatModel::step_with_ports`'s `consume` path writes a SIGNAL-commanded
//! value straight into the live GMAT object via `DerivativeModel::set_real_parameter`, before
//! `dynamics_hash` is ever recomputed -- so the write bypasses the hashed configuration
//! `binding::gmat_settings` covers, and equal `dynamics_hash` no longer implied equal dynamics
//! (`docs/open-questions.md` question 130). The lead's decision: `dynamics_hash` stays the
//! configuration hash, and no segment opens per command -- instead, every command a
//! `step_with_ports` call actually applies (never merely "arrived on a port," see
//! `av_dynamics::AppliedCommand`'s own doc comment) is recorded as one `EVENT_KIND_PORT_COMMAND`
//! event ([`port_command_event`]), so "equal `dynamics_hash` means equal configuration; equal
//! configuration plus equal command events means equal dynamics." `executor::run_shared_group`
//! drains `crate::schedule::HeteroScheduler::applied_commands` (via `HeteroKernel`) once per span,
//! for every model-bound instance, and `executor::execute` turns each into an event once the whole
//! run finishes -- see that function's own "event_ids" pass for how each ends up referenced from
//! its own instance's `Trajectory.event_ids` too, the same as every other per-instance event this
//! module emits (`entity_id` is always the applying instance).
//!
//! **M20.3 (`docs/open-questions.md` question 137) narrows "actually applies" to "actually
//! applies AND changes the value."** M19.3's own "every command a `step_with_ports` call
//! actually applies" produced one `EVENT_KIND_PORT_COMMAND` event per *step*, not per genuine
//! command -- a parameter commanded continuously (a 10 Hz stream over 2 h) produced 71,999
//! events and a 42 MB bundle. The lead's decision: a command whose value equals the last
//! applied value for that (instance, parameter) emits no event, and the first application
//! always does. This module itself needed no change for it -- `gmat_sys::model::GmatModel::
//! step_with_ports` (the sole producer of `av_dynamics::AppliedCommand` in this workspace)
//! already omits an unchanged command from its own returned `Vec<AppliedCommand>`, so nothing
//! this executor drains, sorts, or turns into an `Event` here was ever aware the command was
//! suppressed -- every element of `ModelSpanState::applied_commands` this module ever sees is
//! already exactly the "record" bar, both before and after M20.3.
//!
//! ## Emitted: `EVENT_KIND_CONTACT_START`/`_END` (M25.1, question 149's ground segment)
//!
//! [`contact_event`]: a ground-station visibility transition, reported through the identical
//! applied-commands channel `EVENT_KIND_PORT_COMMAND` already uses -- see that function's own doc
//! comment and `crate::drm::ground`'s own module doc comment ("Contact windows and the elevation
//! mask") for exactly how a `crate::drm::ground::GroundStationModel::step_with_ports` transition
//! reaches this function rather than [`port_command_event`].
//!
//! ## Not emitted: `EVENT_KIND_MODE_CHANGE` ("instance step-rate changes")
//!
//! An instance's effective step rate (`executor::effective_step_rate_hz`) is computed once, at
//! load, from `SystemInstance.step_rate_hz`/`DrmOptions.default_step_rate_hz`, and used as the
//! single `period_ns` for every fault-bounded segment of that instance's run
//! (`executor::run_plain_instance`'s `period_ns` parameter is threaded through unchanged across
//! every `run_span` call, even across a DYNAMICS fault boundary that rebinds the model itself).
//! No `Fault.target` vocabulary (`fault::apply_gmat_target`/`apply_dynamics_fault`) names a
//! step-rate field, and no other mechanism in this crate changes an instance's step rate
//! mid-run -- a step-rate change never occurs at runtime today, so there is nothing real to
//! report, and this executor emits no `MODE_CHANGE` events.

use std::collections::BTreeMap;

use av_cdm::pb::{AuthorKind, Event, EventKind, Fault, FaultTargetKind, Provenance, Scenario};

use super::fault;
use super::maneuver::{ExecutionErrorMode, ParsedManeuver};

/// `Event.provenance` for every event this module builds -- the same shape
/// `executor::finish_trajectory`'s per-instance `Provenance` already uses (`config_hash` = the
/// `SosConfiguration`'s own hash, `attributes` names the `SystemDefinition`), since every event
/// this executor emits is tied to exactly one instance. `created_tai_ns` stays `0` (this crate
/// never reads the wall clock -- ADR-002/ADR-004 determinism, `executor`'s own module doc
/// comment's "Provenance" section).
pub fn event_provenance(sos_hash: &str, data_pack_hash: &str, run_id: &str, sys_hash: &str, sys_id: &str) -> Provenance {
    Provenance {
        author_kind: AuthorKind::Service as i32,
        principal: String::new(),
        tool: "av-kernel::drm::executor".to_string(),
        config_hash: sos_hash.to_string(),
        data_pack_hash: data_pack_hash.to_string(),
        dataset_hash: String::new(),
        created_tai_ns: 0,
        run_id: run_id.to_string(),
        attributes: BTreeMap::from([("system_definition_hash".to_string(), sys_hash.to_string()), ("system_definition_id".to_string(), sys_id.to_string())]),
    }
}

/// One `EVENT_KIND_LIFECYCLE` event -- `name` is `"run_start"`/`"run_end"` (see
/// [`lifecycle_pair`], this function's only caller besides [`declared_events`]).
fn lifecycle_event(instance: &str, tai_ns: i64, name: &str, provenance: Provenance) -> Event {
    Event {
        id: format!("lifecycle:{instance}:{name}"),
        entity_id: instance.to_string(),
        tai_ns,
        kind: EventKind::Lifecycle as i32,
        name: name.to_string(),
        detail: format!("instance {instance:?} {name}"),
        provenance: Some(provenance),
        ..Default::default()
    }
}

/// Both `EVENT_KIND_LIFECYCLE` events for one instance's run -- `"run_start"` at
/// `start_tai_ns`, `"run_end"` at `end_tai_ns` (see the module doc comment's "Emitted:
/// EVENT_KIND_LIFECYCLE" section).
pub fn lifecycle_pair(instance: &str, start_tai_ns: i64, end_tai_ns: i64, provenance: Provenance) -> [Event; 2] {
    [lifecycle_event(instance, start_tai_ns, "run_start", provenance.clone()), lifecycle_event(instance, end_tai_ns, "run_end", provenance)]
}

/// [`event_provenance`]'s run-level counterpart (M14.4): every `Provenance` field
/// `event_provenance` sets except `attributes` (which names a *system*, and
/// [`dropped_messages_event`] is not about any one instance's own system -- see this module's own
/// doc comment's "Emitted: EVENT_KIND_LIFECYCLE (M14.4 ...)" section).
fn run_provenance(sos_hash: &str, data_pack_hash: &str, run_id: &str) -> Provenance {
    Provenance {
        author_kind: AuthorKind::Service as i32,
        principal: String::new(),
        tool: "av-kernel::drm::executor".to_string(),
        config_hash: sos_hash.to_string(),
        data_pack_hash: data_pack_hash.to_string(),
        dataset_hash: String::new(),
        created_tai_ns: 0,
        run_id: run_id.to_string(),
        attributes: BTreeMap::new(),
    }
}

/// One `EVENT_KIND_LIFECYCLE` event recording that `dropped_count` in-flight port message(s)
/// were still queued, undelivered, when the run ended -- see this module's own doc comment's
/// "Emitted: EVENT_KIND_LIFECYCLE (M14.4 ...)" section. Only ever called when `dropped_count >
/// 0` (`executor::execute`'s own call site) -- there is no "zero dropped" event, by design (the
/// module doc comment's "no silent drop" claim is about the *count* always reaching provenance,
/// not about an event firing unconditionally). `tai_ns` is `run_end_tai_ns`: the drop is only
/// ever discovered once the run's own last span has finished and nothing will call `Router::
/// take_inbox` again.
pub fn dropped_messages_event(run_end_tai_ns: i64, dropped_count: usize, sos_hash: &str, data_pack_hash: &str, run_id: &str) -> Event {
    Event {
        id: "lifecycle:run:dropped_in_flight_messages".to_string(),
        entity_id: String::new(),
        tai_ns: run_end_tai_ns,
        kind: EventKind::Lifecycle as i32,
        name: "dropped_in_flight_messages".to_string(),
        detail: format!("{dropped_count} in-flight port message(s) still queued when the run ended; never delivered"),
        values: BTreeMap::from([("dropped_count".to_string(), dropped_count as f64)]),
        provenance: Some(run_provenance(sos_hash, data_pack_hash, run_id)),
        ..Default::default()
    }
}

/// One `EVENT_KIND_PORT_COMMAND` event for a port command a bound instance's `step_with_ports`
/// call actually applied to its own bound configuration -- see the module doc comment's
/// "Emitted: EVENT_KIND_PORT_COMMAND" section. `cmd` is the enriched, kernel-level record
/// (`crate::ports::AppliedPortCommand`) `crate::schedule::HeteroScheduler::
/// advance_to_with_ports` built, itself derived from the `av_dynamics::AppliedCommand`
/// `gmat_sys::model::GmatModel::step_with_ports` returned.
///
/// **Where the five required attributes live** (`docs/open-questions.md` question 130: "the
/// instance, the parameter, the value, the epoch and the sender in the event's attributes"):
/// `Event` itself has no generic string-keyed attribute map (only `values`, `map<string,
/// double>`, and `provenance.attributes`, `map<string, string>` -- `trajectory.proto`'s own
/// `Provenance.attributes` doc comment, "free-form"), so all five are written there, as
/// strings, exactly as named -- `"instance"`/`"parameter"`/`"value"`/`"epoch"`/`"sender"` --
/// the literal, greppable answer to the question's own wording. `sender` is the empty string
/// when the router could not attribute one (never true for a real `execute()` run: every
/// `Inbox` that reaches a bound instance there comes from `crate::router::Router`, which always
/// knows the sender of anything it queued -- see `AppliedPortCommand`'s own doc comment).
/// Alongside those five string attributes, the same information also lands on `Event`'s own
/// typed fields for a consumer that wants it typed rather than parsed: `entity_id` (instance),
/// `tai_ns` (epoch), `values["value"]` (value, as an actual `f64`, not just its string form),
/// `name` (the commanded parameter -- mirrors `fault_event`/`maneuver_event`'s own choice of
/// the one human-facing identifier the underlying thing carries), and `reference_id` (the port
/// name).
pub fn port_command_event(cmd: &crate::ports::AppliedPortCommand, mut provenance: Provenance) -> Event {
    let sender = cmd.sender.clone().unwrap_or_default();
    provenance.attributes.insert("instance".to_string(), cmd.instance.clone());
    provenance.attributes.insert("parameter".to_string(), cmd.field.clone());
    provenance.attributes.insert("value".to_string(), cmd.value.to_string());
    provenance.attributes.insert("epoch".to_string(), cmd.applied_tai_ns.to_string());
    provenance.attributes.insert("sender".to_string(), sender.clone());
    Event {
        id: format!("port_command:{}:{}:{}", cmd.instance, cmd.port, cmd.applied_tai_ns),
        entity_id: cmd.instance.clone(),
        tai_ns: cmd.applied_tai_ns,
        kind: EventKind::PortCommand as i32,
        name: cmd.field.clone(),
        detail: format!("port {:?}: {} = {} (sender {:?})", cmd.port, cmd.field, cmd.value, sender),
        values: BTreeMap::from([("value".to_string(), cmd.value)]),
        reference_id: cmd.port.clone(),
        provenance: Some(provenance),
        ..Default::default()
    }
}

/// One `EVENT_KIND_FAULT` event for a `FAULT_TARGET_KIND_DYNAMICS` fault actually applied -- see
/// the module doc comment's "Emitted: EVENT_KIND_FAULT" section for why `values` is
/// `fault.params` verbatim.
pub fn fault_event(fault: &Fault, instance: &str, provenance: Provenance) -> Event {
    Event {
        id: format!("fault:{}", fault.id),
        entity_id: instance.to_string(),
        tai_ns: fault.tai_ns,
        kind: EventKind::Fault as i32,
        name: fault.id.clone(),
        detail: format!("FAULT_TARGET_KIND_DYNAMICS fault {:?} applied: target={:?}, kind={:?}", fault.id, fault.target, fault.kind),
        values: fault.params.clone(),
        reference_id: fault.id.clone(),
        provenance: Some(provenance),
        ..Default::default()
    }
}

/// M25.1 (`docs/sil-plan.md`'s M25 milestone; `EVENT_KIND_CONTACT_START`/`_END` already exist in
/// `trajectory.proto`): a ground-station visibility transition (`crate::drm::ground::
/// GroundStationModel::step_with_ports`'s own reserved [`crate::drm::ground::
/// CONTACT_TRANSITION_PORT`] `AppliedCommand`) becomes one `Event`, the same "an applied command
/// becomes a CDM event, once, at the point it is actually applied" pipeline [`port_command_event`]
/// already uses for `EVENT_KIND_PORT_COMMAND` -- `executor::execute`'s own applied-commands drain
/// dispatches to this function instead of [`port_command_event`] whenever `cmd.port ==
/// crate::drm::ground::CONTACT_TRANSITION_PORT`, rather than growing `av_dynamics::AppliedCommand`
/// with a new field only one model needs (see `crate::drm::ground`'s own module doc comment,
/// "Contact windows and the elevation mask", for why the reserved port name is the seam).
/// `entity_id` is the receiving (ground) instance -- `cmd.instance`, the same field
/// [`port_command_event`] already uses.
pub fn contact_event(cmd: &crate::ports::AppliedPortCommand, mut provenance: Provenance) -> Event {
    let is_start = cmd.value == crate::drm::ground::CONTACT_START_VALUE;
    let kind = if is_start { EventKind::ContactStart } else { EventKind::ContactEnd };
    let name = if is_start { "contact_start" } else { "contact_end" };
    let sender = cmd.sender.clone().unwrap_or_default();
    provenance.attributes.insert("instance".to_string(), cmd.instance.clone());
    provenance.attributes.insert("sender".to_string(), sender.clone());
    Event {
        id: format!("contact:{}:{}:{}", cmd.instance, cmd.applied_tai_ns, name),
        entity_id: cmd.instance.clone(),
        tai_ns: cmd.applied_tai_ns,
        kind: kind as i32,
        name: name.to_string(),
        detail: format!("ground instance {:?}: {} (target {:?})", cmd.instance, name, sender),
        values: BTreeMap::new(),
        reference_id: String::new(),
        provenance: Some(provenance),
        ..Default::default()
    }
}

/// A sampled Gates-model realization of one maneuver (question 100, M11.4) -- the applied
/// (perturbed) dv `maneuver::sample_execution_error` computed, and the three raw standard
/// normals it drew, to be recorded on the MANEUVER event alongside the commanded vector. See
/// this module's own doc comment's "Emitted: EVENT_KIND_MANEUVER's Gates-sampled record"
/// section.
pub struct SampledManeuver {
    /// Declared-frame basis, same order as `ParsedManeuver.dv`.
    pub applied_dv: [f64; 3],
    /// `[z_magnitude, z_pointing_1, z_pointing_2]`.
    pub draws: [f64; 3],
}

fn norm3(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// One `EVENT_KIND_MANEUVER` event for a `"maneuver"` `ScenarioEvent` actually applied (question
/// 97) -- see the module doc comment's "Emitted: EVENT_KIND_MANEUVER" section. `applied_tai_ns`
/// is the epoch the burn was actually applied at (a GMAT-bound instance's own reconciled epoch,
/// like `EVENT_KIND_FAULT`'s `Fault.tai_ns`), not necessarily the bare declared value. `sampled`
/// (question 100) is `Some` only under [`ExecutionErrorMode::Sampled`] when `m.execution_error`
/// is declared -- see this module's own doc comment's "Emitted: EVENT_KIND_MANEUVER's
/// Gates-sampled record" section for exactly what it adds to `values`. `mode` (question 103) is
/// recorded on `provenance.attributes["execution_error_mode"]` -- "provenance records the mode"
/// applies to every burn, not only one with a declared `execution_error` block, so a reader can
/// tell a `Nominal` run's *absence* of an `applied_dv_*`/`draw_*` key apart from a `Sampled` run
/// whose block happened to be absent (both leave `values` identical, but the attribute
/// disambiguates why).
pub fn maneuver_event(m: &ParsedManeuver, instance: &str, applied_tai_ns: i64, sampled: Option<&SampledManeuver>, mode: ExecutionErrorMode, mut provenance: Provenance) -> Event {
    let mag = norm3(m.dv);
    let mut values = BTreeMap::from([("dv_x".to_string(), m.dv[0]), ("dv_y".to_string(), m.dv[1]), ("dv_z".to_string(), m.dv[2]), ("dv_mps".to_string(), mag)]);
    let mut detail = format!("dv = {mag:.2} m/s ({})", m.axes.as_str_name());
    if let Some(s) = sampled {
        let applied_mag = norm3(s.applied_dv);
        values.insert("applied_dv_x".to_string(), s.applied_dv[0]);
        values.insert("applied_dv_y".to_string(), s.applied_dv[1]);
        values.insert("applied_dv_z".to_string(), s.applied_dv[2]);
        values.insert("applied_dv_mps".to_string(), applied_mag);
        values.insert("draw_magnitude".to_string(), s.draws[0]);
        values.insert("draw_pointing_1".to_string(), s.draws[1]);
        values.insert("draw_pointing_2".to_string(), s.draws[2]);
        detail = format!("{detail}; Gates-sampled applied dv = {applied_mag:.6} m/s (commanded {mag:.6} m/s)");
    }
    provenance.attributes.insert("execution_error_mode".to_string(), mode.as_str().to_string());
    Event {
        id: format!("maneuver:{}", m.id),
        entity_id: instance.to_string(),
        tai_ns: applied_tai_ns,
        kind: EventKind::Maneuver as i32,
        name: m.id.clone(),
        detail,
        values,
        frame_id: m.axes.as_str_name().to_string(),
        reference_id: m.id.clone(),
        provenance: Some(provenance),
        ..Default::default()
    }
}

/// Deterministic ordering (ADR-005 sec 5's `(epoch, id)` rule -- `fault::epoch_id_order`'s own
/// tie-break, applied here to `Event` instead of `Fault`): `executor::execute` sorts every
/// event this crate emits with `all_events.sort_by_key(events::epoch_id_order)` before returning
/// `RunProducts.events`.
pub fn epoch_id_order(event: &Event) -> (i64, String) {
    (event.tai_ns, event.id.clone())
}

/// The *declared* (load-time, pre-propagation) shape of every event this executor's real run
/// will emit -- same `id`/`name`/`kind`/`entity_id` [`lifecycle_pair`]/[`fault_event`]/
/// [`maneuver_event`] build for real (see the module doc comment's "Emitted" sections), `tai_ns`
/// approximated directly from `Scenario.start_tai_ns`/`end_tai_ns` or the declared event's own
/// `tai_ns` (a GMAT-bound instance's own epoch reconciliation can differ from the declared value
/// by the documented A1MJD round-trip tolerance -- immaterial here, since
/// `crate::expr::typecheck::check` only ever checks an event's `name`/`kind` *exist* through
/// `ExprRunProducts::event_epoch_seconds`/`count_events_by_kind`, never its `tai_ns` -- neither
/// bounds-checks). `provenance` is left unset (`None`): load-time typecheck never reads it. Used
/// only so `execute()`'s load-time expression validation (`executor::validate_expression_at_load`)
/// sees the same events, by name/kind, the real run will produce -- exactly the same "shape
/// only, not real values" property `executor::declared_shape_trajectory` already gives
/// `Trajectory`. `maneuvers` is every `Scenario.events` entry already parsed by
/// `maneuver::parse` (`executor::execute`'s own up-front validation pass -- see that function),
/// not re-parsed here.
pub fn declared_events(scenario: &Scenario, instance_names: &[String], maneuvers: &[ParsedManeuver], mode: ExecutionErrorMode) -> Vec<Event> {
    let mut events = Vec::new();
    for instance in instance_names {
        events.extend(lifecycle_pair(instance, scenario.start_tai_ns, scenario.end_tai_ns, Provenance::default()));
    }
    for f in &scenario.faults {
        // M16.2 (question 120): a real run also emits a FAULT event for a container power
        // cycle, which is `FAULT_TARGET_KIND_HARDWARE` now, not `_DYNAMICS`
        // (`executor::run_shared_group`'s own boundary loop, `fault::is_container_power_cycle`).
        // By the time `declared_events` runs, `executor::execute`'s own load-time validation has
        // already refused every other HARDWARE shape and every legacy DYNAMICS/"power_cycle"
        // fault, so `fault::is_container_power_cycle` here can only ever match the one HARDWARE
        // shape that really does reach a real run -- without this, a `MeasureOfEffectiveness`
        // expression naming this event by name would fail load-time typecheck even though the
        // real run goes on to produce it.
        if f.target_kind != FaultTargetKind::Dynamics as i32 && !fault::is_container_power_cycle(f) {
            continue;
        }
        if f.tai_ns <= scenario.start_tai_ns || f.tai_ns >= scenario.end_tai_ns {
            // Outside the executed span -- never actually applied (see
            // `executor::run_plain_instance`'s own identical in-range check), so no event.
            continue;
        }
        events.push(fault_event(f, &f.instance, Provenance::default()));
    }
    for m in maneuvers {
        if m.tai_ns <= scenario.start_tai_ns || m.tai_ns >= scenario.end_tai_ns {
            // Outside the executed span -- never actually applied (mirrors the fault check
            // above and `executor`'s own boundary loop), so no event.
            continue;
        }
        events.push(maneuver_event(m, &m.instance, m.tai_ns, None, mode, Provenance::default()));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_pair_has_matching_instance_and_start_before_end() {
        let pair = lifecycle_pair("veh", 0, 10_000_000_000, Provenance::default());
        assert_eq!(pair[0].name, "run_start");
        assert_eq!(pair[1].name, "run_end");
        assert!(pair.iter().all(|e| e.entity_id == "veh" && e.kind == EventKind::Lifecycle as i32));
        assert_eq!(pair[0].tai_ns, 0);
        assert_eq!(pair[1].tai_ns, 10_000_000_000);
    }

    /// M14.4: [`dropped_messages_event`] carries the real count in `values["dropped_count"]`
    /// (not merely a boolean-shaped 0/1) and ties to no single instance (`entity_id` empty) --
    /// would catch an implementation that hardcoded `1` regardless of the actual count, or one
    /// that attached the event to some arbitrary instance's own `entity_id` instead of leaving
    /// it a genuine run-level event.
    #[test]
    fn dropped_messages_event_carries_the_real_count_and_no_single_entity() {
        let e = dropped_messages_event(10_000_000_000, 3, "sos_hash", "data_pack_hash", "run-1");
        assert_eq!(e.kind, EventKind::Lifecycle as i32);
        assert_eq!(e.entity_id, "", "not tied to any single instance");
        assert_eq!(e.tai_ns, 10_000_000_000);
        assert_eq!(e.name, "dropped_in_flight_messages");
        assert_eq!(e.values.get("dropped_count"), Some(&3.0));
        assert!(e.detail.contains('3'), "{:?}", e.detail);
        assert_eq!(e.provenance.as_ref().unwrap().run_id, "run-1");
    }

    #[test]
    fn fault_event_carries_the_fault_id_and_its_params_verbatim() {
        let fault = Fault { id: "f1".to_string(), tai_ns: 5_000_000_000, target: "accel.x".to_string(), kind: "parameter".to_string(), params: BTreeMap::from([("value".to_string(), 5.0)]), ..Default::default() };
        let e = fault_event(&fault, "veh", Provenance::default());
        assert_eq!(e.id, "fault:f1");
        assert_eq!(e.name, "f1");
        assert_eq!(e.reference_id, "f1");
        assert_eq!(e.kind, EventKind::Fault as i32);
        assert_eq!(e.tai_ns, 5_000_000_000);
        assert_eq!(e.values.get("value"), Some(&5.0));
    }

    /// [`port_command_event`]'s own unit test (question 130): every one of the five required
    /// attributes (instance, parameter, value, epoch, sender) lands in `provenance.attributes`
    /// under its own literal name, plus the typed fields a real consumer would actually want.
    /// Fails against: an implementation that puts fewer than all five string attributes on
    /// `provenance.attributes` (e.g. omitting `sender` or `epoch`); one that mislabels `kind`
    /// (would not be `EventKind::PortCommand`); or one that fabricates a non-empty sender when
    /// `AppliedPortCommand.sender` is `None`.
    #[test]
    fn port_command_event_carries_all_five_required_attributes() {
        let cmd = crate::ports::AppliedPortCommand {
            instance: "demo_flt".to_string(),
            port: "cd_cmd_in".to_string(),
            field: "Cd".to_string(),
            value: 4.4,
            applied_tai_ns: 3_600_000_000_000,
            sender: Some("demo_mvr".to_string()),
        };
        let e = port_command_event(&cmd, Provenance::default());
        assert_eq!(e.kind, EventKind::PortCommand as i32);
        assert_eq!(e.entity_id, "demo_flt");
        assert_eq!(e.tai_ns, 3_600_000_000_000);
        assert_eq!(e.name, "Cd");
        assert_eq!(e.reference_id, "cd_cmd_in");
        assert_eq!(e.values.get("value"), Some(&4.4));

        let attrs = &e.provenance.expect("port_command_event always sets provenance").attributes;
        assert_eq!(attrs.get("instance"), Some(&"demo_flt".to_string()));
        assert_eq!(attrs.get("parameter"), Some(&"Cd".to_string()));
        assert_eq!(attrs.get("value"), Some(&"4.4".to_string()));
        assert_eq!(attrs.get("epoch"), Some(&"3600000000000".to_string()));
        assert_eq!(attrs.get("sender"), Some(&"demo_mvr".to_string()));
    }

    #[test]
    fn port_command_event_reports_an_empty_sender_string_when_the_sender_is_unknown() {
        let cmd = crate::ports::AppliedPortCommand {
            instance: "solo".to_string(),
            port: "cd_cmd_in".to_string(),
            field: "Cd".to_string(),
            value: 2.2,
            applied_tai_ns: 0,
            sender: None,
        };
        let e = port_command_event(&cmd, Provenance::default());
        assert_eq!(
            e.provenance.unwrap().attributes.get("sender"),
            Some(&String::new()),
            "an unknown sender must be reported honestly (empty string), never a fabricated instance name"
        );
    }

    #[test]
    fn epoch_id_order_sorts_by_epoch_then_by_id() {
        let mut events = [
            Event { id: "b".to_string(), tai_ns: 100, ..Default::default() },
            Event { id: "a".to_string(), tai_ns: 100, ..Default::default() },
            Event { id: "z".to_string(), tai_ns: 50, ..Default::default() },
        ];
        events.sort_by_key(epoch_id_order);
        let order: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, ["z", "a", "b"]);
    }

    #[test]
    fn declared_events_skips_a_fault_outside_the_executed_span() {
        let scenario = Scenario {
            start_tai_ns: 0,
            end_tai_ns: 10_000_000_000,
            faults: vec![
                Fault { id: "in_range".to_string(), tai_ns: 5_000_000_000, target_kind: FaultTargetKind::Dynamics as i32, instance: "veh".to_string(), ..Default::default() },
                Fault { id: "before_start".to_string(), tai_ns: -1, target_kind: FaultTargetKind::Dynamics as i32, instance: "veh".to_string(), ..Default::default() },
                Fault { id: "at_or_after_end".to_string(), tai_ns: 10_000_000_000, target_kind: FaultTargetKind::Dynamics as i32, instance: "veh".to_string(), ..Default::default() },
                Fault { id: "not_dynamics".to_string(), tai_ns: 5_000_000_000, target_kind: FaultTargetKind::Port as i32, instance: "veh".to_string(), ..Default::default() },
            ],
            ..Default::default()
        };
        let events = declared_events(&scenario, &["veh".to_string()], &[], ExecutionErrorMode::Nominal);
        // Two lifecycle events (run_start/run_end) plus exactly the one in-range DYNAMICS fault.
        assert_eq!(events.len(), 3);
        assert!(events.iter().any(|e| e.name == "in_range" && e.kind == EventKind::Fault as i32));
        assert!(!events.iter().any(|e| e.name == "before_start" || e.name == "at_or_after_end" || e.name == "not_dynamics"));
    }

    /// M16.2 (question 120): a container power cycle is `FAULT_TARGET_KIND_HARDWARE` now, and a
    /// real run still emits a FAULT event for it (`executor::run_shared_group`'s own boundary
    /// loop) -- `declared_events` must include it too, or a `MeasureOfEffectiveness` expression
    /// naming it would fail load-time typecheck even though the real run goes on to produce the
    /// event. Would fail against the pre-M16.2 filter (`target_kind != Dynamics` alone skips
    /// every HARDWARE fault, this one included) and equally against a filter that accepted every
    /// HARDWARE fault indiscriminately (the `not_power_cycle_hardware` fixture below would then
    /// wrongly appear too, since `executor::execute`'s own load-time validation -- not exercised
    /// by this unit test -- is what would normally have refused that shape before `declared_events`
    /// ever saw it).
    #[test]
    fn declared_events_includes_a_container_power_cycle_hardware_fault() {
        let scenario = Scenario {
            start_tai_ns: 0,
            end_tai_ns: 10_000_000_000,
            faults: vec![
                Fault { id: "power_cycle".to_string(), tai_ns: 5_000_000_000, target_kind: FaultTargetKind::Hardware as i32, kind: "power_cycle".to_string(), instance: "sig".to_string(), ..Default::default() },
                Fault { id: "not_power_cycle_hardware".to_string(), tai_ns: 5_000_000_000, target_kind: FaultTargetKind::Hardware as i32, kind: "board_reset".to_string(), instance: "sig".to_string(), ..Default::default() },
            ],
            ..Default::default()
        };
        let events = declared_events(&scenario, &["sig".to_string()], &[], ExecutionErrorMode::Nominal);
        assert!(events.iter().any(|e| e.name == "power_cycle" && e.kind == EventKind::Fault as i32), "{events:?}");
        assert!(!events.iter().any(|e| e.name == "not_power_cycle_hardware"), "{events:?}");
    }

    #[test]
    fn maneuver_event_carries_the_declared_dv_and_frame() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 5_000_000_000, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: av_cdm::pb::AxesKind::Vnb, execution_error: None };
        let e = maneuver_event(&m, "leo", 5_000_000_000, None, ExecutionErrorMode::Nominal, Provenance::default());
        assert_eq!(e.id, "maneuver:burn1");
        assert_eq!(e.name, "burn1");
        assert_eq!(e.reference_id, "burn1");
        assert_eq!(e.kind, EventKind::Maneuver as i32);
        assert_eq!(e.frame_id, "AXES_KIND_VNB");
        assert_eq!(e.values.get("dv_x"), Some(&20.0));
        assert_eq!(e.values.get("dv_mps"), Some(&20.0));
        assert!(!e.values.contains_key("applied_dv_x"), "no sampled realization -> no applied_dv_* keys");
    }

    #[test]
    fn maneuver_event_with_a_sampled_realization_adds_the_applied_vector_and_draws() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 5_000_000_000, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: av_cdm::pb::AxesKind::Vnb, execution_error: None };
        let sampled = SampledManeuver { applied_dv: [20.5, 0.1, -0.2], draws: [0.5, 0.1, -0.2] };
        let e = maneuver_event(&m, "leo", 5_000_000_000, Some(&sampled), ExecutionErrorMode::Sampled, Provenance::default());
        assert_eq!(e.values.get("dv_x"), Some(&20.0), "commanded vector is still recorded, unchanged");
        assert_eq!(e.values.get("applied_dv_x"), Some(&20.5));
        assert_eq!(e.values.get("applied_dv_y"), Some(&0.1));
        assert_eq!(e.values.get("applied_dv_z"), Some(&-0.2));
        assert_eq!(e.values.get("draw_magnitude"), Some(&0.5));
        assert_eq!(e.values.get("draw_pointing_1"), Some(&0.1));
        assert_eq!(e.values.get("draw_pointing_2"), Some(&-0.2));
    }

    #[test]
    fn maneuver_event_records_the_execution_error_mode_on_provenance_attributes() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 5_000_000_000, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: av_cdm::pb::AxesKind::Vnb, execution_error: None };
        let nominal = maneuver_event(&m, "leo", 5_000_000_000, None, ExecutionErrorMode::Nominal, Provenance::default());
        let sampled = maneuver_event(&m, "leo", 5_000_000_000, None, ExecutionErrorMode::Sampled, Provenance::default());
        assert_eq!(nominal.provenance.unwrap().attributes.get("execution_error_mode"), Some(&"EXECUTION_ERROR_MODE_NOMINAL".to_string()));
        assert_eq!(sampled.provenance.unwrap().attributes.get("execution_error_mode"), Some(&"EXECUTION_ERROR_MODE_SAMPLED".to_string()));
    }

    #[test]
    fn declared_events_skips_a_maneuver_outside_the_executed_span_and_includes_one_in_range() {
        let scenario = Scenario { start_tai_ns: 0, end_tai_ns: 10_000_000_000, ..Default::default() };
        let maneuvers = vec![
            ParsedManeuver { id: "in_range".to_string(), tai_ns: 5_000_000_000, instance: "veh".to_string(), dv: [1.0, 0.0, 0.0], axes: av_cdm::pb::AxesKind::Vnb, execution_error: None },
            ParsedManeuver { id: "before_start".to_string(), tai_ns: -1, instance: "veh".to_string(), dv: [1.0, 0.0, 0.0], axes: av_cdm::pb::AxesKind::Vnb, execution_error: None },
        ];
        let events = declared_events(&scenario, &["veh".to_string()], &maneuvers, ExecutionErrorMode::Nominal);
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(events.iter().any(|e| e.name == "in_range" && e.kind == EventKind::Maneuver as i32));
        assert!(!events.iter().any(|e| e.name == "before_start"));
    }
}
