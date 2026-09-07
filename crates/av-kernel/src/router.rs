//! `Router`: `SosConfiguration.connections` (`proto/altavista/v1/system.proto`) realized as a
//! routing table, plus the run-time queue of messages waiting for delivery
//! (`docs/open-questions.md` question 108, the lead's phase-one decision).
//!
//! [`Router::build`] validates every connection, once, against the two ports it names --
//! existence, direction, kind, and its own `link_model` -- before it ever returns, so a bad
//! connection is a typed load error ([`RouterError`]), never discovered only once two
//! instances actually try to exchange a message. `crate::drm::executor::execute` calls this at
//! load, exactly like its other pre-propagation checks (hash verification, expression
//! validation) -- see that module's own doc comment.
//!
//! ## Delivery model (`docs/open-questions.md` question 110, M14.2)
//!
//! A message [`crate::schedule::HeteroScheduler::advance_to_with_ports`] collects from one
//! instance's `Outbox` is, for every declared `Connection` whose `from_instance`/`from_port`
//! match, copied onto the named `to_instance`'s pending queue, timestamped with the sender's
//! own emission epoch plus that connection's latency (see "Link model" below) -- "the epoch at
//! which the message is available to the receiver" (`lockstep.proto`'s own `PortMessage` doc
//! comment). **Question 110's decision (ADR-005 sec 4):** that timestamp -- call it the
//! message's *availability* -- actually gates delivery now: [`Router::take_inbox`] takes the
//! calling receiver step's own epoch (`as_of_tai_ns`, always that step's *result* epoch --
//! [`crate::schedule::HeteroScheduler::advance_to_with_ports`]'s own `t`, the epoch the
//! receiver's state will have advanced to once this step completes) and returns only the
//! messages whose `tai_ns <= as_of_tai_ns` -- "the first receiver step whose epoch is >=
//! availability, never earlier." A message not yet available stays in the pending queue,
//! untouched, and is re-considered at every later `take_inbox` call for that receiver until one
//! finally clears it -- held, never dropped, by [`Router`] itself. (Question 108's own
//! within-a-tied-instant ordering is unchanged and layers on top of this: two instances due at
//! the same native epoch still step in sorted-instance-name order, so a message a same-epoch
//! sender emits is queued only after any receiver whose name sorts earlier has already taken
//! *its* inbox for that epoch -- that instance sees it at its own next step instead, exactly as
//! before question 110.) A message on a port with no matching connection is silently dropped
//! (not wired anywhere) -- a run-time condition, not the load-time refusal [`Router::build`]
//! already applies to every *declared* connection.
//!
//! **A message still pending when the run ends is lost, not delivered late.** Nothing calls
//! [`Router::take_inbox`] again once the last `advance_to_with_ports`/`run_with_ports` call
//! returns (`crate::drm::executor::run_shared_group` threads one `Router` across every span of
//! a run, but the run itself still has a last span), so a message whose availability epoch
//! never coincides with a receiver step actually taken before the run's own `end_tai_ns` simply
//! stays in `pending` and is dropped along with the `Router` itself. [`Router::has_pending`]
//! after a run tells a caller whether this happened; [`Router::pending_count`] (M14.4) counts
//! exactly how many. **This is no longer a silent drop at the `crate::drm::executor::execute`
//! level**: `execute` reads `pending_count` once `run_shared_group` returns, records it
//! unconditionally in `RunProducts.provenance.attributes["dropped_in_flight_messages"]`, and, when
//! it is non-zero, emits one `EVENT_KIND_LIFECYCLE` event naming it (`events::
//! dropped_messages_event`) -- see that function's own doc comment.
//!
//! ## Link model (question 108: "latency only in phase one")
//!
//! `Connection.link_model` is a free-form "dynamics model id" for a future, richer link (loss,
//! bandwidth, ...). This phase implements exactly one named model, plus the unset case:
//!
//! - `""` (unset) -- no link model requested; messages are delivered with **zero** added
//!   latency (`tai_ns` = the sender's own emission epoch).
//! - `"latency"` -- the phase-one link model; the added latency is the **sum of the two named
//!   ports' own declared `PortTiming.latency_ns`** (`from_port`'s and `to_port`'s -- `Port`'s
//!   own doc comment: "Rates and latencies are part of the system, not of where it runs", so
//!   this is the router applying the numbers a system already declares about its own ports,
//!   not inventing a second place to put them). A port with no declared `timing` contributes
//!   zero.
//! - anything else -- [`RouterError::UnsupportedLinkModel`], a typed refusal (question 108: "a
//!   link_model naming anything else is a typed refusal, not a silent ignore").

