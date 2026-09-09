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
//!
//! ## Port traffic recording (`docs/open-questions.md` question 175, M25.4a)
//!
//! [`Router`] is also the sole recorder of `altavista.v1.PortTrafficLog`, the sidecar
//! `crate::drm::executor::execute` writes beside a run's `RunProducts` (`RunProducts.
//! port_traffic_hash`): [`Router::deliver`] is the single choke point every frame this
//! Router ever carries passes through (this module's own doc comment, above), so it is also
//! the one place that can record every one of them without a second, independently
//! maintained tap. See [`Router::deliver`]'s own doc comment for exactly what is and is not
//! recorded, [`Router::begin_step`] for the `sequence` field's own source, and
//! [`Router::take_port_traffic`] for how a caller drains what has been recorded so far.
//!
//! ## Port fault runtime (`docs/open-questions.md` question 178, ADR-005 sec 5, R4.1a/R4.1b)
//!
//! [`Router::install_port_faults`] resolves every `FAULT_TARGET_KIND_PORT` fault of `kind ==
//! "drop"`, `"delay"`, `"corrupt"` or `"duplicate"` -- ADR-005 section 5's whole PORT vocabulary,
//! all four real as of R4.1b -- into an `InstalledPortFault`, and [`Router::deliver`] is where
//! every one of them actually acts, since it is already this crate's one choke point for every
//! frame a run carries (this module's own doc comment, above). `crate::drm::executor::execute`
//! calls `install_port_faults` once, after [`Router::build`], with `Scenario.faults` filtered to
//! `FAULT_TARGET_KIND_PORT`, `Scenario.seeds`, and `options.sample_interval_s` converted to ns
//! (`"duplicate"`'s own "one step later" needs the run's own output period -- see "Duplicate,"
//! below -- and the Router has no other way to learn it) -- before any binding or GMAT call, the
//! same "checked up front" pattern every other load-time refusal in this crate follows.
//!
//! **Matching.** A PORT fault matches the EMITTING pair `(Fault.instance, Fault.target)` --
//! `Fault.target` names the port, not a `Connection`. `install_port_faults` validates, at load,
//! that the named instance actually declares that port (via this Router's own `port_kinds`,
//! already built by [`Router::build`] from every `SosConfiguration.instances` entry's own
//! `SystemDefinition`, not merely the ports a `Connection` happens to name) and that its own
//! `PortKind` is FRAMED or BYTE_STREAM -- [`Router::deliver`] never records or delivers anything
//! else, so a fault naming a SIGNAL/CDM port would have nothing to act on. Either failure is a
//! distinct typed [`RouterError`], never a silent no-op.
//!
//! **Window.** `[Fault.tai_ns, Fault.tai_ns + duration_ns)`, half-open, compared against the
//! frame's own emission epoch (never the receiver's later arrival epoch). `duration_ns == 0`
//! means persistent to the end of the run, per the proto's own `Fault.duration_ns` doc comment.
//! **`Fault.clear == true` on a PORT fault is refused** ([`RouterError::
//! PortFaultClearNotSupported`]), not honoured: which field would tie a later "clear" `Fault` to
//! the earlier one it ends -- the same `id` at a later `tai_ns`? a shared `instance`/`target`?
//! something else? -- is undefined by both the proto and ADR-005 section 5, and guessing would
//! silently become an unreviewed part of the contract the moment a caller relied on it. Refusing
//! it, typed, keeps that decision open for the manager rather than baking in a guess (see this
//! crate's own `R4_1A_REPORT.md` for the escalation).
//!
//! **Rate and the seeded stream (question 137-style per-fault substreams).** `Fault.params
//! ["rate"]` is the per-candidate-frame probability the fault actually applies, in `[0.0, 1.0]`
//! ([`RouterError::InvalidPortFaultRate`] otherwise); absent means `1.0` (every frame in the
//! window). Every PORT fault requires its own `Scenario.seeds[fault.id]` entry, checked at load
//! ([`RouterError::MissingFaultSeed`], which `crate::drm::executor::execute` maps to the
//! crate-wide `crate::drm::DrmError::MissingFaultSeed`) -- **even at `rate == 1.0`**: uniform and
//! honest, so declaring a `"rate"` later never changes whether a DRM loads. `install_port_faults`
//! constructs exactly one [`crate::rng::Pcg64`] per installed fault, once, from that seed; every
//! candidate frame on the fault's own `(instance, port)` inside its window draws exactly one
//! `bernoulli(rate)` from that fault's own stream, in [`Router::deliver`]'s own deterministic
//! per-message order -- **even at `rate == 1.0`**, so a run's own stream position, and therefore
//! every OTHER fault's own draws, never depends on any one fault's realized outcome. Two PORT
//! faults never share a stream (each gets its own `Pcg64`, seeded independently from its own
//! `Scenario.seeds` entry), so changing one fault's seed changes only that fault's own outcomes
//! -- pinned directly, with many candidate frames and a hand-computed reference stream, by this
//! module's own `mod tests`.
//!
//! **What the log records ("keep the OUT, gate the IN; the OUT is always the emitter's own
//! original bytes").** `"drop"` suppresses delivery AND every IN [`PortTrafficRecord`] for the
//! affected message, but KEEPS the OUT record -- the emitter genuinely emitted; the router is
//! what dropped it. This is the SAME rule this module already applies to a message on a port with
//! no `Connection` at all ([`Router::deliver`]'s own doc comment: "a FRAMED port with no
//! connection still gets an OUT record and no IN record") -- a drop fault is simply another
//! reason delivery does not happen, recorded identically. This is also what keeps replay of the
//! EMITTING instance honest: replay plays back OUT frames, and an interior gap in the emitter's
//! own OUT record sequence would trip the missing-frame refusal for the wrong reason
//! (`crate::drm::replay`'s own module doc comment). `"delay"` leaves BOTH records exactly as an
//! unfaulted delivery would -- `PortTrafficRecord.tai_ns` is always the emission epoch, for the
//! IN record too ([`Router::deliver`]'s own doc comment), so a fault delay never appears there at
//! all -- but the *delivered* [`av_cdm::pb::PortMessage`]'s own availability epoch is later:
//! `Fault.params["delay_s"]` (seconds, `f64`, required for `kind == "delay"` --
//! [`RouterError::MissingPortFaultDelay`] if absent) converts to ns and is ADDED to that
//! connection's own already-declared latency (this module's own "Link model" section, above),
//! for every edge the emitting port connects to alike (a port-level fault, not a per-connection
//! one) -- question 110's existing delivery rule then applies completely unchanged: available at
//! emission plus TOTAL latency (connection latency + fault delay), delivered at the first
//! receiver step at or after that, never earlier. **`"corrupt"`/`"duplicate"` (R4.1b) both keep
//! this same "OUT is the emitter's own truth" invariant** -- see their own subsections below for
//! exactly what each does to the IN side instead.
//!
//! **Corrupt (R4.1b).** Mutates the payload the RECEIVER sees; the OUT record (recorded before
//! any fault is even consulted -- [`Router::deliver`]'s own code order) is never touched, so it
//! always carries the emitter's own real, original bytes -- the identical "OUT is truth" rule
//! `"drop"` already established, extended to a fault that changes content instead of merely
//! gating delivery. **Params** (validated at install, like `"delay"`'s own `delay_s` --
//! [`RouterError::InvalidPortFaultCorruptMask`] for a declared value that is not an integer in
//! `[0, 255]`): `Fault.params["corrupt_mask"]` -- when present, an 8-bit XOR mask (as an `f64`
//! integer in `[0, 255]`, since `Fault.params` is `map<string, double>` and has no byte type of
//! its own) applied to EVERY byte of the payload, so the same declared mask corrupts a whole
//! frame identically regardless of its length. When `"corrupt_mask"` is **absent**, this fault
//! instead draws a single uniformly-random bit position across the WHOLE payload (eight
//! candidate positions per payload byte) from the fault's own seeded [`crate::rng::Pcg64`]
//! stream -- the same stream `bernoulli(rate)` already draws from, this task's own "a bit flip
//! drawn from the fault's own seeded stream when no mask is declared" instruction -- and flips
//! exactly that one bit. Either way, an empty payload is left untouched (nothing to corrupt). The
//! corrupted bytes
//! become both the IN [`PortTrafficRecord`]'s own payload AND the delivered
//! [`av_cdm::pb::PortMessage`]'s own payload -- the receiver genuinely sees mutated bytes, not
//! merely a mutated log entry it never actually consumes. Delivery TIMING is unaffected (no
//! change to the connection's own declared latency); a corrupt fault changes content, not epoch.
//!
//! **Duplicate (R4.1b).** A second delivery, one native output period after the first -- "the
//! Router does not know the output period today" (this task's own brief), so
//! [`Router::install_port_faults`] takes it as a parameter (`output_period_ns`, from
//! `crate::drm::executor::execute`'s own `options.sample_interval_s`) and stores it per installed
//! `"duplicate"` fault rather than guess or invent a second declared param for something this
//! run already has one true value for. **Decided, and pinned by test (this task's own explicit
//! instruction to decide and state both):**
//! - **The duplicate DOES get its own IN [`PortTrafficRecord`]** -- the receiving instance really
//!   does see a second frame arrive (it is queued a second time, at a later availability epoch,
//!   and a real `take_inbox` call really does return it), so recording nothing for it would make
//!   the sidecar under-report what the receiver actually experienced.
//! - **The OUT side does NOT get a second record** -- there is only ever one real emission; the
//!   router is what fabricates the extra delivery, not the sender emitting twice. Duplicating the
//!   OUT record too would falsely claim the emitter itself sent the frame twice.
//! - **Both IN records share the SAME `tai_ns` (the one real `emission_tai_ns`), never the
//!   offset epoch** -- the identical "`PortTrafficRecord.tai_ns` is always the emission epoch,
//!   never the arrival epoch, never a fault's own added offset" rule `"delay"` already
//!   establishes (a delay's own added ns famously never appears on the record either -- see
//!   above). The two IN records are therefore content-identical in the log (same instance, port,
//!   tai_ns, sequence, payload) -- an intentional, honest way of saying "this one emission was
//!   delivered twice," not a distinguishable pair. What differs, and is real, is the *delivered*
//!   `PortMessage`'s own availability: the second copy is queued at `emission_tai_ns + connection
//!   latency + output_period_ns`, one whole native step after the first (`emission_tai_ns +
//!   connection latency`) -- so a receiver's `take_inbox` genuinely returns the frame at two
//!   different steps, never both at once. Delivered PAYLOAD is the emitter's own original bytes,
//!   unmutated (a duplicate does not also corrupt; combining the two would need overlapping
//!   windows on one port, which R4.1b now refuses at load -- see "Overlapping windows," below).
//!
//! **Overlapping windows are refused at load (`docs/open-questions.md` questions 184/186(b),
//! R4.1b).** Two PORT faults on the identical `(instance, port)` whose `[start, end)` windows
//! overlap are [`RouterError::OverlappingPortFaultWindows`], naming both fault ids and the
//! overlapping interval -- checked once per newly-installed fault, against every fault already
//! installed on the same `(instance, port)`, inside [`Router::install_port_faults`]'s own
//! all-or-nothing validation loop. `duration_ns == 0` ("persistent to the end of the run," the
//! proto's own doc comment) means the fault's own window has no declared end, so it overlaps
//! everything at or after its own start on that port -- a second fault on the same port, at any
//! later epoch, always conflicts with a persistent one. Two faults on the same port with
//! genuinely DISJOINT windows stay legal (a `[start, end)` ending exactly where another begins is
//! disjoint, not overlapping -- half-open intervals, the identical convention "Window," above,
//! already uses). **This retires R4.1a's own "delays from every applying fault sum" path**: with
//! overlap refused at load, more than one installed PORT fault can never match the SAME candidate
//! frame on the SAME port any more (only one fault's window can ever contain a given emission
//! epoch), so that summing code is unreachable and has been deleted, not left as dead code behind
//! an unexercised branch -- see `R4_1B_REPORT.md`. **The "every applicable fault always draws,
//! unconditionally" rule (above) is still real and still meaningful**: a fault whose window does
//! NOT contain this emission epoch still costs it nothing (it is simply not "applicable" and
//! never draws), and a fault on a DIFFERENT port is untouched regardless -- what is retired is
//! only the "two faults apply to the identical frame" case, which overlap-refusal now makes
//! structurally impossible.
//!
//! **Events ("one per fault, at its first real effect," now carrying a frame count too).** Every
//! PORT fault [`Router::deliver`] genuinely applies at least once (a frame actually affected, not
//! merely a frame that passed through its declared window) is drained, once, by [`Router::
//! take_applied_port_faults`] -- one `AppliedPortFault` per fault id, carrying the epoch of its
//! FIRST applied frame (never one entry per frame) AND, as of R4.1b (question 186(c)), the TOTAL
//! count of frames it affected over the whole run (`AppliedPortFault::frames_affected`).
//! `crate::drm::executor::execute` turns each into exactly one `EVENT_KIND_FAULT` `Event`
//! (`crate::drm::events::port_fault_event`), `reference_id = Fault.id` (the proto's own
//! `Event.reference_id` doc comment: "for faults: the Fault id in the DRM"), with
//! `values["frames_affected"]` set from that same count -- still ONE event per fault (this is a
//! count carried BY that one event, not a reason to emit more of them). A fault that never
//! actually applies (its window never coincided with a real frame, or `rate < 1.0` and every draw
//! missed) produces no event at all -- symmetric with a DYNAMICS fault outside its executed span
//! (`crate::drm::events::declared_events`). "First real effect, with a count" is a deliberate
//! choice among several honest options (one event per applied frame; one per fault regardless of
//! whether it ever fired) -- it is what keeps a drop fault active for hundreds of steps from
//! producing hundreds of indistinguishable events (`docs/open-questions.md` question 137's
//! "record changes only" rule, applied here the same way `crate::drm::events`'s own
//! `EVENT_KIND_PORT_COMMAND`/`EVENT_KIND_CONTACT_START`/`_END` already apply it to a
//! continuously-driven signal) while the new count keeps "how much did this fault really do"
//! honestly recoverable from the one event, without a second event kind for "window ended."
//! **`PortTrafficRecord` itself still carries no attribute tying a record back to the fault that
//! caused it** -- deliberately: the join is `(tai_ns, instance, port)` plus the fault's own
//! declared window, entirely derivable from the DRM a replay already has (mirrors this module's
//! own "the arrival epoch is derivable, not stored twice" reasoning for latency). R4.1a's own
//! escalation about this join becoming ambiguous under overlapping windows is now moot: R4.1b
//! refuses that shape at load entirely (see "Overlapping windows," above), so the join is
//! unambiguous for every DRM that can ever load.
//!
//! **Replay re-applies every installed PORT fault -- deliberately, and this is what makes replay
//! of a faulted run reproduce the faulted outcome (`crate::drm::replay`'s own module doc
//! comment).** `Router::install_port_faults` is called unconditionally by `crate::drm::executor::
//! execute`, whether or not `RunConfig.replay` is set, and [`Router::deliver`] has no notion of
//! "this Outbox came from a replayed instance" at all -- a replayed instance's own emissions
//! reach `deliver` through the identical `HeteroScheduler::advance_to_with_ports` call path a
//! real model's do. This is safe, not merely convenient, BECAUSE of the "OUT is always the
//! emitter's own original bytes" invariant above: `crate::drm::replay::ReplayModel` only ever
//! plays back OUT frames (never IN), so what it re-emits during a replay run is bit-for-bit the
//! SAME pre-fault content, at the SAME epochs, in the SAME order, as the original run's own real
//! emitter produced -- the fault's own seeded stream therefore sees the identical sequence of
//! candidate frames on its own `(instance, port)` and draws the identical outcomes, reproducing
//! drop/delay/corrupt/duplicate's own effect exactly. See `crate::drm::replay`'s own module doc
//! comment for the full account (including why this is NOT the same thing as applying a fault
//! twice to one frame within a single run).
//!
//! **Epoch grid.** Unlike a DYNAMICS/HARDWARE fault (which must land on the trajectory's own
//! output sampling grid, since it splits a real propagation segment), a PORT fault's own
//! `Fault.tai_ns`/`duration_ns` need NOT land on that grid: it splits no segment, only gates
//! individual frames already flowing through `Router::deliver` at their own real emission epochs
//! -- `crate::drm::executor::execute`'s own load-time loop does not apply
//! `DrmError::FaultEpochNotOnSampleGrid` to a PORT fault, pinned by
//! `tests/port_faults.rs::a_port_fault_off_the_sample_grid_still_loads_and_applies`.

