//! Port-layer types for the router (`docs/open-questions.md` question 108).
//!
//! [`PortMessage`], [`Inbox`] and [`Outbox`] are re-exported from [`av_dynamics`], not
//! redefined here: `av_dynamics::DynamicsModel::step_with_ports` -- the trait method a model
//! overrides to participate in port routing -- needs them in its own signature, and
//! `av_dynamics` sits below `av-kernel` in this workspace's dependency graph (`av-kernel`
//! already depends on `av-dynamics`; the reverse would be a circular dependency). This module
//! is where the *routing* concepts that build on those base types live:
//!
//! - [`QueuedMessage`] -- one message waiting for delivery to a receiving instance, carrying
//!   the extra fields [`crate::router::Router`] needs beyond the wire [`PortMessage`] itself.
//! - [`sorted_inbox`] -- the deterministic delivery order itself (question 108's "receiving
//!   instance, then port name, then sender emission epoch, then sender instance id"). The
//!   "receiving instance" part of that rule is enforced structurally by [`crate::router::
//!   Router`] keeping one queue per receiver, so what this function sorts is exactly the
//!   remaining three fields of one such queue.
//! - [`encode_ccsds_message`]/[`decode_ccsds_message`] -- question 149 (M22.3): a FRAMED port
//!   whose `Port.schema` is `"ccsds.spp"` carries one CCSDS space packet per `PortMessage`.
//!   The bit-level codec lives in [`crate::codec`] (shared with `crate::drm::schema`'s
//!   load-time validation of a declared `PacketCodec`); these two functions are the thin seam
//!   that ties it to this module's own `PortMessage`.

use std::collections::BTreeMap;

use av_cdm::pb;

pub use av_dynamics::{decode_signal, encode_signal, Inbox, Outbox, PortMessage};

/// One message queued for delivery to a receiving instance: the wire [`PortMessage`] (already
/// timestamped with the epoch at which it becomes available -- the sender's emission epoch
/// plus the connection's own latency, per `lockstep.proto`'s own `PortMessage` doc comment)
/// plus the two extra fields the delivery order (question 108) needs beyond the message's own
/// `port`: the sender's *raw* emission epoch (before latency is added) and its instance id.
/// [`crate::router::Router`] is the only constructor.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedMessage {
    pub message: PortMessage,
    pub sender_emission_tai_ns: i64,
    pub sender_instance: String,
}

/// One command a `DynamicsModel::step_with_ports` call actually applied, enriched with the
/// receiving instance and (when known) the sending instance -- the two fields
/// `av_dynamics::AppliedCommand` itself cannot carry, since a model sees only its own `Inbox`,
/// never its own instance name or another instance's identity (`docs/open-questions.md`
/// question 130). [`crate::schedule::HeteroScheduler::advance_to_with_ports`] is the one place
/// that builds these: right after a `step_with_ports` call returns, it already has both the
/// receiving instance's own id (the `BTreeMap` key it is currently iterating) and the `Inbox`
/// that call was handed in scope, and resolves `sender` from that same `Inbox` via
/// [`av_dynamics::Inbox::last_on_port`] -- the identical "last message on this port" selection
/// the model itself just used to decide what to apply, so the sender attributed here can never
/// disagree with which message was actually consumed.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedPortCommand {
    pub instance: String,
    pub port: String,
    pub field: String,
    pub value: f64,
    pub applied_tai_ns: i64,
    pub sender: Option<String>,
}

/// One undecodable FRAMED frame a `DynamicsModel::step_with_ports` call received and skipped,
/// enriched with the receiving instance -- the one field `av_dynamics::DecodeErrorOccurrence`
/// itself cannot carry, since a model sees only its own `Inbox`, never its own instance name
/// (`docs/open-questions.md` question 188, R5.2, mirrors [`AppliedPortCommand`]'s own identical
/// "the caller knows the instance, the model does not" enrichment). [`crate::schedule::
/// HeteroScheduler::advance_to_with_ports`] is the one place that builds these, right after a
/// `step_with_ports` call returns, from `model.drain_decode_errors()`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeErrorRecord {
    pub instance: String,
    pub port: String,
    pub tai_ns: i64,
    pub sequence_count: Option<u16>,
    pub error: String,
}