use std::collections::BTreeMap;

use av_cdm::pb::{Connection, Port, PortDirection, PortKind, SosConfiguration, SystemDefinition};
use av_dynamics::Outbox;

use crate::ports::{sorted_inbox, Inbox, QueuedMessage};

/// The one named link model phase one implements (see the module doc comment's "Link model"
/// section). Any other non-empty `Connection.link_model` is [`RouterError::UnsupportedLinkModel`].
pub const LATENCY_LINK_MODEL: &str = "latency";

/// Everything [`Router::build`] can refuse a `SosConfiguration` for. `connection_index` is the
/// zero-based position in `SosConfiguration.connections`, since a `Connection` carries no id of
/// its own to name in an error.
#[derive(Debug, Clone, PartialEq)]
pub enum RouterError {
    /// `Connection.from_instance`/`to_instance` named no `SosConfiguration.instances` entry.
    UnknownInstance { connection_index: usize, instance: String },
    /// A named instance's `SystemInstance.system_id` named no `SystemDefinition` in the map
    /// `Router::build` was given.
    UnknownSystemDefinition { connection_index: usize, instance: String, system_id: String },
    /// `Connection.from_port`/`to_port` named no `Port.name` in that instance's own
    /// `SystemDefinition.ports` -- question 108's "connection to an undeclared port".
    UndeclaredPort { connection_index: usize, instance: String, port: String },
    /// The named port's declared `PortDirection` cannot act as this end of the connection: the
    /// sender's `from_port` must be OUT or INOUT, the receiver's `to_port` must be IN or INOUT
    /// -- question 108's "direction mismatch".
    DirectionMismatch { connection_index: usize, instance: String, port: String, direction: String, expected: &'static str },
    /// `from_port.kind != to_port.kind` -- question 108's "kind mismatch".
    KindMismatch { connection_index: usize, from_port: String, from_kind: String, to_port: String, to_kind: String },
    /// `Connection.link_model` named something other than `""` or `"latency"` (see the module
    /// doc comment's "Link model" section) -- question 108's "a link_model naming anything else
    /// is a typed refusal".
    UnsupportedLinkModel { connection_index: usize, link_model: String },
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterError::UnknownInstance { connection_index, instance } => {
                write!(f, "connection[{connection_index}]: instance {instance:?} is not in this SosConfiguration")
            }
            RouterError::UnknownSystemDefinition { connection_index, instance, system_id } => {
                write!(f, "connection[{connection_index}]: instance {instance:?} names system_id {system_id:?}, which has no supplied SystemDefinition")
            }
            RouterError::UndeclaredPort { connection_index, instance, port } => {
                write!(f, "connection[{connection_index}]: instance {instance:?} declares no port named {port:?}")
            }
            RouterError::DirectionMismatch { connection_index, instance, port, direction, expected } => {
                write!(f, "connection[{connection_index}]: instance {instance:?} port {port:?} has direction {direction} but must be {expected} to be used this way")
            }
            RouterError::KindMismatch { connection_index, from_port, from_kind, to_port, to_kind } => {
                write!(f, "connection[{connection_index}]: from_port {from_port:?} (kind {from_kind}) and to_port {to_port:?} (kind {to_kind}) do not agree")
            }
            RouterError::UnsupportedLinkModel { connection_index, link_model } => {
                write!(f, "connection[{connection_index}]: link_model {link_model:?} is not supported yet (phase one is latency only: \"\" or {LATENCY_LINK_MODEL:?})")
            }
        }
    }
}
impl std::error::Error for RouterError {}

#[derive(Debug, Clone)]
struct Edge {
    to_instance: String,
    to_port: String,
    latency_ns: i64,
}

/// `SosConfiguration.connections` realized as a routing table, plus the pending-delivery
/// queues (question 108). See the module doc comment for the delivery model and the link
/// model this phase implements.
#[derive(Debug, Clone, Default)]
pub struct Router {
    /// (from_instance, from_port) -> every connected receiver.
    edges: BTreeMap<(String, String), Vec<Edge>>,
    /// to_instance -> messages queued, not yet drained by that instance's own next step.
    pending: BTreeMap<String, Vec<QueuedMessage>>,
}