use std::collections::BTreeMap;

use av_cdm::pb::{Connection, Fault, FaultTargetKind, Port, PortDirection, PortKind, PortTrafficRecord, SosConfiguration, SystemDefinition};
use av_dynamics::Outbox;

use crate::ports::{sorted_inbox, Inbox, QueuedMessage};
use crate::rng::{seed_for, Pcg64};

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
    /// A `FAULT_TARGET_KIND_PORT` fault's own `target` named no declared port of `instance`'s
    /// own `SystemDefinition` -- question 178, [`Router::install_port_faults`]'s own "validate at
    /// load that the instance declares that port" rule.
    UndeclaredPortFaultTarget { fault_id: String, instance: String, port: String },
    /// A `FAULT_TARGET_KIND_PORT` fault named a real, declared port whose own `PortKind` is
    /// neither FRAMED nor BYTE_STREAM -- [`Router::deliver`] only ever records/delivers frames on
    /// those two kinds, so a fault naming a SIGNAL/CDM port would have nothing to act on.
    PortFaultTargetNotFramed { fault_id: String, instance: String, port: String, kind: String },
    /// A `FAULT_TARGET_KIND_PORT` fault's own `kind` was not one of ADR-005 section 5's own four
    /// documented PORT kinds (`"drop"`, `"delay"`, `"corrupt"`, `"duplicate"` -- all four real as
    /// of R4.1b) -- outside the whole documented vocabulary entirely.
    /// `crate::drm::executor::execute`'s own load-time loop already refuses this shape earlier
    /// with [`crate::drm::DrmError::UnknownPortFaultKind`], so a caller going through `execute()`
    /// never actually reaches this variant; a caller reaching [`Router::install_port_faults`]
    /// directly (as this module's own unit tests do) gets it for any kind outside the vocabulary.
    UnsupportedPortFaultKind { fault_id: String, kind: String },
    /// `kind == "delay"` but `Fault.params` has no `"delay_s"` entry -- required, never defaulted
    /// (the module doc comment's "Rate and the seeded stream" section).
    MissingPortFaultDelay { fault_id: String },
    /// `Fault.params["rate"]` is set but outside `[0.0, 1.0]` -- not a valid probability.
    InvalidPortFaultRate { fault_id: String, rate: f64 },
    /// R4.1b: `kind == "corrupt"` declared `Fault.params["corrupt_mask"]`, but its value is not
    /// an integer in `[0, 255]` -- `Fault.params` is `map<string, double>` (no byte type of its
    /// own), so this is the runtime's own "is this actually a valid byte value" check, the same
    /// role [`RouterError::InvalidPortFaultRate`] plays for `"rate"`. See the module doc
    /// comment's "Corrupt" section for exactly what a declared mask does.
    InvalidPortFaultCorruptMask { fault_id: String, mask: f64 },
    /// R4.1b (`docs/open-questions.md` questions 184/186(b)): two `FAULT_TARGET_KIND_PORT`
    /// faults on the identical `(instance, port)` declared `[start, end)` windows that overlap --
    /// see the module doc comment's "Overlapping windows are refused at load" section for the
    /// full contract (including why `duration_ns == 0` overlaps everything at or after its own
    /// start). `overlap_end_tai_ns` is `None` when both faults' own windows are persistent
    /// (`duration_ns == 0`), mirroring [`InstalledPortFault::end_tai_ns`]'s own "`None` ==
    /// persistent" convention.
    OverlappingPortFaultWindows { fault_a: String, fault_b: String, instance: String, port: String, overlap_start_tai_ns: i64, overlap_end_tai_ns: Option<i64> },
    /// `Fault.clear == true` on a `FAULT_TARGET_KIND_PORT` fault -- refused, not silently
    /// ignored. See the module doc comment's "Port fault runtime" section, "Window," for why this
    /// design does not honour `clear` for a PORT fault.
    PortFaultClearNotSupported { fault_id: String },
    /// No `Scenario.seeds[fault.id]` entry for a `FAULT_TARGET_KIND_PORT` fault -- every PORT
    /// fault requires a seed, even at `rate == 1.0` (the module doc comment's "Rate and the
    /// seeded stream" section). `crate::drm::executor::execute` maps this one variant to
    /// `crate::drm::DrmError::MissingFaultSeed`, reusing the crate-wide variant, rather than
    /// wrapping it generically through `DrmError::Router` -- see that call site's own comment.
    MissingFaultSeed { fault_id: String },
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
            RouterError::UndeclaredPortFaultTarget { fault_id, instance, port } => {
                write!(f, "fault {fault_id:?}: instance {instance:?} declares no port named {port:?}")
            }
            RouterError::PortFaultTargetNotFramed { fault_id, instance, port, kind } => {
                write!(f, "fault {fault_id:?}: instance {instance:?} port {port:?} has kind {kind} but a PORT fault can only target a FRAMED or BYTE_STREAM port")
            }
            RouterError::UnsupportedPortFaultKind { fault_id, kind } => {
                write!(f, "fault {fault_id:?}: kind {kind:?} is not one of ADR-005 section 5's own documented PORT fault kinds (\"drop\", \"delay\", \"corrupt\", \"duplicate\")")
            }
            RouterError::MissingPortFaultDelay { fault_id } => {
                write!(f, "fault {fault_id:?}: kind \"delay\" requires params[\"delay_s\"], which is absent")
            }
            RouterError::InvalidPortFaultRate { fault_id, rate } => {
                write!(f, "fault {fault_id:?}: params[\"rate\"] = {rate} is outside [0.0, 1.0]")
            }
            RouterError::InvalidPortFaultCorruptMask { fault_id, mask } => {
                write!(f, "fault {fault_id:?}: params[\"corrupt_mask\"] = {mask} is not an integer in [0, 255]")
            }
            RouterError::OverlappingPortFaultWindows { fault_a, fault_b, instance, port, overlap_start_tai_ns, overlap_end_tai_ns } => {
                let end = overlap_end_tai_ns.map(|e| e.to_string()).unwrap_or_else(|| "end of run".to_string());
                write!(f, "faults {fault_a:?} and {fault_b:?}: both target instance {instance:?} port {port:?} with overlapping windows ([{overlap_start_tai_ns}, {end}))")
            }
            RouterError::PortFaultClearNotSupported { fault_id } => {
                write!(f, "fault {fault_id:?}: clear = true on a FAULT_TARGET_KIND_PORT fault is not supported")
            }
            RouterError::MissingFaultSeed { fault_id } => {
                write!(f, "fault {fault_id:?} needs a random draw but Scenario.seeds has no entry keyed by its id")
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

/// The four `Fault.kind` values ADR-005 section 5 documents for `FAULT_TARGET_KIND_PORT`, all
/// real as of R4.1b -- see the module doc comment's "Port fault runtime" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortFaultKind {
    Drop,
    Delay,
    Corrupt,
    Duplicate,
}

