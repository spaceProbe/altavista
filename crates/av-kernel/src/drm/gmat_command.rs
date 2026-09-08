//! GMAT-bound FRAMED consume (M25.2b, `docs/open-questions.md` questions 126/137/149;
//! `docs/sil-plan.md`'s M25 milestone: migrate the demo's drag-sail command from a native
//! SIGNAL sender to a ground-issued CCSDS telecommand).
//!
//! ## The shape (stated by the task brief, not redesigned here)
//!
//! `PacketField.target` (question 149) already declares which writable parameter a decoded
//! command field drives; `gmat_sys::model::GmatModel::step_with_ports` (M18.3, question 126)
//! already applies a SIGNAL-consumed value to exactly that parameter via `DerivativeModel::
//! set_real_parameter`, with M20.3's own "changed, or first" reporting (question 137) and a
//! `last_applied` cache scoped to the `GmatModel` instance, emptied by construction at every
//! re-materialization -- all of it inside `gmat-sys`, which cannot depend on `crate::codec` (the
//! CCSDS bit-packing implementation lives in `av-kernel`, and `av-kernel` already depends on
//! `gmat-sys`, not the other way around -- confirmed by reading both `Cargo.toml`s, the same
//! layering fact M25.2's own report found and left as "not done").
//!
//! [`GmatFramedCommandModel`] is the second entry point this task adds, entirely on this side of
//! that boundary: it wraps a `GmatModel` and, when a telecommand-in port was declared, decodes
//! the CCSDS packet at THIS layer (`crate::codec::decode_packet`, already available here),
//! extracts the one field whose `target` names a `binding::GMAT_WRITABLE_PARAMETERS` entry
//! (`super::binding::resolve_gmat_command_port`'s own job -- the "mapping layer" question 149
//! describes), and hands that value to the wrapped `GmatModel` by constructing a small synthetic
//! `Inbox` carrying a SIGNAL-encoded message (`av_dynamics::encode_signal`) on the *identical*
//! port name `GmatModel`'s own `GmatPortConfig::consume` already expects -- the exact byte
//! encoding `GmatModel::step_with_ports`'s own, unmodified, already-tested SIGNAL-consume path
//! already decodes and applies.
//!
//! **No new `gmat-sys` code, no new FFI/shim call.** `GmatModel` never learns this command
//! arrived over a CCSDS-framed port at all: from its own point of view it received an ordinary
//! SIGNAL command on `GmatPortConfig::consume`'s existing port, and the existing `last_applied`/
//! "changed, or first" cache (scoped to the wrapped `GmatModel` instance, emptied by construction
//! at every re-materialization -- `binding::materialize_gmat` builds a fresh `GmatModel`, and
//! therefore a fresh `GmatFramedCommandModel`, at every re-materialization boundary) governs
//! reporting exactly as it always has.
//!
//! ## Explicit delegation, not the trait's own defaults (question 112)
//!
//! `av_dynamics::DynamicsModel`'s own module doc comment names a recurring defect: a wrapper
//! that erases/boxes a model but forgets to delegate one method, silently falling back to the
//! *trait's* own default instead of the *wrapped model's* override (three prior occurrences in
//! this workspace). [`GmatFramedCommandModel`] therefore delegates every one of the nine trait
//! methods explicitly to `self.inner`, mirroring `super::controller::CommandedAttitude`'s own
//! same-shaped wrapper and `super::binding::AnyModel`'s own explicit, deliberate per-method
//! match arms -- never relying on a default for a method this wrapper does not itself override
//! semantically (only `step_with_ports` actually changes behaviour; every other method is a
//! straight passthrough).
//!
//! ## Ack telemetry, mirroring `ConstantAccelModel::consume_framed`'s own ack (M25.2)
//!
//! `super::executor::run_shared_group`'s own applied-commands drain derives a dispatched
//! `command` `Scenario.event`'s ACKED transition from "this applied command's mere presence [...]
//! is already proof the ack telemetry was sent" (that function's own comment) -- an assumption
//! that was true only because `ConstantAccelModel::consume_framed` always sends its own ack in
//! the same step it reports an `AppliedCommand`. Wiring a GMAT-bound target into that same
//! dispatch path (`super::binding::BindingPlan::Gmat` growing a `consume_framed_codec` arm in
//! that match) would make the assumption **false** for a GMAT target unless this wrapper does
//! the identical thing -- so [`GmatFramedCommandModel`] optionally carries an `ack_framed` send,
//! keyed off the *wrapped* `GmatModel::step_with_ports`'s own returned `Vec<AppliedCommand>`
//! (non-empty exactly when "changed, or first" just fired), using
//! `super::binding::resolve_constant_accel_ack_port` unchanged (that resolver takes only
//! `sys`/`instance`/`port_name` -- nothing `ConstantAccelSpec`-specific -- so it is reused
//! verbatim here rather than duplicated).