fn direction_name(raw: i32) -> String {
    PortDirection::try_from(raw).map(|d| d.as_str_name().to_string()).unwrap_or_else(|_| format!("<unknown PortDirection {raw}>"))
}
fn kind_name(raw: i32) -> String {
    PortKind::try_from(raw).map(|k| k.as_str_name().to_string()).unwrap_or_else(|_| format!("<unknown PortKind {raw}>"))
}
fn port_latency_ns(port: &Port) -> i64 {
    port.timing.as_ref().map(|t| t.latency_ns).unwrap_or(0)
}

fn lookup_port<'a>(
    connection_index: usize,
    instance: &str,
    port_name: &str,
    instance_systems: &BTreeMap<&str, &str>,
    systems: &'a BTreeMap<String, SystemDefinition>,
) -> Result<&'a Port, RouterError> {
    let system_id = instance_systems.get(instance).ok_or_else(|| RouterError::UnknownInstance { connection_index, instance: instance.to_string() })?;
    let sys = systems
        .get(*system_id)
        .ok_or_else(|| RouterError::UnknownSystemDefinition { connection_index, instance: instance.to_string(), system_id: system_id.to_string() })?;
    sys.ports.iter().find(|p| p.name == port_name).ok_or_else(|| RouterError::UndeclaredPort { connection_index, instance: instance.to_string(), port: port_name.to_string() })
}

fn effective_latency_ns(connection_index: usize, conn: &Connection, from_port: &Port, to_port: &Port) -> Result<i64, RouterError> {
    match conn.link_model.as_str() {
        "" => Ok(0),
        LATENCY_LINK_MODEL => Ok(port_latency_ns(from_port) + port_latency_ns(to_port)),
        other => Err(RouterError::UnsupportedLinkModel { connection_index, link_model: other.to_string() }),
    }
}

impl Router {
    /// Build a `Router` from `sos.connections`, validating every one against the declared
    /// ports of the `SystemDefinition`s named in `systems` (keyed by `SystemDefinition.id`,
    /// exactly `crate::drm::executor::RunConfig::systems`'s own shape) -- see the module doc
    /// comment. An empty `connections` list always succeeds trivially with an empty routing
    /// table, so every existing DRM that declares no ports/connections (the golden included)
    /// builds a `Router` that never queues or delivers anything.
    pub fn build(sos: &SosConfiguration, systems: &BTreeMap<String, SystemDefinition>) -> Result<Router, RouterError> {
        let instance_systems: BTreeMap<&str, &str> = sos.instances.iter().map(|i| (i.name.as_str(), i.system_id.as_str())).collect();
        let mut edges: BTreeMap<(String, String), Vec<Edge>> = BTreeMap::new();

        for (idx, conn) in sos.connections.iter().enumerate() {
            let from_port = lookup_port(idx, &conn.from_instance, &conn.from_port, &instance_systems, systems)?;
            let to_port = lookup_port(idx, &conn.to_instance, &conn.to_port, &instance_systems, systems)?;

            let from_dir = PortDirection::try_from(from_port.direction).unwrap_or(PortDirection::Unspecified);
            if !matches!(from_dir, PortDirection::Out | PortDirection::Inout) {
                return Err(RouterError::DirectionMismatch {
                    connection_index: idx,
                    instance: conn.from_instance.clone(),
                    port: conn.from_port.clone(),
                    direction: direction_name(from_port.direction),
                    expected: "OUT or INOUT",
                });
            }
            let to_dir = PortDirection::try_from(to_port.direction).unwrap_or(PortDirection::Unspecified);
            if !matches!(to_dir, PortDirection::In | PortDirection::Inout) {
                return Err(RouterError::DirectionMismatch {
                    connection_index: idx,
                    instance: conn.to_instance.clone(),
                    port: conn.to_port.clone(),
                    direction: direction_name(to_port.direction),
                    expected: "IN or INOUT",
                });
            }
            if from_port.kind != to_port.kind {
                return Err(RouterError::KindMismatch {
                    connection_index: idx,
                    from_port: conn.from_port.clone(),
                    from_kind: kind_name(from_port.kind),
                    to_port: conn.to_port.clone(),
                    to_kind: kind_name(to_port.kind),
                });
            }
            let latency_ns = effective_latency_ns(idx, conn, from_port, to_port)?;

            edges.entry((conn.from_instance.clone(), conn.from_port.clone())).or_default().push(Edge {
                to_instance: conn.to_instance.clone(),
                to_port: conn.to_port.clone(),
                latency_ns,
            });
        }

        Ok(Router { edges, pending: BTreeMap::new() })
    }