/// One successful decode on `(instance, port)` -- the signal `docs/open-questions.md` question
/// 193 (R6.2) needs to close a decode-error episode ("closes when that consumer next decodes a
/// frame on that port successfully"), enriched with the receiving instance the same way
/// [`DecodeErrorRecord`] enriches `av_dynamics::DecodeErrorOccurrence`.
///
/// **Why this is derived in `crate::schedule::HeteroScheduler::advance_to_with_ports` rather than
/// reported by the model itself (no new `av_dynamics::DynamicsModel` trait method).** A model's
/// real decode-success signal would need to cross the `av_dynamics::erase::ErasedModel` type-
/// erasure boundary (every model this crate constructs is wrapped there before it ever reaches a
/// `BoxedModel`, `crate::registry::ModelHandle::into_boxed`'s own doc comment) exactly the way
/// [`DecodeErrorRecord`]'s own `av_dynamics::DecodeErrorOccurrence` already does -- but
/// `av-dynamics/src/erase.rs` is outside this round's file allowlist (`crates/av-dynamics/src/
/// lib.rs` only), so a new *required* trait method cannot be wired through it without touching a
/// file this round does not own, and a new *defaulted* method would still need an explicit
/// override in `ErasedModel`'s own `impl DynamicsModel` block to ever see past the default (an
/// `impl` that does not mention a defaulted method inherits the trait's own default, never the
/// wrapped inner model's override -- Rust has no automatic forwarding). See `R6_2_REPORT.md`
/// ("What was considered") for the alternatives weighed and why this one was chosen instead:
/// [`crate::schedule::HeteroScheduler::advance_to_with_ports`] already has, at the exact point it
/// drains `model.drain_decode_errors()`, both the `Inbox` that call was handed AND that call's own
/// freshly-drained failure occurrences -- a port present in the `Inbox` (the same `Inbox::
/// last_on_port` selection every real FRAMED consumer in this workspace uses to pick which
/// message to attempt) whose port name does NOT appear among this call's own failure occurrences
/// is, by construction, a successful decode (every real consumer either updates its cache or
/// records exactly one failure per port per call -- the two are mutually exclusive and
/// exhaustive), so its absence from the failures is a sound, derived success signal, never a
/// guess. A port this instance never actually decodes (a SIGNAL port, or a declared FRAMED port no
/// model logic reads) also passes this test, but harmlessly: such a port can never have
/// accumulated a [`DecodeErrorRecord`] either, so it can never have an open episode for this
/// signal to close.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeSuccessRecord {
    pub instance: String,
    pub port: String,
    pub tai_ns: i64,
}

/// Sort `queued` (every message currently waiting for one receiving instance) into question
/// 108's deterministic delivery order -- `(port name, sender emission epoch, sender instance
/// id)`, the receiving-instance field of the full four-field rule already fixed by which queue
/// this is -- and return it as the [`Inbox`] the receiver's next `step_with_ports` call sees.
/// A stable sort: two [`QueuedMessage`]s that tie on every key field (same port, same sender,
/// same epoch -- only possible if a model queued two messages on the same port in one step)
/// keep their emission order rather than being reordered by this call.
///
/// **Question 130: the sender survives into the returned `Inbox`.** Every `QueuedMessage` this
/// function ever receives already carries `sender_instance` (`Router::deliver`'s own doc
/// comment); through M14.2 this was discarded here (`Inbox::new` only ever took the bare
/// `PortMessage`s), which is exactly why `av_dynamics::Inbox` grew a real sender slot
/// ([`av_dynamics::Inbox::new_with_senders`]) for this task -- carrying it through here, rather
/// than fabricating it later, is what lets `crate::schedule::HeteroScheduler::
/// advance_to_with_ports` attribute a real sender to a `port_command` CDM event.
pub fn sorted_inbox(mut queued: Vec<QueuedMessage>) -> Inbox {
    queued.sort_by(|a, b| {
        (a.message.port.as_str(), a.sender_emission_tai_ns, a.sender_instance.as_str()).cmp(&(b.message.port.as_str(), b.sender_emission_tai_ns, b.sender_instance.as_str()))
    });
    let (messages, senders) = queued.into_iter().map(|q| (q.message, Some(q.sender_instance))).unzip();
    Inbox::new_with_senders(messages, senders)
}