use std::collections::BTreeMap;

use av_cdm::pb::{ModelInfo, PacketCodec};
use av_dynamics::{encode_signal, integrate::Dopri5, AppliedCommand, DynamicsModel, Inbox, Outbox, PortMessage, StepResult, StmStepResult};
use gmat_sys::model::GmatModel;
use gmat_sys::GmatError;

use crate::codec::{self, ApidMap, FieldValue};

/// Declared-once-at-construction telecommand-in configuration -- `None` for every GMAT-bound
/// instance that declares no `"port.consume_framed"` (every fixture before M25.2b), in which
/// case [`GmatFramedCommandModel`] is a strict, byte-identical no-op: `step_with_ports` passes
/// `inbox` straight through to the wrapped `GmatModel` unmodified (proven by
/// `gmat_framed_command_none_is_a_byte_identical_no_op` below).
pub(crate) struct FramedCommandInput {
    /// The real, declared `PORT_KIND_FRAMED`/`PORT_DIRECTION_IN` port name this instance's own
    /// `SystemDefinition.ports` names -- read from the real `Inbox` the router delivered, and
    /// reused, unchanged, as the synthetic SIGNAL port name handed to the wrapped `GmatModel`'s
    /// own `GmatPortConfig::consume` (see the module doc comment) -- so `AppliedCommand.port`
    /// reports the real declared port a reader would expect, never a synthetic internal name.
    pub port: String,
    pub apid_map: ApidMap,
    /// The resolved codec field's own `name` -- `crate::codec::DecodedPacket.fields` is keyed by
    /// this, never by `target` (that struct's own doc comment).
    pub packet_field: String,
    /// The resolved codec field's own `target` -- one of `super::binding::GMAT_WRITABLE_
    /// PARAMETERS`, and exactly the `field_name` `GmatPortConfig::consume`/`GmatModel::
    /// step_with_ports` already expects.
    pub target: String,
}

/// Declared-once-at-construction ack-out configuration -- `None` unless `"port.ack_framed"` was
/// also declared (requires `FramedCommandInput` to be `Some`, checked at parse time,
/// `super::binding::parse_gmat_spec`). See the module doc comment's "Ack telemetry" section.
pub(crate) struct FramedAck {
    pub port: String,
    pub codec: PacketCodec,
}

/// See the module doc comment.
pub(crate) struct GmatFramedCommandModel {
    inner: GmatModel,
    command: Option<FramedCommandInput>,
    ack: Option<FramedAck>,
}

impl GmatFramedCommandModel {
    pub fn new(inner: GmatModel, command: Option<FramedCommandInput>, ack: Option<FramedAck>) -> Self {
        Self { inner, command, ack }
    }
}

impl DynamicsModel for GmatFramedCommandModel {
    type Error = GmatError;