/// The effect one genuinely-applying [`InstalledPortFault`] has on a single candidate frame,
/// resolved once per [`Router::deliver`] call by the fault-matching loop -- see that method's own
/// body. Overlap-refusal at install time ("Overlapping windows are refused at load," the module
/// doc comment) guarantees at most one installed fault ever matches (window-contains) the same
/// `(instance, port, emission_tai_ns)` triple, so `deliver` only ever resolves at most one of
/// these per message, never several to combine.
enum PortFaultEffect {
    Drop,
    /// Extra delay (ns) ADDED to the connection's own already-declared latency.
    Delay(i64),
    /// The corrupted payload the receiver actually sees (the OUT record keeps the original,
    /// unmutated bytes regardless -- module doc comment, "Corrupt").
    Corrupt(Vec<u8>),
    /// Offset (ns) of the SECOND delivery, added on top of the connection's own normal latency
    /// (module doc comment, "Duplicate").
    Duplicate(i64),
}

/// One resolved `FAULT_TARGET_KIND_PORT` fault, built once by [`Router::install_port_faults`]
/// from a `Fault` + its own `Scenario.seeds` entry -- see the module doc comment's "Port fault
/// runtime" section for exactly what each field means and how [`Router::deliver`] consults it.
#[derive(Debug, Clone)]
struct InstalledPortFault {
    id: String,
    instance: String,
    port: String,
    kind: PortFaultKind,
    start_tai_ns: i64,
    /// `None` == persistent to the end of the run (`Fault.duration_ns == 0`).
    end_tai_ns: Option<i64>,
    rate: f64,
    /// Only meaningful for `PortFaultKind::Delay` (`Fault.params["delay_s"]`, converted to ns).
    delay_ns: i64,
    /// Only meaningful for `PortFaultKind::Corrupt`. `Some(mask)` == `Fault.params
    /// ["corrupt_mask"]`, declared and validated at install (`[0, 255]`); `None` == no mask
    /// declared, so [`Router::deliver`] draws a random bit position from `rng` instead (module
    /// doc comment, "Corrupt").
    corrupt_mask: Option<u8>,
    /// Only meaningful for `PortFaultKind::Duplicate`: the run's own `output_period_ns`
    /// (`Router::install_port_faults`'s own parameter), i.e. exactly "one step later" (module doc
    /// comment, "Duplicate"). Zero for every other kind.
    duplicate_offset_ns: i64,
    rng: Pcg64,
    /// `None` until [`Router::deliver`] first genuinely applies this fault (a frame actually
    /// affected); set once, to that first epoch, and never overwritten again -- see
    /// [`Router::take_applied_port_faults`]'s own doc comment for why only the first epoch is
    /// kept.
    first_applied_tai_ns: Option<i64>,
    /// R4.1b (question 186(c)): total count of frames [`Router::deliver`] has genuinely applied
    /// this fault to so far, over the whole run -- never reset except by
    /// [`Router::take_applied_port_faults`]'s own drain. Unlike `first_applied_tai_ns`, this
    /// keeps counting after the first application.
    frames_affected: u64,
}

/// One `FAULT_TARGET_KIND_PORT` fault [`Router::deliver`] genuinely applied at least once during
/// this run -- [`Router::take_applied_port_faults`]'s own return element. See the module doc
/// comment's "Events" section for exactly what "genuinely applied" and "first epoch only" mean.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedPortFault {
    pub fault_id: String,
    pub instance: String,
    pub port: String,
    /// The epoch of this fault's FIRST applied frame -- never the fault's own declared
    /// `Fault.tai_ns` window start, and never repeated for a later applied frame.
    pub applied_tai_ns: i64,
    /// R4.1b (question 186(c)): the TOTAL count of frames this fault affected over the whole
    /// run, not merely at its first applied frame -- see [`InstalledPortFault::frames_affected`]'s
    /// own doc comment.
    pub frames_affected: u64,
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
    /// (instance, port) -> that port's own declared `PortKind`, covering EVERY declared port
    /// of every `SosConfiguration.instances` entry's own `SystemDefinition` -- not only ports
    /// a `Connection` names (question 175: an emitter's own port may have no connection at
    /// all). Built once, by [`Router::build`]. [`Router::deliver`] consults this to decide
    /// whether an emitted message is FRAMED/BYTE_STREAM (recorded into `port_traffic`) or
    /// SIGNAL/CDM (never recorded), and to detect an emission on a port this Router has no
    /// record of at all.
    port_kinds: BTreeMap<(String, String), PortKind>,
    /// Monotonic step counter (question 175, M25.4a) -- see [`Router::begin_step`]'s own doc
    /// comment for exactly when this advances and why it is never reset mid-run.
    step: u64,
    /// Every FRAMED/BYTE_STREAM frame this Router has recorded so far this run (question
    /// 175). See [`Router::deliver`]'s own doc comment for exactly what is and is not
    /// recorded, and [`Router::take_port_traffic`] for how a caller drains this.
    port_traffic: Vec<PortTrafficRecord>,
    /// How many messages [`Router::deliver`] has seen on a port with no declared `PortKind` at
    /// all, and therefore could not classify as recordable or not -- see that method's own doc
    /// comment for the real, legitimate case this counts (`crate::drm::sensors`'s truth
    /// broadcast ports) and why skipping them loses nothing. Never reset; read by
    /// [`Router::undeclared_port_emissions`].
    undeclared_port_emissions: u64,
    /// Every `FAULT_TARGET_KIND_PORT` fault [`Router::install_port_faults`] has resolved and
    /// installed (question 178, R4.1a) -- see the module doc comment's "Port fault runtime"
    /// section. Empty for every Router built before this field existed and for one no caller
    /// ever installs faults onto (`Router::empty`, every existing test that does not construct
    /// this list). Sorted `(tai_ns, id)` at install time, so [`Router::deliver`]'s own iteration
    /// order over faults matching one `(instance, port)` is deterministic and stable regardless
    /// of the order `Scenario.faults` happened to declare them in.
    port_faults: Vec<InstalledPortFault>,
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