// --------------------------------------------------------------------------------------
// CCSDS space-packet framing on a FRAMED port (`docs/open-questions.md` question 149,
// `docs/sil-plan.md`'s M22.3): a `Port.schema` of `"ccsds.spp"` means the `PortMessage`
// payload *is* one CCSDS space packet's bytes, encoded/decoded by the declared `PacketCodec`.
// The bit-level codec itself lives in `crate::codec` (shared with `crate::drm::schema`'s
// load-time validation, so there is exactly one implementation of the wire format); these two
// functions are the thin seam that ties it to this module's own `PortMessage` -- the only
// thing a `"ccsds.spp"` port needs beyond a bare byte codec is `PortMessage`'s `port`/`tai_ns`
// bookkeeping, which `crate::codec` deliberately knows nothing about (it is GMAT/port-router
// free, like every other module `lib.rs`'s own doc comment lists alongside it).
// --------------------------------------------------------------------------------------

/// Build the `PortMessage` a FRAMED `"ccsds.spp"` port sends: the payload is exactly one
/// encoded CCSDS space packet ([`crate::codec::encode_packet`]), and `tai_ns` is the sender's
/// own emission epoch -- the same field every other port message already carries (question
/// 108). A [`crate::codec::CodecError`] (a missing field value, a value that does not fit its
/// declared bit width, or a field extent past `user_data_bytes`) is returned rather than ever
/// emitting a malformed packet.
pub fn encode_ccsds_message(
    port: &str,
    tai_ns: i64,
    codec: &pb::PacketCodec,
    sequence_count: u16,
    secondary_header: &[u8],
    values: &BTreeMap<String, crate::codec::FieldValue>,
) -> Result<PortMessage, crate::codec::CodecError> {
    let payload = crate::codec::encode_packet(codec, sequence_count, secondary_header, values)?;
    Ok(PortMessage { port: port.to_string(), tai_ns, payload })
}