    /// An empty router with no connections -- every `Outbox` handed to [`Router::deliver`] on
    /// this router is simply dropped (no edges to follow). Useful for driving a port-aware
    /// model with no wiring, or in tests that only need one model's own `step_with_ports`
    /// behaviour.
    pub fn empty() -> Router {
        Router::default()
    }

    /// Queue every message in `outbox` (emitted by `from_instance` at `emission_tai_ns`) onto
    /// every connected receiver's pending queue, applying that connection's own latency -- see
    /// the module doc comment's "Delivery model"/"Link model" sections. A message on a port
    /// with no matching connection is dropped, not an error (see the module doc comment).
    pub fn deliver(&mut self, from_instance: &str, emission_tai_ns: i64, outbox: Outbox) {
        for message in outbox.into_messages() {
            let Some(targets) = self.edges.get(&(from_instance.to_string(), message.port.clone())) else {
                continue;
            };
            for edge in targets {
                let delivered = av_cdm::pb::PortMessage { port: edge.to_port.clone(), tai_ns: emission_tai_ns + edge.latency_ns, payload: message.payload.clone() };
                self.pending.entry(edge.to_instance.clone()).or_default().push(QueuedMessage {
                    message: delivered,
                    sender_emission_tai_ns: emission_tai_ns,
                    sender_instance: from_instance.to_string(),
                });
            }
        }
    }

    /// Drain and sort every message currently queued for `to_instance` whose availability
    /// (`PortMessage.tai_ns`) is `<= as_of_tai_ns` into question 108's deterministic delivery
    /// order ([`crate::ports::sorted_inbox`]) -- the [`Inbox`] the receiver step at that epoch
    /// sees (question 110: "the first receiver step whose epoch is >= availability"). `caller`
    /// must pass that step's own *result* epoch (`crate::schedule::HeteroScheduler::
    /// advance_to_with_ports`'s `t`), not the state epoch the step reads from, so a message
    /// emitted exactly on a step boundary with zero latency is available to (not held past)
    /// that very step -- `as_of_tai_ns` is compared with `<=`, inclusive at the boundary.
    /// Anything left over (`tai_ns > as_of_tai_ns`) is **not** removed: it stays queued for
    /// `to_instance` and is reconsidered the next time this is called for the same receiver, at
    /// a later (larger) `as_of_tai_ns` -- see the module doc comment's "still pending when the
    /// run ends" note for what happens if that never comes. An instance never named as a
    /// `to_instance` by any connection (or with nothing available right now) gets an empty
    /// `Inbox`, never an error.
    pub fn take_inbox(&mut self, to_instance: &str, as_of_tai_ns: i64) -> Inbox {
        let queued = self.pending.remove(to_instance).unwrap_or_default();
        let (ready, held): (Vec<QueuedMessage>, Vec<QueuedMessage>) = queued.into_iter().partition(|q| q.message.tai_ns <= as_of_tai_ns);
        if !held.is_empty() {
            self.pending.insert(to_instance.to_string(), held);
        }
        sorted_inbox(ready)
    }

    /// Whether anything is currently queued for any receiver, delivered or not -- true both for
    /// a message a caller simply hasn't drained yet with [`Router::take_inbox`] and for one
    /// [`Router::take_inbox`] has already seen and held back because it was not yet available.
    /// Used by tests and callers that want to assert a run drained (or, after M14.2, actually
    /// delivered) everything it queued.
    pub fn has_pending(&self) -> bool {
        self.pending.values().any(|v| !v.is_empty())
    }