    fn state_dim(&self) -> usize {
        self.inner.state_dim()
    }
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        self.inner.derivatives(state, t_tai_ns, controls, out)
    }
    fn describe(&self) -> ModelInfo {
        self.inner.describe()
    }
    fn stm_capable(&self) -> bool {
        self.inner.stm_capable()
    }
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), Self::Error> {
        self.inner.stm_derivatives(augmented_state, t_tai_ns, controls, augmented_state_dot)
    }
    fn integrator(&self) -> Dopri5 {
        self.inner.integrator()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        self.inner.step(state, t_tai_ns, controls, dt_ns)
    }
    fn step_with_stm(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StmStepResult, Self::Error> {
        self.inner.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns)
    }

    /// See the module doc comment. `self.command == None`: passes `inbox` straight through,
    /// unmodified, to `self.inner.step_with_ports` -- byte-identical to calling the wrapped
    /// `GmatModel` directly.
    ///
    /// `self.command == Some(cmd)`: decodes the *last* message on `cmd.port` in the REAL
    /// `inbox` (question 108's own delivery-order guarantee -- `Inbox::last_on_port` is the one
    /// place that selection lives), mirroring `ConstantAccelModel::step_with_ports`'s own
    /// `consume_framed` shape exactly: a message that fails to decode, or no message on this
    /// port at all this step, is the same "no message, no effect" contract every FRAMED
    /// consumer in this crate already follows -- never a hard error here, consistent with
    /// `ConstantAccelModel`'s own identical `if let Ok(decoded) = ...` shape (M25.2). A decoded
    /// value is handed to the wrapped `GmatModel` as a **freshly built, minimal `Inbox`**
    /// containing exactly one synthetic SIGNAL message on `cmd.port` -- never the real `inbox`
    /// forwarded verbatim, so the wrapped `GmatModel`'s own `decode_signal` (which requires an
    /// exact 8-byte payload) can never be handed the real CCSDS bytes by coincidence.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let Some(cmd) = &self.command else {
            return self.inner.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox);
        };
        let mut inner_inbox = Inbox::empty();
        let mut decoded_seq: Option<u16> = None;
        if let Some((msg, _sender)) = inbox.last_on_port(&cmd.port) {
            if let Ok(decoded) = codec::decode_packet(&cmd.apid_map, &msg.payload) {
                if let Some(FieldValue::Numeric(value)) = decoded.fields.get(&cmd.packet_field) {
                    // Hand the decoded value to the SAME apply path a SIGNAL command already
                    // takes -- see the module doc comment.
                    inner_inbox = Inbox::new(vec![PortMessage { port: cmd.port.clone(), tai_ns: msg.tai_ns, payload: encode_signal(*value) }]);
                    decoded_seq = Some(decoded.sequence_count);
                }
            }
        }
        let (result, mut outbox, applied) = self.inner.step_with_ports(state, t_tai_ns, controls, dt_ns, &inner_inbox)?;
        // Ack only in the same step the wrapped GmatModel's own "changed, or first" cache
        // (question 137) actually reported a new AppliedCommand -- see the module doc comment's
        // "Ack telemetry" section for why this must track the inner model's own reporting
        // decision exactly, not merely "a message decoded this step" (which could be a message
        // whose value repeats the last-applied one, correctly suppressed by `GmatModel` itself).
        if !applied.is_empty() {
            if let (Some(ack), Some(seq)) = (&self.ack, decoded_seq) {
                let mut values = BTreeMap::new();
                values.insert("cmd_seq".to_string(), FieldValue::Numeric(seq as f64));
                let payload = codec::encode_packet(&ack.codec, seq, &[], &values).expect(
                    "ack_framed_codec was resolved by binding::resolve_constant_accel_ack_port, which already requires a \"cmd_seq\" field wide enough for a Numeric value -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
                );
                outbox.push(ack.port.clone(), result.t_tai_ns, payload);
            }
        }
        Ok((result, outbox, applied))
    }

    /// Delegates to `self.inner.last_measurements` -- this wrapper decorates command/ack
    /// handling only; `GmatModel` never produces a CDM measurement today (see its own
    /// `last_measurements` doc comment), but this must not silently diverge from whatever the
    /// wrapped model reports if that ever changes.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        self.inner.last_measurements()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use av_cdm::pb::{PacketField, PacketFieldType, Unit};
    use gmat_sys::model::{GmatModelInfo, GmatPortConfig};
    use gmat_sys::Gmat;

    use super::*;
    use crate::codec::encode_packet;
    use crate::drm::command::command_ack_packet_codec;

    const CD_BEFORE: f64 = 2.2;
    const CD_AFTER: f64 = 4.4;
    const CMD_PORT: &str = "cmd_in";
    const ACK_PORT: &str = "ack_out";

    fn command_in_codec() -> PacketCodec {
        let f = PacketField { name: "value".to_string(), bit_offset: 0, bit_width: 64, r#type: PacketFieldType::Float64 as i32, unit: Unit::Dimensionless as i32, scale: 1.0, offset: 0.0, target: "Cd".to_string() };
        PacketCodec { id: "gmat_framed_command_test_cmd_in".to_string(), apid: 900, is_command: true, secondary_header_bytes: 0, user_data_bytes: 8, fields: vec![f], description: "test-only Cd telecommand".to_string() }
    }

    /// A drag-inclusive `GmatModel` (`DragForce` + `JacchiaRoberts`, ~250 km-altitude LEO) so a
    /// commanded `Cd` has a real, physically measurable effect -- mirrors `crates/gmat-sys/
    /// tests/gmat_port_cd_command.rs::build_model` field-for-field (that file's own doc comment
    /// explains why: `demo_two_instance.system.yaml`'s own force model has no drag, so a bare
    /// `Cd` change there is physically inert and would not prove anything).
    fn build_model(gmat: &Gmat, tag: &str, ports: GmatPortConfig) -> GmatModel {
        let sat = gmat.construct("Spacecraft", &format!("{tag}Sat")).unwrap();
        sat.set_str("DateFormat", "UTCGregorian").unwrap();
        sat.set_str("Epoch", "01 Jan 2026 00:00:00.000").unwrap();
        sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
        sat.set_str("DisplayStateType", "Keplerian").unwrap();
        sat.set_real("SMA", 6628.0).unwrap();
        sat.set_real("ECC", 0.001).unwrap();
        sat.set_real("INC", 51.6).unwrap();
        sat.set_real("RAAN", 30.0).unwrap();
        sat.set_real("AOP", 0.0).unwrap();
        sat.set_real("TA", 0.0).unwrap();
        sat.set_real("DryMass", 500.0).unwrap();
        sat.set_real("Cd", CD_BEFORE).unwrap();
        sat.set_real("Cr", 1.8).unwrap();
        sat.set_real("DragArea", 5.0).unwrap();
        sat.set_real("SRPArea", 5.0).unwrap();

        let fm = gmat.construct("ForceModel", &format!("{tag}FM")).unwrap();
        fm.set_str("CentralBody", "Earth").unwrap();
        let grav = gmat.construct("GravityField", &format!("{tag}Grav")).unwrap();
        grav.set_str("BodyName", "Earth").unwrap();
        grav.set_str("PotentialFile", "JGM2.cof").unwrap();
        grav.set_int("Degree", 8).unwrap();
        grav.set_int("Order", 8).unwrap();
        fm.add_force(&grav).unwrap();
        for (i, body) in ["Luna", "Sun"].into_iter().enumerate() {
            let pm = gmat.construct("PointMassForce", &format!("{tag}Pm{i}")).unwrap();
            pm.set_str("BodyName", body).unwrap();
            fm.add_force(&pm).unwrap();
        }
        let drag = gmat.construct("DragForce", &format!("{tag}Drag")).unwrap();
        drag.set_str("AtmosphereModel", "JacchiaRoberts").unwrap();
        let atmos = gmat.construct("JacchiaRoberts", &format!("{tag}Atmos")).unwrap();
        drag.set_reference(&atmos).unwrap();
        fm.add_force(&drag).unwrap();

        gmat.initialize().unwrap();
        let derivative_model = gmat.derivative_model(&fm, &sat).expect("derivative model");
        let info = GmatModelInfo { id: format!("gmat.test.{tag}"), version: "R2026a".to_string(), state_space_id: "gmat.orbital.cartesian6".to_string(), frame_id: "EarthMJ2000Eq".to_string(), goldens: vec![], has_relativistic_correction: false };
        GmatModel::new(derivative_model, info, &BTreeMap::new(), false).with_ports(ports)
    }

    fn command_packet(seq: u16, value: f64) -> av_dynamics::PortMessage {
        let mut values = BTreeMap::new();
        values.insert("value".to_string(), FieldValue::Numeric(value));
        av_dynamics::PortMessage { port: CMD_PORT.to_string(), tai_ns: 0, payload: encode_packet(&command_in_codec(), seq, &[], &values).unwrap() }
    }

    fn wrapped(gmat: &Gmat, tag: &str, command: Option<FramedCommandInput>, ack: Option<FramedAck>) -> (GmatFramedCommandModel, [f64; 6]) {
        let raw = build_model(gmat, tag, GmatPortConfig { emit: None, consume: command.as_ref().map(|c| (c.port.clone(), c.target.clone())) });
        let x0 = raw.initial_state_si().expect("initial state");
        (GmatFramedCommandModel::new(raw, command, ack), x0)
    }

    fn command_input() -> FramedCommandInput {
        let mut apid_map = ApidMap::new();
        apid_map.insert(command_in_codec().apid, command_in_codec());
        FramedCommandInput { port: CMD_PORT.to_string(), apid_map, packet_field: "value".to_string(), target: "Cd".to_string() }
    }
    fn ack_output() -> FramedAck {
        FramedAck { port: ACK_PORT.to_string(), codec: command_ack_packet_codec("gmat_framed_command_test_ack_out", 901) }
    }

    /// **The headline behavioural test for the GMAT-bound side of Job 1.** A decoded `command`
    /// packet is (a) applied within the SAME step -- `result.outputs[OUTPUT_CD]` (GMAT's own
    /// real-parameter readback, not Rust arithmetic) already reflects the commanded value; (b)
    /// reported as exactly one `AppliedCommand`, `field == "Cd"` (the codec's own declared
    /// `target`, not the packet field's own name `"value"`); (c) triggers exactly one ack packet
    /// carrying the command's own CCSDS `sequence_count`. Fails against an implementation that
    /// (i) never decodes/translates the FRAMED message at all (Cd stays at the baseline 2.2, no
    /// `AppliedCommand`, no ack -- the pre-Job-1 state for a GMAT-bound instance); (ii) forgets
    /// to wire `GmatPortConfig::consume` to the translated port/target (the wrapped `GmatModel`
    /// would never look at `inbox` at all, identical symptom to (i)); or (iii) never sends the
    /// ack.
    #[test]
    fn gmat_framed_command_applies_within_the_same_step_reports_it_and_sends_an_ack() {
        let _engine = gmat_sys::engine_lock();
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let (model, x0) = wrapped(&gmat, "GfcHeadline", Some(command_input()), Some(ack_output()));

        let inbox = Inbox::new(vec![command_packet(11, CD_AFTER)]);
        let (result, outbox, applied) = model.step_with_ports(&x0, 0, &[], 60_000_000_000, &inbox).expect("step_with_ports");

        let cd_after_step = *result.outputs.get(gmat_sys::model::OUTPUT_CD).expect("GmatModel::step always populates OUTPUT_CD");
        assert!((cd_after_step - CD_AFTER).abs() < 1e-12, "the SAME step's own GMAT real-parameter readback must already reflect the commanded Cd; got {cd_after_step}");

        assert_eq!(applied.len(), 1, "a changed value must be reported as exactly one AppliedCommand");
        assert_eq!(applied[0].field, "Cd", "AppliedCommand.field must be the codec's own declared target, not the packet field's own name \"value\"");
        assert_eq!(applied[0].value, CD_AFTER);
        assert_eq!(applied[0].port, CMD_PORT, "AppliedCommand.port must be the real declared FRAMED port name, not a synthetic internal one");

        assert_eq!(outbox.messages().len(), 1, "exactly one ack packet");
        assert_eq!(outbox.messages()[0].port, ACK_PORT);
        let mut ack_map = ApidMap::new();
        ack_map.insert(ack_output().codec.apid, ack_output().codec);
        let decoded_ack = codec::decode_packet(&ack_map, &outbox.messages()[0].payload).expect("ack packet must decode against its own declared codec");
        assert_eq!(decoded_ack.fields.get("cmd_seq"), Some(&FieldValue::Numeric(11.0)), "the ack must echo the command packet's own CCSDS sequence_count");
    }

    /// M20.3-style (question 137) "changed, or first": re-sending the IDENTICAL value a second
    /// step must not report a second `AppliedCommand` or send a second ack -- fails against an
    /// implementation that reports/acks unconditionally on every decoded message.
    #[test]
    fn gmat_framed_command_does_not_reapply_report_or_ack_an_unchanged_value() {
        let _engine = gmat_sys::engine_lock();
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let (model, x0) = wrapped(&gmat, "GfcChangeOnly", Some(command_input()), Some(ack_output()));

        let inbox1 = Inbox::new(vec![command_packet(1, CD_AFTER)]);
        let (r1, o1, applied1) = model.step_with_ports(&x0, 0, &[], 60_000_000_000, &inbox1).unwrap();
        assert_eq!(applied1.len(), 1);
        assert_eq!(o1.messages().len(), 1);

        // A different sequence_count (a real stream would never repeat one), same engineering
        // value: must not re-report or re-ack.
        let inbox2 = Inbox::new(vec![command_packet(2, CD_AFTER)]);
        let (_r2, o2, applied2) = model.step_with_ports(&r1.state, r1.t_tai_ns, &[], 60_000_000_000, &inbox2).unwrap();
        assert!(applied2.is_empty(), "an unchanged value must not be reported a second time");
        assert!(o2.messages().is_empty(), "an unchanged value must not be acked a second time");
    }

    /// **Job 1's own "no new gmat-sys call, additive, off by default" bar, made direct.** With
    /// `command: None` (every GMAT-bound fixture before M25.2b), a message on the same port name
    /// a would-be command port would use is never looked at: the propagated state and `Cd`
    /// readback are byte-identical to a run with no message at all, and no ack is ever sent even
    /// with `ack` set (degenerately, since `command` gates the whole translation). Fails against
    /// an implementation that reaches for `self.command` unconditionally instead of behind its
    /// own `Option`.
    #[test]
    fn gmat_framed_command_none_is_a_byte_identical_no_op_even_with_a_stray_message_on_the_same_port_name() {
        let _engine = gmat_sys::engine_lock();
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let (with_stray_model, x0) = wrapped(&gmat, "GfcNoneA", None, Some(ack_output()));
        let stray = Inbox::new(vec![command_packet(9, 999.0)]);
        let (with_stray, outbox, applied) = with_stray_model.step_with_ports(&x0, 0, &[], 60_000_000_000, &stray).unwrap();

        let (without_message_model, x0b) = wrapped(&gmat, "GfcNoneB", None, None);
        assert_eq!(x0, x0b, "two identically-configured spacecraft must share the identical initial state");
        let (without_message, _, _) = without_message_model.step_with_ports(&x0b, 0, &[], 60_000_000_000, &Inbox::empty()).unwrap();

        assert_eq!(with_stray.state, without_message.state, "a message on the same port name, with command unset, must not perturb the propagated state at all");
        let cd_with_stray = *with_stray.outputs.get(gmat_sys::model::OUTPUT_CD).unwrap();
        assert!((cd_with_stray - CD_BEFORE).abs() < 1e-12, "Cd must stay at the declared baseline; got {cd_with_stray}");
        assert!(applied.is_empty());
        assert!(outbox.messages().is_empty(), "ack is set but command is None: nothing was ever applied, so nothing is ever acked");
    }
}