/// Decode a FRAMED `"ccsds.spp"` port message's payload as one CCSDS space packet against the
/// declared [`crate::codec::ApidMap`]. Question 149: an unknown APID or a malformed packet is
/// the typed [`crate::codec::CodecError`] [`crate::codec::decode_packet`] itself returns,
/// never a silent drop -- this wrapper adds no fallback of its own.
pub fn decode_ccsds_message(apid_map: &crate::codec::ApidMap, message: &PortMessage) -> Result<crate::codec::DecodedPacket, crate::codec::CodecError> {
    crate::codec::decode_packet(apid_map, &message.payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `label` is baked into the payload (as its own UTF-8 bytes) purely so this test can
    /// identify which `QueuedMessage` survived the sort by reading the delivered `Inbox` alone
    /// -- `PortMessage` itself carries no sender field once queued.
    fn queued(port: &str, sender_epoch: i64, sender: &str, delivered_tai_ns: i64, label: &str) -> QueuedMessage {
        QueuedMessage {
            message: PortMessage { port: port.to_string(), tai_ns: delivered_tai_ns, payload: label.as_bytes().to_vec() },
            sender_emission_tai_ns: sender_epoch,
            sender_instance: sender.to_string(),
        }
    }

    #[test]
    fn sorted_inbox_orders_by_port_then_sender_epoch_then_sender_instance() {
        // Two entries deliberately tie on (port, sender_epoch) so only the fourth key (sender
        // instance id) can break the tie -- "sat_a" < "sat_b".
        let input = vec![
            queued("telemetry", 200, "sat_b", 999, "late_telemetry"),
            queued("command", 100, "gnc", 999, "the_command"),
            queued("telemetry", 100, "sat_b", 999, "tele_from_b"),
            queued("telemetry", 100, "sat_a", 999, "tele_from_a"),
        ];
        let inbox = sorted_inbox(input);
        let got: Vec<String> = inbox.messages().iter().map(|m| String::from_utf8(m.payload.clone()).unwrap()).collect();
        // "command" sorts before "telemetry" (port name first); within "telemetry", epoch 100
        // sorts before 200, and at epoch 100 sender "sat_a" sorts before "sat_b".
        assert_eq!(got, vec!["the_command", "tele_from_a", "tele_from_b", "late_telemetry"]);
    }

    #[test]
    fn sorted_inbox_of_empty_input_is_an_empty_inbox() {
        assert!(sorted_inbox(vec![]).is_empty());
    }

    // -----------------------------------------------------------------------------------
    // CCSDS space-packet framing on a FRAMED port (question 149, M22.3).
    // -----------------------------------------------------------------------------------

    fn tm_codec() -> pb::PacketCodec {
        pb::PacketCodec {
            id: "tm_test".to_string(),
            apid: 10,
            is_command: false,
            secondary_header_bytes: 0,
            user_data_bytes: 4,
            description: String::new(),
            fields: vec![pb::PacketField {
                name: "word".to_string(),
                bit_offset: 0,
                bit_width: 32,
                r#type: pb::PacketFieldType::Uint as i32,
                unit: pb::Unit::Unspecified as i32,
                scale: 1.0,
                offset: 0.0,
                target: String::new(),
            }],
        }
    }

    /// A FRAMED `"ccsds.spp"` port's `PortMessage.payload` round-trips through
    /// `encode_ccsds_message`/`decode_ccsds_message` -- fails against a wrapper that drops
    /// `port`/`tai_ns` on encode, or that hands `decode_ccsds_message` the wrong bytes.
    #[test]
    fn encode_and_decode_ccsds_message_round_trip_through_a_port_message() {
        let codec = tm_codec();
        let mut values = BTreeMap::new();
        values.insert("word".to_string(), crate::codec::FieldValue::Numeric(123456.0));
        let message = encode_ccsds_message("telemetry_out", 5_000_000_000, &codec, 1, &[], &values).expect("encodes");
        assert_eq!(message.port, "telemetry_out");
        assert_eq!(message.tai_ns, 5_000_000_000);
        assert_eq!(message.payload.len(), 10); // 6-byte primary header + 4 user-data bytes

        let mut apid_map = crate::codec::ApidMap::new();
        apid_map.insert(10, codec);
        let decoded = decode_ccsds_message(&apid_map, &message).expect("decodes");
        assert_eq!(decoded.apid, 10);
        assert_eq!(decoded.fields.get("word"), Some(&crate::codec::FieldValue::Numeric(123456.0)));
    }

    /// An unknown APID surfaces as the typed `CodecError`, not a silent drop, even through
    /// this module's own thin wrapper -- question 149's rule applied at the port-message seam.
    #[test]
    fn decode_ccsds_message_of_an_unknown_apid_is_a_typed_error() {
        let codec = tm_codec();
        let mut values = BTreeMap::new();
        values.insert("word".to_string(), crate::codec::FieldValue::Numeric(1.0));
        let message = encode_ccsds_message("telemetry_out", 0, &codec, 0, &[], &values).expect("encodes");
        let empty_map = crate::codec::ApidMap::new();
        let err = decode_ccsds_message(&empty_map, &message).unwrap_err();
        assert!(matches!(err, crate::codec::CodecError::UnknownApid { apid: 10 }), "{err:?}");
    }
}