    /// How many messages are currently queued for any receiver, delivered or not -- the counted
    /// form of [`Router::has_pending`] (M14.4, `docs/open-questions.md`: "in-flight messages at
    /// run end are not silently dropped"). `crate::drm::executor::execute` calls this once the
    /// run's own last span has finished (nothing calls [`Router::take_inbox`] again after that --
    /// see the module doc comment's "still pending when the run ends" note) to record exactly
    /// how many in-flight messages were lost, never just whether any were.
    pub fn pending_count(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{PortTiming, SystemInstance};

    fn port(name: &str, kind: PortKind, direction: PortDirection) -> Port {
        Port { name: name.to_string(), kind: kind as i32, direction: direction as i32, ..Default::default() }
    }
    fn port_with_latency(name: &str, kind: PortKind, direction: PortDirection, latency_ns: i64) -> Port {
        Port { timing: Some(PortTiming { latency_ns, ..Default::default() }), ..port(name, kind, direction) }
    }
    fn sys(id: &str, ports: Vec<Port>) -> SystemDefinition {
        SystemDefinition { id: id.to_string(), ports, ..Default::default() }
    }
    fn instance(name: &str, system_id: &str) -> SystemInstance {
        SystemInstance { name: name.to_string(), system_id: system_id.to_string(), ..Default::default() }
    }
    fn conn(from_i: &str, from_p: &str, to_i: &str, to_p: &str, link_model: &str) -> Connection {
        Connection { from_instance: from_i.to_string(), from_port: from_p.to_string(), to_instance: to_i.to_string(), to_port: to_p.to_string(), link_model: link_model.to_string() }
    }
    fn systems_map(defs: Vec<SystemDefinition>) -> BTreeMap<String, SystemDefinition> {
        defs.into_iter().map(|d| (d.id.clone(), d)).collect()
    }

    fn two_signal_instances(from_dir: PortDirection, to_dir: PortDirection) -> (SosConfiguration, BTreeMap<String, SystemDefinition>) {
        let sender = sys("sender_sys", vec![port("out", PortKind::Signal, from_dir)]);
        let receiver = sys("receiver_sys", vec![port("in", PortKind::Signal, to_dir)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", "")],
            ..Default::default()
        };
        (sos, systems_map(vec![sender, receiver]))
    }

    #[test]
    fn an_empty_connections_list_builds_trivially() {
        let sos = SosConfiguration { instances: vec![instance("a", "sys_a")], ..Default::default() };
        let systems = systems_map(vec![sys("sys_a", vec![])]);
        let router = Router::build(&sos, &systems).expect("no connections to validate");
        assert!(!router.has_pending());
    }

    #[test]
    fn a_valid_connection_with_no_link_model_builds_and_delivers_with_zero_latency() {
        let (sos, systems) = two_signal_instances(PortDirection::Out, PortDirection::In);
        let mut router = Router::build(&sos, &systems).expect("valid connection");

        let mut outbox = Outbox::new();
        outbox.push_signal("out", 1_000, 42.0);
        router.deliver("sender", 1_000, outbox);

        let inbox = router.take_inbox("receiver", 1_000);
        assert_eq!(inbox.messages().len(), 1);
        assert_eq!(inbox.messages()[0].port, "in");
        assert_eq!(inbox.messages()[0].tai_ns, 1_000, "no link_model requested: zero added latency");
        assert_eq!(av_dynamics::decode_signal(&inbox.messages()[0].payload), Some(42.0));
        // Draining is destructive: a second take_inbox call with nothing newly delivered is empty.
        assert!(router.take_inbox("receiver", 2_000).is_empty());
    }

    #[test]
    fn the_latency_link_model_sums_both_ports_declared_latency() {
        let sender = sys("sender_sys", vec![port_with_latency("out", PortKind::Signal, PortDirection::Out, 30_000_000)]);
        let receiver = sys("receiver_sys", vec![port_with_latency("in", PortKind::Signal, PortDirection::In, 20_000_000)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", LATENCY_LINK_MODEL)],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connection");

        let mut outbox = Outbox::new();
        outbox.push_signal("out", 1_000, 7.0);
        router.deliver("sender", 1_000, outbox);

        // Available at 1_000 + 50_000_000; a step whose own epoch has not reached that yet must
        // not see it (question 110), only one whose epoch has.
        assert!(router.take_inbox("receiver", 1_000 + 49_999_999).is_empty(), "not yet available: held, not delivered early");
        let inbox = router.take_inbox("receiver", 1_000 + 50_000_000);
        assert_eq!(inbox.messages()[0].tai_ns, 1_000 + 30_000_000 + 20_000_000, "latency link model: sum of both ports' declared latency_ns");
    }

    #[test]
    fn take_inbox_holds_a_not_yet_available_message_across_multiple_calls_then_delivers_it_at_or_after_availability() {
        // 30 ms latency on the sender's own port: available at exactly 10_000 + 30_000_000, an
        // epoch distinct from the emission epoch so "held" vs. "delivered" is unambiguous.
        let sender = sys("sender_sys", vec![port_with_latency("out", PortKind::Signal, PortDirection::Out, 30_000_000)]);
        let receiver = sys("receiver_sys", vec![port("in", PortKind::Signal, PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", LATENCY_LINK_MODEL)],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let mut outbox = Outbox::new();
        outbox.push_signal("out", 10_000, 9.0);
        router.deliver("sender", 10_000, outbox);
        let available_at = 10_000 + 30_000_000;

        // Three calls at successively later epochs, all still before availability: held every
        // time, never lost, never delivered early.
        for as_of in [10_000, 10_001, available_at - 1] {
            assert!(router.take_inbox("receiver", as_of).is_empty(), "as_of={as_of} is before availability {available_at}: must still be held");
            assert!(router.has_pending(), "a held message is still pending, even though take_inbox already ran for this receiver");
        }
        // Delivered the instant a call's epoch reaches (not merely passes) availability.
        let inbox = router.take_inbox("receiver", available_at);
        assert_eq!(inbox.messages().len(), 1, "delivered exactly when as_of reaches availability");
        assert!(!router.has_pending(), "nothing left queued once the only message clears");
    }

    /// M14.4: [`Router::pending_count`] is the exact count [`Router::has_pending`] only reports
    /// as a bool -- would catch an implementation that special-cased "any pending" without ever
    /// summing across receivers (e.g. one that always returned `0` or `1`), or one that counted
    /// `self.pending.len()` (the number of *receivers* with something queued, not the number of
    /// *messages* -- wrong the moment one receiver has more than one message held).
    #[test]
    fn pending_count_is_the_total_number_of_queued_messages_not_merely_whether_any_are_pending() {
        let sender = sys(
            "sender_sys",
            vec![port_with_latency("fast", PortKind::Signal, PortDirection::Out, 0), port_with_latency("slow", PortKind::Signal, PortDirection::Out, 40_000_000)],
        );
        let receiver = sys("receiver_sys", vec![port("fast", PortKind::Signal, PortDirection::In), port("slow", PortKind::Signal, PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "fast", "receiver", "fast", LATENCY_LINK_MODEL), conn("sender", "slow", "receiver", "slow", LATENCY_LINK_MODEL)],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connections");
        assert_eq!(router.pending_count(), 0, "nothing queued yet");

        let mut outbox = Outbox::new();
        outbox.push_signal("fast", 1_000, 1.0);
        outbox.push_signal("slow", 1_000, 2.0);
        router.deliver("sender", 1_000, outbox);
        assert_eq!(router.pending_count(), 2, "two messages queued for one receiver");

        // Draining only the already-available "fast" message (zero latency) must drop the count
        // by exactly one, not to zero and not left at two -- proving this is a real per-message
        // count, not a receiver-level flag.
        let inbox = router.take_inbox("receiver", 1_000);
        assert_eq!(inbox.messages().len(), 1);
        assert_eq!(router.pending_count(), 1, "the 40 ms-latency \"slow\" message is still held");
        assert!(router.has_pending());

        router.take_inbox("receiver", 1_000 + 40_000_000);
        assert_eq!(router.pending_count(), 0);
        assert!(!router.has_pending());
    }

    #[test]
    fn a_connection_to_an_undeclared_port_is_a_typed_refusal() {
        let sender = sys("sender_sys", vec![port("out", PortKind::Signal, PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![]); // no ports declared at all
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", "")],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let err = Router::build(&sos, &systems).unwrap_err();
        assert!(matches!(err, RouterError::UndeclaredPort { ref instance, ref port, .. } if instance == "receiver" && port == "in"), "{err:?}");
    }

    #[test]
    fn a_connection_with_a_direction_mismatch_is_a_typed_refusal() {
        // "out" is declared IN, not OUT/INOUT -- cannot be a connection's from_port.
        let (sos, systems) = two_signal_instances(PortDirection::In, PortDirection::In);
        let err = Router::build(&sos, &systems).unwrap_err();
        assert!(matches!(err, RouterError::DirectionMismatch { ref instance, ref port, .. } if instance == "sender" && port == "out"), "{err:?}");
    }

    #[test]
    fn a_connection_with_a_kind_mismatch_is_a_typed_refusal() {
        let sender = sys("sender_sys", vec![port("out", PortKind::Signal, PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![port("in", PortKind::Cdm, PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", "")],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let err = Router::build(&sos, &systems).unwrap_err();
        assert!(matches!(err, RouterError::KindMismatch { ref from_port, ref to_port, .. } if from_port == "out" && to_port == "in"), "{err:?}");
    }

    #[test]
    fn a_connection_with_a_non_latency_link_model_is_a_typed_refusal() {
        let (mut sos, systems) = two_signal_instances(PortDirection::Out, PortDirection::In);
        sos.connections[0].link_model = "lossy_udp".to_string();
        let err = Router::build(&sos, &systems).unwrap_err();
        assert!(matches!(err, RouterError::UnsupportedLinkModel { ref link_model, .. } if link_model == "lossy_udp"), "{err:?}");
    }

    #[test]
    fn inout_ports_satisfy_either_end_of_a_connection() {
        let (sos, systems) = two_signal_instances(PortDirection::Inout, PortDirection::Inout);
        Router::build(&sos, &systems).expect("INOUT is valid on either end");
    }

    #[test]
    fn a_message_on_a_port_with_no_matching_connection_is_dropped_not_an_error() {
        let (sos, systems) = two_signal_instances(PortDirection::Out, PortDirection::In);
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let mut outbox = Outbox::new();
        outbox.push_signal("some_other_port", 1_000, 1.0);
        router.deliver("sender", 1_000, outbox);
        assert!(router.take_inbox("receiver", 1_000).is_empty());
        assert!(!router.has_pending());
    }

    #[test]
    fn take_inbox_delivers_on_the_boundary_epoch_equal_to_availability_not_only_strictly_after() {
        // Zero latency: availability equals the emission epoch exactly. A step whose own epoch
        // equals that availability must see it -- "at or after", never only "strictly after".
        let (sos, systems) = two_signal_instances(PortDirection::Out, PortDirection::In);
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let mut outbox = Outbox::new();
        outbox.push_signal("out", 5_000, 3.0);
        router.deliver("sender", 5_000, outbox);

        assert!(router.take_inbox("receiver", 4_999).is_empty(), "one ns before availability: held");
        let inbox = router.take_inbox("receiver", 5_000);
        assert_eq!(inbox.messages().len(), 1, "as_of exactly equal to availability delivers -- the boundary is inclusive");
    }

    #[test]
    fn take_inbox_never_loses_messages_across_many_calls_regardless_of_latency() {
        // Several messages, several distinct latencies (including zero), queued in one call and
        // drained across many small `as_of` increments -- total delivered must equal total
        // queued no matter how finely delivery is sliced, proving nothing a held message can be
        // silently skipped by a `take_inbox` call that lands between two of these epochs.
        let sender = sys(
            "sender_sys",
            vec![
                port_with_latency("fast", PortKind::Signal, PortDirection::Out, 0),
                port_with_latency("slow", PortKind::Signal, PortDirection::Out, 40_000_000),
            ],
        );
        let receiver = sys("receiver_sys", vec![port("fast", PortKind::Signal, PortDirection::In), port("slow", PortKind::Signal, PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![
                conn("sender", "fast", "receiver", "fast", LATENCY_LINK_MODEL),
                conn("sender", "slow", "receiver", "slow", LATENCY_LINK_MODEL),
            ],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connections");

        // One `deliver` call per emission epoch -- `deliver`'s own `emission_tai_ns` argument
        // applies to every message in that call's `Outbox` alike (real callers -- `crate::
        // schedule::HeteroScheduler::advance_to_with_ports` -- call it once per native step,
        // with that step's own result epoch), so five separate epochs need five separate calls.
        for k in 0..5i64 {
            let emission_tai_ns = k * 10_000_000;
            let mut outbox = Outbox::new();
            outbox.push_signal("fast", emission_tai_ns, k as f64);
            outbox.push_signal("slow", emission_tai_ns, 100.0 + k as f64);
            router.deliver("sender", emission_tai_ns, outbox);
        }
        let total_queued = 10;

        let mut delivered = 0;
        for as_of in (0..200_000_000).step_by(1_000_000) {
            delivered += router.take_inbox("receiver", as_of).messages().len();
        }
        assert_eq!(delivered, total_queued, "every queued message must eventually be delivered, exactly once, none lost");
        assert!(!router.has_pending(), "nothing left holding once as_of has run past every message's own availability");
    }
}