/// R4.1b: the "corrupt" PORT fault's own byte-mutation, applied to a COPY of `payload` (the
/// caller's own original bytes -- e.g. the OUT record's -- are never touched; see the module doc
/// comment's "Corrupt" section for the full contract this implements). `mask`, when `Some`, is
/// the declared `Fault.params["corrupt_mask"]` (already validated to `[0, 255]` by
/// [`Router::install_port_faults`]), XORed into EVERY byte. When `mask` is `None`, draws exactly
/// one uniformly-random bit position across the whole payload from `rng` (the fault's own seeded
/// stream -- the SAME stream `bernoulli(rate)` already drew from for this candidate frame) and
/// flips that one bit. An empty payload is returned unchanged either way -- nothing to corrupt,
/// and no bit position to draw from a zero-length range.
fn corrupt_payload(payload: &[u8], mask: Option<u8>, rng: &mut Pcg64) -> Vec<u8> {
    let mut bytes = payload.to_vec();
    match mask {
        Some(m) => {
            for b in bytes.iter_mut() {
                *b ^= m;
            }
        }
        None => {
            if !bytes.is_empty() {
                let bit_pos = rng.next_u64() % (bytes.len() as u64 * 8);
                let byte_idx = (bit_pos / 8) as usize;
                let bit_idx = (bit_pos % 8) as u32;
                bytes[byte_idx] ^= 1u8 << bit_idx;
            }
        }
    }
    bytes
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

        // Question 175 (M25.4a): every declared port of every instance, not only ports a
        // Connection names -- see `port_kinds`'s own field doc comment. An instance whose
        // `system_id` names no supplied `SystemDefinition` contributes no ports here; that is
        // not this function's own refusal to raise (a `Connection`-touched instance already
        // gets `RouterError::UnknownSystemDefinition` above; an instance no `Connection` ever
        // names is `execute()`'s Pass 1 (`DrmError::UnknownSystemDefinition`) to refuse, before
        // this Router is ever asked to deliver anything).
        let mut port_kinds: BTreeMap<(String, String), PortKind> = BTreeMap::new();
        for instance in &sos.instances {
            let Some(sys) = systems.get(&instance.system_id) else { continue };
            for port in &sys.ports {
                let kind = PortKind::try_from(port.kind).unwrap_or(PortKind::Unspecified);
                port_kinds.insert((instance.name.clone(), port.name.clone()), kind);
            }
        }

        Ok(Router { edges, pending: BTreeMap::new(), port_kinds, step: 0, port_traffic: Vec::new(), undeclared_port_emissions: 0, port_faults: Vec::new() })
    }

    /// Resolve and install every `FAULT_TARGET_KIND_PORT` fault in `faults` (a caller typically
    /// passes the whole `Scenario.faults`; every non-PORT entry is simply skipped) against
    /// `seeds` (`Scenario.seeds`) -- question 178, R4.1a. See the module doc comment's "Port
    /// fault runtime" section for the full contract: matching, window, rate/seed, `clear`. Called
    /// once, by `crate::drm::executor::execute`, after [`Router::build`] and before any
    /// binding/GMAT call; replaces whatever this Router previously had installed (idempotent for
    /// a caller that calls it exactly once, which is every real caller today).
    ///
    /// All-or-nothing on error: `self.port_faults` is only ever replaced once every fault in
    /// `faults` has validated successfully, so a caller never ends up with a partially-installed
    /// set (mirrors [`Router::build`]'s own all-or-nothing contract for `SosConfiguration.
    /// connections`) -- the first fault (in `(tai_ns, id)` order) that fails validation is the one
    /// named in the returned error, and nothing after it is even checked.
    ///
    /// `output_period_ns` is the run's own native output period (`crate::drm::executor::execute`'s
    /// own `options.sample_interval_s`, converted to ns) -- R4.1b's `"duplicate"` kind needs it
    /// (module doc comment, "Duplicate"); every other kind ignores it.
    pub fn install_port_faults(&mut self, faults: &[Fault], seeds: &BTreeMap<String, u64>, output_period_ns: i64) -> Result<(), RouterError> {
        let mut relevant: Vec<&Fault> = faults.iter().filter(|f| f.target_kind == FaultTargetKind::Port as i32).collect();
        // ADR-005 sec 5's `(epoch, id)` tie-break (`crate::drm::fault::epoch_id_order`'s own
        // rule, restated here rather than imported to keep this module free of a dependency on
        // `crate::drm` -- see the module doc comment's own note on layering).
        relevant.sort_by(|a, b| (a.tai_ns, a.id.as_str()).cmp(&(b.tai_ns, b.id.as_str())));

        let mut installed: Vec<InstalledPortFault> = Vec::with_capacity(relevant.len());
        for f in relevant {
            if f.clear {
                return Err(RouterError::PortFaultClearNotSupported { fault_id: f.id.clone() });
            }
            let kind = match f.kind.as_str() {
                "drop" => PortFaultKind::Drop,
                "delay" => PortFaultKind::Delay,
                "corrupt" => PortFaultKind::Corrupt,
                "duplicate" => PortFaultKind::Duplicate,
                other => return Err(RouterError::UnsupportedPortFaultKind { fault_id: f.id.clone(), kind: other.to_string() }),
            };
            let port_kind = self.port_kinds.get(&(f.instance.clone(), f.target.clone())).copied();
            let Some(port_kind) = port_kind else {
                return Err(RouterError::UndeclaredPortFaultTarget { fault_id: f.id.clone(), instance: f.instance.clone(), port: f.target.clone() });
            };
            if !matches!(port_kind, PortKind::Framed | PortKind::ByteStream) {
                return Err(RouterError::PortFaultTargetNotFramed { fault_id: f.id.clone(), instance: f.instance.clone(), port: f.target.clone(), kind: kind_name(port_kind as i32) });
            }
            let rate = f.params.get("rate").copied().unwrap_or(1.0);
            if !(0.0..=1.0).contains(&rate) {
                return Err(RouterError::InvalidPortFaultRate { fault_id: f.id.clone(), rate });
            }
            let delay_ns = match kind {
                PortFaultKind::Delay => {
                    let delay_s = f.params.get("delay_s").copied().ok_or_else(|| RouterError::MissingPortFaultDelay { fault_id: f.id.clone() })?;
                    (delay_s * 1e9).round() as i64
                }
                PortFaultKind::Drop | PortFaultKind::Corrupt | PortFaultKind::Duplicate => 0,
            };
            let corrupt_mask = match kind {
                PortFaultKind::Corrupt => match f.params.get("corrupt_mask").copied() {
                    Some(mask) if mask.fract() == 0.0 && (0.0..=255.0).contains(&mask) => Some(mask as u8),
                    Some(mask) => return Err(RouterError::InvalidPortFaultCorruptMask { fault_id: f.id.clone(), mask }),
                    None => None,
                },
                PortFaultKind::Drop | PortFaultKind::Delay | PortFaultKind::Duplicate => None,
            };
            let duplicate_offset_ns = match kind {
                PortFaultKind::Duplicate => output_period_ns,
                PortFaultKind::Drop | PortFaultKind::Delay | PortFaultKind::Corrupt => 0,
            };
            let seed = seed_for(seeds, &f.id).ok_or_else(|| RouterError::MissingFaultSeed { fault_id: f.id.clone() })?;
            let end_tai_ns = if f.duration_ns == 0 { None } else { Some(f.tai_ns + f.duration_ns) };

            // Question 184/186(b) (R4.1b): refuse an overlapping window on the identical
            // (instance, port) -- see the module doc comment's "Overlapping windows are refused
            // at load" section. Checked against every fault already validated and pushed into
            // `installed` this call (faults are processed in (tai_ns, id) order, so `existing` is
            // always the earlier-sorted one of the pair).
            for existing in &installed {
                if existing.instance != f.instance || existing.port != f.target {
                    continue;
                }
                let existing_end = existing.end_tai_ns.unwrap_or(i64::MAX);
                let new_end = end_tai_ns.unwrap_or(i64::MAX);
                let overlap_start = existing.start_tai_ns.max(f.tai_ns);
                let overlap_end = existing_end.min(new_end);
                if overlap_start < overlap_end {
                    let overlap_end_tai_ns = if existing.end_tai_ns.is_none() && end_tai_ns.is_none() { None } else { Some(overlap_end) };
                    return Err(RouterError::OverlappingPortFaultWindows {
                        fault_a: existing.id.clone(),
                        fault_b: f.id.clone(),
                        instance: f.instance.clone(),
                        port: f.target.clone(),
                        overlap_start_tai_ns: overlap_start,
                        overlap_end_tai_ns,
                    });
                }
            }

            installed.push(InstalledPortFault {
                id: f.id.clone(),
                instance: f.instance.clone(),
                port: f.target.clone(),
                kind,
                start_tai_ns: f.tai_ns,
                end_tai_ns,
                rate,
                delay_ns,
                corrupt_mask,
                duplicate_offset_ns,
                rng: Pcg64::new(seed),
                first_applied_tai_ns: None,
                frames_affected: 0,
            });
        }
        self.port_faults = installed;
        Ok(())
    }

    /// Drain and return every [`AppliedPortFault`] this Router genuinely applied at least once so
    /// far (question 178, R4.1a) -- see the module doc comment's "Port fault runtime" section,
    /// "Events," for exactly what "genuinely applied" and "first epoch only" mean.
    /// `crate::drm::executor::execute` calls this exactly once, alongside [`Router::
    /// pending_count`]/[`Router::take_port_traffic`], once every span of the run has finished.
    /// **Destructive**: a fault already drained here, and never applied again before a second
    /// call, does not reappear -- mirrors [`Router::take_port_traffic`]'s own "drain, do not
    /// repeat" contract. A fault installed but never applied at all contributes nothing, ever
    /// (never a zero/empty placeholder entry).
    pub fn take_applied_port_faults(&mut self) -> Vec<AppliedPortFault> {
        self.port_faults
            .iter_mut()
            .filter_map(|pf| {
                let applied_tai_ns = pf.first_applied_tai_ns.take()?;
                let frames_affected = std::mem::take(&mut pf.frames_affected);
                Some(AppliedPortFault { fault_id: pf.id.clone(), instance: pf.instance.clone(), port: pf.port.clone(), applied_tai_ns, frames_affected })
            })
            .collect()
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
    ///
    /// **Port traffic recording (question 175, M25.4a).** For every message whose emitting
    /// port's own declared `PortKind` is FRAMED or BYTE_STREAM, this records exactly one OUT
    /// `PortTrafficRecord` (`instance = from_instance`, `port = message.port`, `tai_ns =
    /// emission_tai_ns`) plus, for every connected receiver edge, exactly one IN record
    /// (`instance = edge.to_instance`, `port = edge.to_port`). **The IN record's own `tai_ns`
    /// is `emission_tai_ns` too, not the receiver's later arrival epoch** -- `PortTrafficRecord
    /// .tai_ns`'s own proto doc comment ("the emission epoch") makes this the field's
    /// contract, not an oversight: the arrival epoch is always `emission_tai_ns + that
    /// connection's own declared latency`, entirely derivable from the `SosConfiguration` a
    /// replay already has, so storing it twice would only be a second, redundant place for the
    /// two numbers to silently disagree. Both records share one `sequence` (`self.step`,
    /// [`Router::begin_step`]'s own doc comment), since both come from the one `deliver` call
    /// that this step's own emission produced.
    ///
    /// A message on a FRAMED/BYTE_STREAM port with **no** connection still gets its own OUT
    /// record and no IN record at all -- deliberate, the identical "a port with no connection
    /// is not wired anywhere, not an error" rule this method already applies to delivery
    /// itself, restated for recording (mirrors question 176's identical fact for `Measurement`
    /// production: a packet the router never delivers still gets recorded at its own emitting
    /// end). A SIGNAL or CDM port is never recorded, at either end, regardless of connections
    /// -- question 175's own scope is FRAMED/BYTE_STREAM only (`RunProducts.port_traffic_hash`'s
    /// own proto doc comment).
    ///
    /// **An emission on a port this Router has no declared `PortKind` for at all is recorded as
    /// nothing, and is not an error.** M25.4a's own first implementation made this a
    /// `debug_assert!`, on the reasoning that a model can only ever emit on a port it was itself
    /// configured with. **That reasoning is wrong, and the manager's own gate run proved it:**
    /// [`crate::drm::sensors::TruthBroadcastAttitude`] broadcasts the truth quaternion and body
    /// rates every step on the seven fixed conventional port names
    /// [`crate::drm::sensors::TRUTH_PORT_NAMES`] (`truth_qx`..`truth_wz`) whether or not the
    /// emitting instance's own `SystemDefinition` declares them -- so `drms/
    /// demo_attitude_precession` and `drms/demo_attitude_wheel_fault`, which declare no ports at
    /// all, legitimately emit seven undeclared-port messages per step, and five
    /// `crates/av-kernel/tests/drm_attitude.rs` tests panicked on that assertion.
    ///
    /// Skipping such a message loses nothing a replay could ever have used, and that is
    /// checkable rather than merely plausible: [`Router::build`] refuses any `Connection` naming
    /// a port the endpoint's own `SystemDefinition` does not declare
    /// ([`RouterError::UndeclaredPort`]), so an undeclared port is guaranteed to have no `edges`
    /// entry -- nothing is ever delivered from it, to anyone, and there is therefore nothing for
    /// a replay binding to play back.
    ///
    /// It is skipped, but never *silently*: every such emission is counted
    /// ([`Router::undeclared_port_emissions`]) and `crate::drm::executor::execute` carries a
    /// non-zero count into the sidecar's own `PortTrafficLog.provenance.attributes[
    /// "undeclared_port_emissions"]`, so a reader of a sidecar can always tell how much traffic
    /// this Router saw and could not classify. There is no zero-valued attribute, by design --
    /// the same convention `crate::drm::events::dropped_messages_event` already follows for
    /// in-flight messages.
    ///
    /// **Port faults (question 178, R4.1a).** After recording the OUT record above, this method
    /// consults every [`Router::install_port_faults`]-installed fault matching this message's own
    /// `(from_instance, message.port)` -- see the module doc comment's "Port fault runtime"
    /// section for the complete contract (matching, window, rate/seed, what a "drop" vs. "delay"
    /// fault changes about the OUT/IN records and delivery below).
    pub fn deliver(&mut self, from_instance: &str, emission_tai_ns: i64, outbox: Outbox) {
        for message in outbox.into_messages() {
            let kind = self.port_kinds.get(&(from_instance.to_string(), message.port.clone())).copied();
            if kind.is_none() {
                self.undeclared_port_emissions += 1;
            }
            let recordable = matches!(kind, Some(PortKind::Framed) | Some(PortKind::ByteStream));
            if recordable {
                self.port_traffic.push(PortTrafficRecord {
                    instance: from_instance.to_string(),
                    port: message.port.clone(),
                    direction: PortDirection::Out as i32,
                    tai_ns: emission_tai_ns,
                    payload: message.payload.clone(),
                    sequence: self.step,
                });
            }

            // Question 178 (R4.1a/R4.1b): the port fault runtime -- see the module doc comment's
            // "Port fault runtime" section for the full contract. Every installed PORT fault
            // whose `(instance, port)` matches this emission and whose window contains
            // `emission_tai_ns` draws exactly one `bernoulli(rate)` from its own seeded
            // substream, UNCONDITIONALLY (even at `rate == 1.0`, even though only one fault can
            // ever end up applying -- overlap-refusal at install time, "Overlapping windows are
            // refused at load") -- so a run's own stream position never depends on any fault's
            // realized outcome.
            let mut effect: Option<PortFaultEffect> = None;
            for pf in &mut self.port_faults {
                if pf.instance != from_instance || pf.port != message.port {
                    continue;
                }
                if emission_tai_ns < pf.start_tai_ns || pf.end_tai_ns.is_some_and(|end| emission_tai_ns >= end) {
                    continue;
                }
                let applies = pf.rng.bernoulli(pf.rate);
                if !applies {
                    continue;
                }
                if pf.first_applied_tai_ns.is_none() {
                    pf.first_applied_tai_ns = Some(emission_tai_ns);
                }
                pf.frames_affected += 1;
                let this_effect = match pf.kind {
                    PortFaultKind::Drop => PortFaultEffect::Drop,
                    PortFaultKind::Delay => PortFaultEffect::Delay(pf.delay_ns),
                    PortFaultKind::Corrupt => PortFaultEffect::Corrupt(corrupt_payload(&message.payload, pf.corrupt_mask, &mut pf.rng)),
                    PortFaultKind::Duplicate => PortFaultEffect::Duplicate(pf.duplicate_offset_ns),
                };
                debug_assert!(
                    effect.is_none(),
                    "install_port_faults refuses overlapping windows on the same (instance, port) at load, so at most one installed fault can ever match one candidate frame"
                );
                effect = Some(this_effect);
            }

            if matches!(effect, Some(PortFaultEffect::Drop)) {
                // The OUT record above already recorded the emitter's own genuine emission; a
                // "drop" fault suppresses everything past that point -- no IN record, no
                // delivery -- the identical "a message on a port with no connection is dropped,
                // not an error" recording rule this module already applies, restated for a fault
                // instead of missing wiring (module doc comment, "What the log records").
                continue;
            }

            let Some(targets) = self.edges.get(&(from_instance.to_string(), message.port.clone())) else {
                continue;
            };
            // What the receiver actually sees: original bytes unless "corrupt" applied (module
            // doc comment, "Corrupt" -- the OUT record above always keeps the original,
            // regardless); zero extra delay unless "delay" applied; a second, later delivery
            // (never a second OUT record) only when "duplicate" applied (module doc comment,
            // "Duplicate").
            let (delivery_payload, extra_delay_ns, duplicate_offset_ns): (Vec<u8>, i64, Option<i64>) = match effect {
                Some(PortFaultEffect::Corrupt(corrupted)) => (corrupted, 0, None),
                Some(PortFaultEffect::Delay(extra)) => (message.payload.clone(), extra, None),
                Some(PortFaultEffect::Duplicate(offset)) => (message.payload.clone(), 0, Some(offset)),
                Some(PortFaultEffect::Drop) => unreachable!("handled above"),
                None => (message.payload.clone(), 0, None),
            };
            for edge in targets {
                if recordable {
                    self.port_traffic.push(PortTrafficRecord {
                        instance: edge.to_instance.clone(),
                        port: edge.to_port.clone(),
                        direction: PortDirection::In as i32,
                        tai_ns: emission_tai_ns,
                        payload: delivery_payload.clone(),
                        sequence: self.step,
                    });
                }
                // A "delay" fault's own extra_delay_ns is ADDED to this connection's own already-
                // declared latency, for every edge alike (a port-level fault, not a per-
                // connection one) -- question 110's existing delivery rule applies unchanged from
                // there (module doc comment, "What the log records").
                let delivered = av_cdm::pb::PortMessage { port: edge.to_port.clone(), tai_ns: emission_tai_ns + edge.latency_ns + extra_delay_ns, payload: delivery_payload.clone() };
                self.pending.entry(edge.to_instance.clone()).or_default().push(QueuedMessage {
                    message: delivered,
                    sender_emission_tai_ns: emission_tai_ns,
                    sender_instance: from_instance.to_string(),
                });

                // "duplicate" (R4.1b): a second IN record and a second delivery, one native
                // output period later -- never a second OUT record (module doc comment,
                // "Duplicate": there is only one real emission).
                if let Some(offset_ns) = duplicate_offset_ns {
                    if recordable {
                        self.port_traffic.push(PortTrafficRecord {
                            instance: edge.to_instance.clone(),
                            port: edge.to_port.clone(),
                            direction: PortDirection::In as i32,
                            tai_ns: emission_tai_ns,
                            payload: delivery_payload.clone(),
                            sequence: self.step,
                        });
                    }
                    let delivered_again = av_cdm::pb::PortMessage { port: edge.to_port.clone(), tai_ns: emission_tai_ns + edge.latency_ns + offset_ns, payload: delivery_payload.clone() };
                    self.pending.entry(edge.to_instance.clone()).or_default().push(QueuedMessage {
                        message: delivered_again,
                        sender_emission_tai_ns: emission_tai_ns,
                        sender_instance: from_instance.to_string(),
                    });
                }
            }
        }
    }

    /// Advance this Router's own port-traffic step counter by one (question 175, M25.4a) --
    /// called exactly once per output tick, by [`crate::kernel::HeteroKernel::run_with_ports`]'s
    /// own loop, immediately before it calls [`crate::schedule::HeteroScheduler::
    /// advance_to_with_ports`] -- that loop is this codebase's own definition of a "step". The
    /// counter starts at 0 (`Router::build`/`Router::empty`), so the very first call makes it
    /// 1: the first output tick of a run is `PortTrafficRecord.sequence` 1, never 0. **Never
    /// reset**: `crate::drm::executor::run_shared_group` threads one `Router` across every
    /// fault/maneuver-bounded span of a run (`run_one_span` builds a fresh `HeteroKernel` per
    /// span but is handed the identical `&mut Router` every time -- verified by reading both
    /// functions, not assumed), so this counter stays monotonic across the whole run, not just
    /// within one span.
    ///
    /// **Not every [`Router::deliver`] call happens inside a step this counter has counted.**
    /// `run_shared_group`'s own command-dispatch pass (`docs/sil-plan.md`'s M25 milestone) calls
    /// `Router::deliver` directly, once per declared `command` `Scenario.event`, before this
    /// method is ever called for the run at all -- so a dispatched command's own OUT/IN
    /// `PortTrafficRecord`s carry `sequence = 0`, distinguishably earlier than any real output
    /// tick's own sequence (which is always >= 1). This was measured, not designed around --
    /// see `tests/port_traffic_sidecar.rs`'s own module doc comment for the concrete case
    /// (`demo_command`'s own dispatched telecommand).
    pub fn begin_step(&mut self) {
        self.step += 1;
    }

    /// Drain and return every [`PortTrafficRecord`] this Router has recorded so far (question
    /// 175, M25.4a) -- in this Router's own emission order (unsorted: `crate::drm::executor::
    /// execute` imposes `PortTrafficLog.records`'s required `(tai_ns, sequence, instance, port)`
    /// order itself -- epoch first, question 181 -- with a stable sort, so this call's own order
    /// survives as that sort's tie-break).
    /// `execute()` calls this exactly once, at the same place it reads [`Router::pending_count`]
    /// (after every span of the run has finished) -- so the returned `Vec` is the whole run's
    /// own port traffic, never a partial slice a caller has to remember to merge.
    pub fn take_port_traffic(&mut self) -> Vec<PortTrafficRecord> {
        std::mem::take(&mut self.port_traffic)
    }

    /// How many emissions this Router saw on a port with no declared `PortKind` and therefore
    /// recorded nothing for (question 175, M25.4a) -- see [`Router::deliver`]'s own doc comment
    /// for the real case this counts (`crate::drm::sensors::TRUTH_PORT_NAMES`, broadcast every
    /// step whether or not the emitting instance declares those ports) and why skipping them
    /// loses nothing a replay could have used. Not drained by [`Router::take_port_traffic`]:
    /// this is a run-total, read once by `crate::drm::executor::execute` alongside it.
    pub fn undeclared_port_emissions(&self) -> u64 {
        self.undeclared_port_emissions
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
        // The emitted port ("unwired") must still be a *declared* port of the sender's own
        // SystemDefinition -- an entirely undeclared port is a different case (question 175's
        // own "unknown (instance, port) pair", which `Router::deliver` flags with a
        // `debug_assert!` -- see that method's own doc comment and `an_emission_on_an_
        // undeclared_port_trips_the_debug_assertion`, below). This test's own point is
        // narrower: a real, declared port with no `Connection` naming it at all.
        let sender = sys("sender_sys", vec![port("out", PortKind::Signal, PortDirection::Out), port("unwired", PortKind::Signal, PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![port("in", PortKind::Signal, PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", "")],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let mut outbox = Outbox::new();
        outbox.push_signal("unwired", 1_000, 1.0);
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

    // ------------------------------------------------------------------------------------
    // Port traffic recording (question 175, M25.4a) -- see the module doc comment's "Port
    // traffic recording" section and `Router::deliver`/`Router::begin_step`'s own doc comments.
    // `tests/port_traffic_sidecar.rs` covers the whole `execute()`-level sidecar (file, hash,
    // provenance); these are the Router-level unit tests for the recording mechanism itself.
    // ------------------------------------------------------------------------------------

    fn framed_port(name: &str, direction: PortDirection) -> Port {
        Port { name: name.to_string(), kind: PortKind::Framed as i32, direction: direction as i32, ..Default::default() }
    }

    fn two_framed_instances() -> (SosConfiguration, BTreeMap<String, SystemDefinition>) {
        let sender = sys("sender_sys", vec![framed_port("out", PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![framed_port("in", PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", "")],
            ..Default::default()
        };
        (sos, systems_map(vec![sender, receiver]))
    }

    /// [`Router::begin_step`]'s own documented contract: the counter starts at 0
    /// (`Router::build`), so the very first call makes it 1 -- never 0 -- and every later call
    /// keeps incrementing by exactly one, monotonically. Would catch an off-by-one
    /// implementation (e.g. one that returns the post-increment value starting at 0, or one
    /// that only increments every other call).
    #[test]
    fn begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, vec![1, 2, 3]);
        router.deliver("sender", 1_000, outbox);
        let recorded = router.take_port_traffic();
        assert_eq!(recorded.len(), 2, "one OUT + one IN record");
        assert!(recorded.iter().all(|r| r.sequence == 1), "the first begin_step() call must produce sequence 1, not 0: {recorded:?}");

        router.begin_step();
        router.begin_step();
        let mut outbox2 = Outbox::new();
        outbox2.push("out", 2_000, vec![4]);
        router.deliver("sender", 2_000, outbox2);
        let recorded2 = router.take_port_traffic();
        assert!(recorded2.iter().all(|r| r.sequence == 3), "two more begin_step() calls: sequence must be 3, not 2 or 4: {recorded2:?}");
    }

    /// A FRAMED connection's own [`Router::deliver`] call records exactly one OUT record (the
    /// sender's own emission) and one IN record per connected receiver edge, both carrying the
    /// *emission* epoch (not the receiver's later arrival epoch) and the same `sequence`. Fails
    /// against a wrong implementation that records only the OUT side, records the arrival
    /// epoch instead of the emission epoch on the IN record, or gives the two records
    /// different `sequence` values.
    #[test]
    fn deliver_records_out_and_in_port_traffic_for_a_framed_connection_sharing_one_sequence() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push("out", 5_000, vec![9, 9]);
        router.deliver("sender", 5_000, outbox);

        let recorded = router.take_port_traffic();
        assert_eq!(recorded.len(), 2, "{recorded:?}");
        let out_rec = recorded.iter().find(|r| r.direction == PortDirection::Out as i32).expect("an OUT record");
        let in_rec = recorded.iter().find(|r| r.direction == PortDirection::In as i32).expect("an IN record");
        assert_eq!(out_rec.instance, "sender");
        assert_eq!(out_rec.port, "out");
        assert_eq!(out_rec.tai_ns, 5_000);
        assert_eq!(out_rec.payload, vec![9, 9]);
        assert_eq!(in_rec.instance, "receiver");
        assert_eq!(in_rec.port, "in");
        assert_eq!(in_rec.tai_ns, 5_000, "the IN record's tai_ns is the EMISSION epoch, not emission + latency");
        assert_eq!(in_rec.payload, vec![9, 9]);
        assert_eq!(out_rec.sequence, 1);
        assert_eq!(in_rec.sequence, 1, "both records from one deliver() call share one sequence");
    }

    /// A FRAMED/BYTE_STREAM message on a port with no matching `Connection` still gets its own
    /// OUT record -- no IN record, since nothing is wired to receive it (mirrors this module's
    /// own "a message on a port with no connection is dropped, not an error" delivery rule).
    #[test]
    fn a_framed_port_with_no_connection_still_gets_an_out_record_and_no_in_record() {
        let sender = sys("sender_sys", vec![framed_port("out", PortDirection::Out), framed_port("unwired", PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![framed_port("in", PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "out", "receiver", "in", "")],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push("unwired", 1_000, vec![7]);
        router.deliver("sender", 1_000, outbox);

        let recorded = router.take_port_traffic();
        assert_eq!(recorded.len(), 1, "an OUT record only, no IN: {recorded:?}");
        assert_eq!(recorded[0].direction, PortDirection::Out as i32);
        assert_eq!(recorded[0].instance, "sender");
        assert_eq!(recorded[0].port, "unwired");
    }

    /// SIGNAL and CDM ports are never recorded, connected or not -- question 175's own scope is
    /// FRAMED/BYTE_STREAM only.
    #[test]
    fn signal_and_cdm_ports_are_never_recorded() {
        let sender = sys("sender_sys", vec![port("sig_out", PortKind::Signal, PortDirection::Out), port("cdm_out", PortKind::Cdm, PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![port("sig_in", PortKind::Signal, PortDirection::In), port("cdm_in", PortKind::Cdm, PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "sig_out", "receiver", "sig_in", ""), conn("sender", "cdm_out", "receiver", "cdm_in", "")],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);
        let mut router = Router::build(&sos, &systems).expect("valid connections");
        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push_signal("sig_out", 1_000, 1.0);
        outbox.push("cdm_out", 1_000, vec![1]);
        router.deliver("sender", 1_000, outbox);

        assert!(router.take_port_traffic().is_empty(), "SIGNAL/CDM ports must never be recorded");
        // Delivery itself is unaffected by recording -- both messages still actually queue.
        assert_eq!(router.take_inbox("receiver", 1_000).messages().len(), 2);
    }

    /// [`Router::take_port_traffic`] drains what has been recorded and leaves nothing behind
    /// for a second call -- the identical "destructive drain" contract [`Router::take_inbox`]
    /// already has. Would catch an implementation that clones rather than drains, or that
    /// never actually clears its own accumulator.
    #[test]
    fn take_port_traffic_drains_and_does_not_repeat_on_a_second_call() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, vec![1]);
        router.deliver("sender", 1_000, outbox);
        assert_eq!(router.take_port_traffic().len(), 2);
        assert!(router.take_port_traffic().is_empty(), "a second call with nothing new recorded must be empty, not a repeat of the first");
    }

    /// Question 175's own "an unknown (instance, port) pair must not silently record or silently
    /// skip" requirement, as it actually resolved (see [`Router::deliver`]'s own doc comment):
    /// an emission on a port with no declared `PortKind` records nothing, does not panic, still
    /// delivers nothing (an undeclared port can have no `edges` -- `Router::build` refuses any
    /// `Connection` naming one), and is COUNTED, so the skip is never silent.
    ///
    /// This replaces M25.4a's own first attempt, a `debug_assert!` that a port must always be
    /// declared. That assertion was wrong and five `tests/drm_attitude.rs` tests proved it:
    /// `crate::drm::sensors::TruthBroadcastAttitude` broadcasts on the seven
    /// `TRUTH_PORT_NAMES` every step whether or not the instance declares them. Fails against
    /// an implementation that records an undeclared port anyway (it cannot know its kind), that
    /// panics on it, or that skips it without counting.
    #[test]
    fn an_emission_on_an_undeclared_port_records_nothing_and_is_counted_not_silent() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        router.begin_step();
        let mut outbox = Outbox::new();
        // Exactly the shape `TruthBroadcastAttitude` produces on an instance whose own
        // SystemDefinition declares no truth ports (drms/demo_attitude_precession).
        outbox.push_signal(crate::drm::sensors::TRUTH_PORT_QX, 1_000, 0.5);
        outbox.push("nobody_declared_this_port", 1_000, vec![1]);
        router.deliver("sender", 1_000, outbox);

        assert!(router.take_port_traffic().is_empty(), "an undeclared port has no PortKind, so nothing can be classified as recordable");
        assert!(!router.has_pending(), "an undeclared port can have no edges, so nothing is ever delivered from it");
        assert_eq!(router.undeclared_port_emissions(), 2, "both undeclared emissions must be counted -- the skip is never silent");
    }

    /// The counter stays at zero for a run whose every emission is on a declared port -- there
    /// is no zero-valued attribute in the sidecar, by design (`Router::deliver`'s own doc
    /// comment), so this is the case that must produce nothing to report.
    #[test]
    fn a_declared_port_emission_never_counts_as_an_undeclared_one() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, vec![1]);
        router.deliver("sender", 1_000, outbox);
        assert_eq!(router.undeclared_port_emissions(), 0);
        assert_eq!(router.take_port_traffic().len(), 2);
    }

    // ------------------------------------------------------------------------------------
    // Port fault runtime (question 178, R4.1a) -- see the module doc comment's "Port fault
    // runtime" section. `crates/av-kernel/tests/port_faults.rs` covers the whole `execute()`-
    // level acceptance path (real DRMs, real events, byte-identical determinism); these are the
    // Router-level unit tests for the mechanism itself.
    // ------------------------------------------------------------------------------------

    fn port_fault(id: &str, instance: &str, port: &str, kind: &str, tai_ns: i64, duration_ns: i64) -> Fault {
        Fault { id: id.to_string(), tai_ns, duration_ns, target_kind: FaultTargetKind::Port as i32, instance: instance.to_string(), target: port.to_string(), kind: kind.to_string(), ..Default::default() }
    }
    fn with_param(mut f: Fault, key: &str, value: f64) -> Fault {
        f.params.insert(key.to_string(), value);
        f
    }
    fn seeds_with(id: &str, seed: u64) -> BTreeMap<String, u64> {
        BTreeMap::from([(id.to_string(), seed)])
    }
    /// A representative 1 Hz output period -- used everywhere below that does not itself test
    /// `"duplicate"`'s own use of it (module doc comment, "Duplicate").
    const TEST_OUTPUT_PERIOD_NS: i64 = 1_000_000_000;

    #[test]
    fn install_port_faults_refuses_an_undeclared_port_target() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f1", "sender", "nonexistent", "drop", 0, 0);
        let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(matches!(err, RouterError::UndeclaredPortFaultTarget { ref fault_id, ref instance, ref port } if fault_id == "f1" && instance == "sender" && port == "nonexistent"), "{err:?}");
    }

    #[test]
    fn install_port_faults_refuses_a_non_framed_port_target() {
        let (sos, systems) = two_signal_instances(PortDirection::Out, PortDirection::In);
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f1", "sender", "out", "drop", 0, 0);
        let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(matches!(err, RouterError::PortFaultTargetNotFramed { ref fault_id, ref instance, ref port, .. } if fault_id == "f1" && instance == "sender" && port == "out"), "{err:?}");
    }

    #[test]
    fn install_port_faults_refuses_an_unrecognized_kind() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f1", "sender", "out", "not_a_real_kind", 0, 0);
        let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(matches!(err, RouterError::UnsupportedPortFaultKind { ref fault_id, ref kind } if fault_id == "f1" && kind == "not_a_real_kind"), "{err:?}");
    }

    /// R4.1b: `"corrupt"`/`"duplicate"` are now real, implemented PORT fault kinds -- through
    /// R4.1a both were refused at the Router level with the identical generic
    /// `UnsupportedPortFaultKind` any other unimplemented kind got
    /// (`install_port_faults_refuses_corrupt_and_duplicate_the_same_generic_way_at_the_router_
    /// level`, this test's own prior name/shape -- see `R4_1B_REPORT.md`). Converted, not
    /// deleted: this now pins the opposite fact -- both kinds install cleanly, with no error at
    /// all, the same way `"drop"`/`"delay"` already did before R4.1b. Their own real per-kind
    /// EFFECTS are pinned by dedicated tests further down (`a_corrupt_fault_...`/
    /// `a_duplicate_fault_...`), not here -- this test's own job is only "the kind itself is no
    /// longer refused."
    #[test]
    fn install_port_faults_accepts_corrupt_and_duplicate_as_real_kinds_r4_1b() {
        let (sos, systems) = two_framed_instances();
        for kind in ["corrupt", "duplicate"] {
            let mut router = Router::build(&sos, &systems).expect("valid connection");
            let f = port_fault("f1", "sender", "out", kind, 0, 0);
            router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_or_else(|e| panic!("kind {kind:?} must install cleanly now: {e:?}"));
        }
    }

    /// R4.1b: `kind == "corrupt"` declared `params["corrupt_mask"]` outside `[0, 255]`, or a
    /// non-integer value, is a typed refusal -- `Fault.params` is `map<string, double>`, so this
    /// is the runtime's own "is this actually a valid byte" check (module doc comment,
    /// "Corrupt").
    #[test]
    fn install_port_faults_refuses_a_non_byte_corrupt_mask() {
        let (sos, systems) = two_framed_instances();
        for bad_mask in [-1.0, 256.0, 3.5] {
            let mut router = Router::build(&sos, &systems).expect("valid connection");
            let f = with_param(port_fault("f1", "sender", "out", "corrupt", 0, 0), "corrupt_mask", bad_mask);
            let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
            assert!(matches!(err, RouterError::InvalidPortFaultCorruptMask { ref fault_id, mask } if fault_id == "f1" && mask == bad_mask), "mask {bad_mask}: {err:?}");
        }
    }

    #[test]
    fn install_port_faults_refuses_a_missing_seed() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f_unseeded", "sender", "out", "drop", 0, 0);
        let err = router.install_port_faults(&[f], &BTreeMap::new(), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(matches!(err, RouterError::MissingFaultSeed { ref fault_id } if fault_id == "f_unseeded"), "{err:?}");
    }

    /// A `"delay"` fault requires `params["delay_s"]`, even at the default `rate == 1.0` -- never
    /// defaulted silently.
    #[test]
    fn install_port_faults_refuses_a_delay_fault_missing_delay_s() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f1", "sender", "out", "delay", 0, 0);
        let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(matches!(err, RouterError::MissingPortFaultDelay { ref fault_id } if fault_id == "f1"), "{err:?}");
    }

    #[test]
    fn install_port_faults_refuses_a_rate_outside_zero_one() {
        let (sos, systems) = two_framed_instances();
        for bad_rate in [-0.1, 1.1, 2.0] {
            let mut router = Router::build(&sos, &systems).expect("valid connection");
            let f = with_param(port_fault("f1", "sender", "out", "drop", 0, 0), "rate", bad_rate);
            let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
            assert!(matches!(err, RouterError::InvalidPortFaultRate { ref fault_id, rate } if fault_id == "f1" && rate == bad_rate), "rate {bad_rate}: {err:?}");
        }
    }

    /// `Fault.clear == true` on a PORT fault is refused, not silently ignored -- see the module
    /// doc comment's "Window" section for why this design does not honour `clear`.
    #[test]
    fn install_port_faults_refuses_clear_true() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let mut f = port_fault("f1", "sender", "out", "drop", 0, 0);
        f.clear = true;
        let err = router.install_port_faults(&[f], &seeds_with("f1", 1), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(matches!(err, RouterError::PortFaultClearNotSupported { ref fault_id } if fault_id == "f1"), "{err:?}");
    }

    /// A rate-1.0 "drop" fault whose window covers the emission suppresses the IN record AND
    /// delivery but KEEPS the OUT record -- the module doc comment's "What the log records"
    /// section, pinned directly. An emission OUTSIDE the fault's own window is unaffected: both
    /// records appear and the message is delivered, proving the window bound is real, not merely
    /// "the fault always applies once installed."
    #[test]
    fn a_drop_fault_suppresses_the_in_record_and_delivery_but_keeps_the_out_record() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f_drop", "sender", "out", "drop", 1_000, 1_000); // window [1000, 2000)
        router.install_port_faults(&[f], &seeds_with("f_drop", 42), TEST_OUTPUT_PERIOD_NS).expect("valid fault");

        router.begin_step();
        let mut inside = Outbox::new();
        inside.push("out", 1_000, vec![9]);
        router.deliver("sender", 1_000, inside);

        let recorded = router.take_port_traffic();
        assert_eq!(recorded.len(), 1, "OUT only, no IN: {recorded:?}");
        assert_eq!(recorded[0].direction, PortDirection::Out as i32);
        assert_eq!(recorded[0].instance, "sender");
        assert!(router.take_inbox("receiver", 1_000).is_empty(), "delivery must be suppressed");
        assert!(!router.has_pending());

        // Outside the fault's own window (>= 2_000): unaffected, both records, real delivery.
        router.begin_step();
        let mut outside = Outbox::new();
        outside.push("out", 5_000, vec![9]);
        router.deliver("sender", 5_000, outside);
        let recorded2 = router.take_port_traffic();
        assert_eq!(recorded2.len(), 2, "OUT + IN outside the fault's own window: {recorded2:?}");
        assert_eq!(router.take_inbox("receiver", 5_000).messages().len(), 1, "delivered once the fault's window has passed");

        // Applied exactly once, at the first (only) frame it actually dropped -- frames_affected
        // (question 186(c), R4.1b) must count that same one frame, not the two candidate
        // emissions this test made in total (the second, at 5_000, is outside the window and
        // never even drew).
        let applied = router.take_applied_port_faults();
        assert_eq!(applied, vec![AppliedPortFault { fault_id: "f_drop".to_string(), instance: "sender".to_string(), port: "out".to_string(), applied_tai_ns: 1_000, frames_affected: 1 }]);
        assert!(router.take_applied_port_faults().is_empty(), "a second drain with nothing newly applied must be empty, not a repeat");
    }

    /// A "delay" fault's own `params["delay_s"]` is ADDED to the connection's own already-
    /// declared latency (here zero, no `link_model`) -- pinned the same "held until, and
    /// delivered exactly at, availability" way `take_inbox_delivers_on_the_boundary_epoch_
    /// equal_to_availability_not_only_strictly_after` already pins the base latency case.
    /// `duration_ns == 0` (persistent) is exercised here too: a message far past any "reasonable"
    /// window still gets the fault.
    #[test]
    fn a_delay_fault_adds_its_own_delay_to_the_connections_declared_latency() {
        let (sos, systems) = two_framed_instances(); // zero declared connection latency
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = with_param(port_fault("f_delay", "sender", "out", "delay", 0, 0 /* persistent */), "delay_s", 0.05); // 50 ms
        router.install_port_faults(&[f], &seeds_with("f_delay", 7), TEST_OUTPUT_PERIOD_NS).expect("valid fault");

        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, vec![1]);
        router.deliver("sender", 1_000, outbox);
        let available_at = 1_000 + 50_000_000;
        assert!(router.take_inbox("receiver", available_at - 1).is_empty(), "not yet available: held");
        let inbox = router.take_inbox("receiver", available_at);
        assert_eq!(inbox.messages().len(), 1, "delivered exactly when as_of reaches emission + connection latency (0) + fault delay (50ms)");

        // Persistent (duration_ns == 0): a message far in the future still gets the fault.
        let far_future = 1_000_000_000_000;
        let mut outbox2 = Outbox::new();
        outbox2.push("out", far_future, vec![2]);
        router.deliver("sender", far_future, outbox2);
        assert!(router.take_inbox("receiver", far_future + 49_999_999).is_empty(), "persistent fault still applies far in the future");
        assert_eq!(router.take_inbox("receiver", far_future + 50_000_000).messages().len(), 1);
    }

    /// **R4.1b (`docs/open-questions.md` questions 184/186(b)): this test's own prior shape --
    /// two DELAY faults installed on the SAME port, both persistent (`duration_ns == 0`), so
    /// their windows trivially overlap -- is now a REFUSAL, not a "delays sum" success case. See
    /// `R4_1B_REPORT.md` for why: the manager's ruling decided overlapping windows on one port
    /// are refused at load, which retires R4.1a's own "delays from every applying fault sum"
    /// path as unreachable (deleted, not left as dead code -- module doc comment, "Overlapping
    /// windows are refused at load"). Both fault ids and the overlapping interval must be named.
    #[test]
    fn two_delay_faults_on_the_same_port_with_overlapping_persistent_windows_are_refused_at_load() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f1 = with_param(port_fault("f1", "sender", "out", "delay", 0, 0), "delay_s", 0.01);
        let f2 = with_param(port_fault("f2", "sender", "out", "delay", 0, 0), "delay_s", 0.02);
        let err = router.install_port_faults(&[f1, f2], &BTreeMap::from([("f1".to_string(), 1u64), ("f2".to_string(), 2u64)]), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(
            matches!(&err, RouterError::OverlappingPortFaultWindows { fault_a, fault_b, instance, port, overlap_start_tai_ns: 0, overlap_end_tai_ns: None }
                if fault_a == "f1" && fault_b == "f2" && instance == "sender" && port == "out"),
            "{err:?}"
        );
    }

    /// The other half of the same ruling: two faults on the identical port with GENUINELY
    /// DISJOINT windows (one ends exactly where the other begins -- half-open, so touching is not
    /// overlapping) stay legal, and each still applies independently within its own window.
    #[test]
    fn two_delay_faults_on_the_same_port_with_disjoint_windows_are_both_legal_and_independent() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        // f1: [0, 1_000); f2: [1_000, 2_000) -- touching, not overlapping.
        let f1 = with_param(port_fault("f1", "sender", "out", "delay", 0, 1_000), "delay_s", 0.01);
        let f2 = with_param(port_fault("f2", "sender", "out", "delay", 1_000, 1_000), "delay_s", 0.02);
        router.install_port_faults(&[f1, f2], &BTreeMap::from([("f1".to_string(), 1u64), ("f2".to_string(), 2u64)]), TEST_OUTPUT_PERIOD_NS).expect("disjoint windows must stay legal");

        let mut outbox_a = Outbox::new();
        outbox_a.push("out", 500, vec![1]);
        router.deliver("sender", 500, outbox_a);
        assert!(router.take_inbox("receiver", 500 + 10_000_000 - 1).is_empty());
        assert_eq!(router.take_inbox("receiver", 500 + 10_000_000).messages().len(), 1, "f1's own 10ms delay applies inside its own window");

        let mut outbox_b = Outbox::new();
        outbox_b.push("out", 1_500, vec![2]);
        router.deliver("sender", 1_500, outbox_b);
        assert!(router.take_inbox("receiver", 1_500 + 20_000_000 - 1).is_empty());
        assert_eq!(router.take_inbox("receiver", 1_500 + 20_000_000).messages().len(), 1, "f2's own 20ms delay applies inside its own, disjoint window");
    }

    /// The core determinism/independence claim (question 178, rule 6, "question 137-style
    /// substreams"): two PORT faults on two DIFFERENT ports each draw from their OWN seeded
    /// `Pcg64` stream, unconditionally, once per candidate frame -- reconstructed by hand here
    /// (a fresh `Pcg64::new(seed)`, the same `bernoulli(rate)` calls in the same order) and
    /// compared, exactly, against what `Router::deliver` actually decided. Run TWICE, with fault
    /// B's own seed changed between runs: fault A's own reconstructed-and-matched sequence is
    /// identical both times, proving its outcomes depend on nothing about fault B (not its seed,
    /// not its presence) -- the exact property "changing one fault's seed changes only that
    /// fault's own outcomes" requires. Fails against an implementation that shares one Pcg64
    /// across every installed fault (fault A's sequence would shift when fault B's seed changes),
    /// or one that skips a fault's own draw once another fault on the same candidate frame has
    /// already decided its fate (rate would then depend on installation order).
    #[test]
    fn two_port_faults_on_different_ports_draw_from_independent_seeded_substreams() {
        let sender = sys("sender_sys", vec![framed_port("p1", PortDirection::Out), framed_port("p2", PortDirection::Out)]);
        let receiver = sys("receiver_sys", vec![framed_port("p1", PortDirection::In), framed_port("p2", PortDirection::In)]);
        let sos = SosConfiguration {
            instances: vec![instance("sender", "sender_sys"), instance("receiver", "receiver_sys")],
            connections: vec![conn("sender", "p1", "receiver", "p1", ""), conn("sender", "p2", "receiver", "p2", "")],
            ..Default::default()
        };
        let systems = systems_map(vec![sender, receiver]);

        const SEED_A: u64 = 12345;
        const N: i64 = 25;

        fn run_with(systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration, seed_b: u64) -> (Vec<bool>, Vec<bool>) {
            let mut router = Router::build(sos, systems).expect("valid connections");
            let fa = with_param(port_fault("fA", "sender", "p1", "drop", 0, 0), "rate", 0.5);
            let fb = with_param(port_fault("fB", "sender", "p2", "drop", 0, 0), "rate", 0.5);
            router.install_port_faults(&[fa, fb], &BTreeMap::from([("fA".to_string(), SEED_A), ("fB".to_string(), seed_b)]), TEST_OUTPUT_PERIOD_NS).expect("valid faults");

            let mut a_dropped = Vec::new();
            let mut b_dropped = Vec::new();
            for k in 0..N {
                let t = k * 1_000_000;
                let mut oa = Outbox::new();
                oa.push("p1", t, vec![1]);
                router.deliver("sender", t, oa);
                a_dropped.push(router.take_inbox("receiver", t).is_empty());

                let mut ob = Outbox::new();
                ob.push("p2", t, vec![2]);
                router.deliver("sender", t, ob);
                b_dropped.push(router.take_inbox("receiver", t).is_empty());
            }
            (a_dropped, b_dropped)
        }

        fn reconstruct(seed: u64) -> Vec<bool> {
            let mut rng = Pcg64::new(seed);
            (0..N).map(|_| rng.bernoulli(0.5)).collect()
        }

        let (a1, b1) = run_with(&systems, &sos, 999);
        let (a2, b2) = run_with(&systems, &sos, 111_111);

        let expected_a = reconstruct(SEED_A);
        assert_eq!(a1, expected_a, "fault A's own outcome sequence must match its own seeded stream exactly");
        assert_eq!(a2, expected_a, "fault A's outcomes must be identical regardless of fault B's own seed -- independent substreams");
        assert_eq!(b1, reconstruct(999), "fault B's own outcome sequence (seed 999) must match its own seeded stream exactly");
        assert_eq!(b2, reconstruct(111_111), "fault B's own outcome sequence (seed 111_111) must match its own seeded stream exactly");
    }

    // ------------------------------------------------------------------------------------
    // Corrupt / duplicate (R4.1b, question 178) -- see the module doc comment's "Port fault
    // runtime" section's own "Corrupt"/"Duplicate" subsections. `crates/av-kernel/tests/
    // port_faults.rs` covers the whole `execute()`-level acceptance path.
    // ------------------------------------------------------------------------------------

    /// A declared `corrupt_mask` is XORed into EVERY byte of the payload the RECEIVER sees; the
    /// OUT record (recorded before the fault runtime is even consulted) keeps the emitter's own
    /// original, unmutated bytes -- module doc comment, "Corrupt". Predicted, before running:
    /// `[0x01, 0x02, 0x03] XOR 0xFF = [0xFE, 0xFD, 0xFC]`.
    #[test]
    fn a_declared_corrupt_mask_is_xored_into_every_byte_on_the_in_side_only() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = with_param(port_fault("f_corrupt", "sender", "out", "corrupt", 0, 0), "corrupt_mask", 0xFF as f64);
        router.install_port_faults(&[f], &seeds_with("f_corrupt", 1), TEST_OUTPUT_PERIOD_NS).expect("valid fault");

        router.begin_step();
        let original = vec![0x01u8, 0x02, 0x03];
        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, original.clone());
        router.deliver("sender", 1_000, outbox);

        let recorded = router.take_port_traffic();
        let out_rec = recorded.iter().find(|r| r.direction == PortDirection::Out as i32).expect("an OUT record");
        assert_eq!(out_rec.payload, original, "the OUT record must keep the emitter's own original bytes, never the corrupted ones");
        let in_rec = recorded.iter().find(|r| r.direction == PortDirection::In as i32).expect("an IN record");
        assert_eq!(in_rec.payload, vec![0xFE, 0xFD, 0xFC], "declared corrupt_mask=0xFF XORed into every byte");

        let inbox = router.take_inbox("receiver", 1_000);
        assert_eq!(inbox.messages()[0].payload, vec![0xFE, 0xFD, 0xFC], "the RECEIVER must actually see the mutated bytes, not merely the log entry");
    }

    /// No declared `corrupt_mask`: exactly one bit, at a position drawn from the fault's own
    /// seeded stream (the SAME stream `bernoulli(rate)` already drew from for this candidate
    /// frame), is flipped. Reconstructed by hand -- a fresh `Pcg64::new(seed)`, one `bernoulli`
    /// draw (the "applies" check, `rate` defaults to 1.0) then one `next_u64() % (payload.len() *
    /// 8)` draw for the bit position -- and compared exactly against what `Router::deliver`
    /// actually produced, the same reconstruction method `two_port_faults_on_different_ports_
    /// draw_from_independent_seeded_substreams` already uses. A 4-byte all-zero payload makes
    /// "exactly one bit differs" trivial to check by popcount.
    #[test]
    fn a_corrupt_fault_with_no_declared_mask_flips_exactly_one_bit_from_its_own_seeded_stream() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f_corrupt", "sender", "out", "corrupt", 0, 0);
        router.install_port_faults(&[f], &seeds_with("f_corrupt", 99), TEST_OUTPUT_PERIOD_NS).expect("valid fault");

        let original = vec![0u8; 4];
        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, original.clone());
        router.deliver("sender", 1_000, outbox);
        let corrupted = router.take_inbox("receiver", 1_000).messages()[0].payload.clone();

        let mut rng = Pcg64::new(99);
        assert!(rng.bernoulli(1.0), "sanity: default rate is 1.0, always applies");
        let bit_pos = rng.next_u64() % (original.len() as u64 * 8);
        let mut expected = original.clone();
        expected[(bit_pos / 8) as usize] ^= 1u8 << (bit_pos % 8);
        assert_eq!(corrupted, expected, "must match the fault's own reconstructed seeded stream exactly");

        let diff_bits: u32 = corrupted.iter().zip(&original).map(|(a, b)| (a ^ b).count_ones()).sum();
        assert_eq!(diff_bits, 1, "exactly one bit must differ from the original: {corrupted:?} vs {original:?}");
    }

    /// A "duplicate" fault: the receiver sees the frame TWICE, the second copy exactly one
    /// `output_period_ns` later than the first's own normal availability. The IN side gets its
    /// own second `PortTrafficRecord` (both share the one real `tai_ns`, module doc comment); the
    /// OUT side does NOT (module doc comment, "Duplicate": there is only ever one real emission).
    /// Predicted record counts, stated before running: 1 OUT + 2 IN = 3 total.
    #[test]
    fn a_duplicate_fault_delivers_a_second_copy_one_output_period_later_with_its_own_in_record_but_no_second_out_record() {
        let (sos, systems) = two_framed_instances(); // zero declared connection latency
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f_dup", "sender", "out", "duplicate", 0, 0);
        let output_period_ns = 1_000_000_000;
        router.install_port_faults(&[f], &seeds_with("f_dup", 5), output_period_ns).expect("valid fault");

        router.begin_step();
        let mut outbox = Outbox::new();
        outbox.push("out", 1_000, vec![7]);
        router.deliver("sender", 1_000, outbox);

        let recorded = router.take_port_traffic();
        assert_eq!(recorded.len(), 3, "1 OUT + 2 IN, stated before running: {recorded:?}");
        assert_eq!(recorded.iter().filter(|r| r.direction == PortDirection::Out as i32).count(), 1, "no second OUT record for a duplicate: {recorded:?}");
        let in_recs: Vec<_> = recorded.iter().filter(|r| r.direction == PortDirection::In as i32).collect();
        assert_eq!(in_recs.len(), 2, "the duplicate gets its own IN record: {recorded:?}");
        assert!(in_recs.iter().all(|r| r.tai_ns == 1_000), "both IN records share the one real emission epoch, never the fault's own added offset: {in_recs:?}");

        assert!(router.take_inbox("receiver", 999).is_empty(), "not yet available");
        assert_eq!(router.take_inbox("receiver", 1_000).messages().len(), 1, "first copy delivered at normal availability");
        assert!(router.take_inbox("receiver", 1_000 + output_period_ns - 1).is_empty(), "second copy not yet available");
        let second = router.take_inbox("receiver", 1_000 + output_period_ns);
        assert_eq!(second.messages().len(), 1, "second copy delivered exactly one output period later, not zero and not two");
        assert_eq!(second.messages()[0].payload, vec![7], "the duplicate's own payload is the emitter's original bytes, unmutated");
    }

    /// Question 186(c): `AppliedPortFault::frames_affected` counts EVERY frame a fault genuinely
    /// applied to over the whole run, not merely the first -- distinguishing a one-frame drop
    /// from a many-frame one is exactly this task's own required proof (a persistent fault
    /// applying to 5 separate candidate emissions here).
    #[test]
    fn frames_affected_counts_every_applied_frame_not_just_the_first() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f = port_fault("f_drop_many", "sender", "out", "drop", 0, 0); // persistent
        router.install_port_faults(&[f], &seeds_with("f_drop_many", 3), TEST_OUTPUT_PERIOD_NS).expect("valid fault");

        for k in 0..5i64 {
            let mut outbox = Outbox::new();
            outbox.push("out", k * 1_000, vec![1]);
            router.deliver("sender", k * 1_000, outbox);
        }
        let applied = router.take_applied_port_faults();
        assert_eq!(applied.len(), 1, "{applied:?}");
        assert_eq!(applied[0].applied_tai_ns, 0, "the FIRST applied epoch is still the first frame, unchanged by the count");
        assert_eq!(applied[0].frames_affected, 5, "must count every one of the 5 applied frames, not just the first: {applied:?}");
    }

    /// Overlap-refusal (question 184/186(b)) is not limited to two faults of the SAME kind --
    /// any two PORT fault kinds on the identical `(instance, port)` with overlapping windows are
    /// refused identically, since the rule is about the WINDOW, not the effect.
    #[test]
    fn overlapping_windows_are_refused_regardless_of_the_two_faults_own_kinds() {
        let (sos, systems) = two_framed_instances();
        let mut router = Router::build(&sos, &systems).expect("valid connection");
        let f1 = port_fault("f_drop", "sender", "out", "drop", 0, 2_000); // [0, 2000)
        let f2 = port_fault("f_corrupt", "sender", "out", "corrupt", 1_000, 2_000); // [1000, 3000) -- overlaps [1000, 2000)
        let err = router.install_port_faults(&[f1, f2], &BTreeMap::from([("f_drop".to_string(), 1u64), ("f_corrupt".to_string(), 2u64)]), TEST_OUTPUT_PERIOD_NS).unwrap_err();
        assert!(
            matches!(&err, RouterError::OverlappingPortFaultWindows { fault_a, fault_b, overlap_start_tai_ns: 1_000, overlap_end_tai_ns: Some(2_000), .. }
                if fault_a == "f_drop" && fault_b == "f_corrupt"),
            "{err:?}"
        );
    }
}
