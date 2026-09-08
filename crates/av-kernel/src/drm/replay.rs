//! Replay: play a recorded `PortTrafficLog` (question 175/M25.4a) back through a
//! [`ReplayModel`] instead of running whatever process actually produced it (question 175's
//! own follow-on, M25.4b). `crate::drm::executor::RunConfig::replay` is the caller-facing
//! entry point; this module owns the load-time hash check ([`verify_and_load`]) and the
//! playback binding itself ([`ReplayModel`]).
//!
//! ## What is, and is not, replayed
//!
//! Only OUT frames on FRAMED/BYTE_STREAM ports are replayed -- exactly the subset
//! `crate::router::Router::deliver` ever records (that method's own doc comment). **IN records
//! are never consulted.** An IN record is the *receiver's* own view of a frame some OTHER
//! instance emitted (`Router::deliver`'s own doc comment: an IN record shares the identical
//! `tai_ns`/`payload` as the OUT record it mirrors, addressed to the other end) -- if
//! [`ReplayModel`] also played its own IN records back as if they were its own emissions, every
//! frame it ever received would be doubled: once from the real sender that (still, for real)
//! emits it during replay, and once fabricated here from the log. Filtering to `direction ==
//! PORT_DIRECTION_OUT` and `instance == <this instance>` at construction ([`ReplayModel::new`])
//! is what keeps this a replay of what the instance itself said, not of everything it heard.
//!
//! Nothing else about a run is replayed. `StepResult.state`/`.outputs` and CDM `Measurement`s
//! are model-internal computations that were never serialized onto any port in the first
//! place, so a port-traffic log cannot honestly reconstruct them -- [`ReplayModel`] reports a
//! zero-order-hold physical state (never moves: there is no bound process left to compute a
//! real derivative from) and produces no CDM measurement of its own. This is a genuine,
//! disclosed limitation, not an oversight -- see [`ReplayModel::derivatives`]'s and
//! [`ReplayModel::last_measurements`]'s own doc comments.
//!
//! ## The missing-frame rule
//!
//! [`ReplayModel::step_with_ports`] emits exactly the frames recorded at that step's own
//! emission epoch (`t_tai_ns + dt_ns`, matching `crate::schedule::HeteroScheduler::
//! advance_to_with_ports`'s own `Router::deliver` call -- see that function's own doc comment).
//! When nothing was recorded at that exact epoch, the rule is:
//!
//! - **Before this instance's first recorded epoch, or after its last:** legitimate silence --
//!   emits nothing, no error. An instance genuinely has nothing to say before it starts, or
//!   after it stops.
//! - **Strictly between this instance's own first and last recorded epoch, on this instance's
//!   own step grid, with nothing recorded:** [`ReplayError::MissingFrame`] -- a typed error,
//!   never interpolated, held, or silently skipped. Every recorded epoch for one instance is by
//!   construction present as a key in [`ReplayModel::frames_by_epoch`], so "first" and "last"
//!   are themselves always hits, never candidates for this rule; only a genuinely *interior*
//!   epoch with no entry at all can trip it.
//!
//! **Honest limit of this rule (state plainly, per this task's own standing instruction): it
//! detects a deleted or corrupted INTERIOR record, and cannot detect one deleted from the
//! leading or trailing edge.** An instance that genuinely emitted nothing on its own first or
//! last step is, from the log alone, indistinguishable from one whose very first or very last
//! record was quietly removed -- both leave the same "no entry outside [first, last]" shape.
//! Closing that gap would need an independent signal this log does not carry (e.g. the run's
//! own declared step count for that instance, cross-checked against how many *are* recorded) --
//! out of this module's scope; not claimed here.
//!
//! ## Reconstructing the ORIGINAL model's own `ModelInfo` (`describe()`/`state_dim()`)
//!
//! `crate::trajectory::build_trajectory` stamps `TrajectorySegment.dynamics_model`/
//! `.dynamics_hash`/`.dynamics_depth` straight from `ModelInfo.id`/`.settings_hash`/`.depth` --
//! for a replayed run's `Trajectory` to come out byte-identical to the original, [`ReplayModel`]
//! must report the EXACT same three values the replaced model would have. For a
//! `BINDING_KIND_MODEL` instance this is always achievable, because that `ModelInfo` is a pure
//! function of the instance's own DECLARED configuration (no GMAT call, no network) --
//! `crate::registry::ModelRegistry::wrap_replay` constructs the real model exactly as a
//! non-replayed run would, reads its `describe()`/`state_dim()` once, and only THEN discards its
//! real behaviour in favour of a [`ReplayModel`] built from those captured values. A
//! `BINDING_KIND_CONTAINER` instance has no such option (its own `ModelInfo`/`binding_hash` come
//! from a live `Bind` RPC this replay path deliberately never makes -- the entire point of
//! replaying a container instance is running Docker-free); `crate::drm::executor::
//! run_shared_group`'s own "replay" section documents the synthetic `ModelInfo` used there
//! instead, and the resulting, disclosed difference from the original run's own container
//! segment.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{ModelInfo, PortDirection, PortTrafficLog};
use av_dynamics::{AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult};
use prost::Message as _;

use super::hash;
use super::DrmError;

/// `crate::drm::executor::RunConfig::replay`'s own value type -- see that field's doc comment
/// for the full load-time contract.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// Path to the recorded `PortTrafficLog` sidecar (`crate::drm::executor::RunConfig::
    /// products_dir`'s own `port_traffic.pb`, from the run being replayed).
    pub log_path: PathBuf,
    /// The original run's own `RunProducts.port_traffic_hash` -- verified against `log_path`'s
    /// exact bytes before anything else happens (`verify_and_load`).
    pub expected_hash: String,
    /// Which instances to replay. Empty means "every `BINDING_KIND_CONTAINER` instance"
    /// (`crate::drm::executor::execute`'s own resolution of this default); non-empty names
    /// exactly the instances to replay, of ANY binding kind -- naming a `BINDING_KIND_MODEL`
    /// instance here is what makes a Docker-free acceptance test possible at all.
    pub instances: Vec<String>,
}

/// Read `cfg.log_path`'s exact bytes, hash them (`hash::sha256_hex` -- this crate's one SHA-256
/// primitive, no `ring`), and refuse with a typed error if they do not match
/// `cfg.expected_hash` -- BEFORE returning the decoded log to the caller. `crate::drm::executor::
/// execute` calls this as its very first action whenever `RunConfig.replay` is `Some`, before
/// canonical DRM/SOS/system hash verification, before any binding, before any GMAT call, and
/// before any step -- see that function's own doc comment.
///
/// A byte mismatch is [`DrmError::ReplayLogHashMismatch`]; a mismatch that is not even valid
/// `PortTrafficLog` protobuf (or an unreadable path) is [`DrmError::ReplayLogIo`] -- two
/// genuinely different failures (one says "this is not the file the run producer named," the
/// other says "this file could not even be read/parsed"), so they get two variants rather than
/// one folding the second case into the first.
pub(crate) fn verify_and_load(cfg: &ReplayConfig) -> Result<PortTrafficLog, DrmError> {
    let bytes = std::fs::read(&cfg.log_path).map_err(|e| DrmError::ReplayLogIo { path: cfg.log_path.clone(), detail: format!("reading: {e}") })?;
    let computed = hash::sha256_hex(&bytes);
    if computed != cfg.expected_hash {
        return Err(DrmError::ReplayLogHashMismatch { path: cfg.log_path.clone(), expected: cfg.expected_hash.clone(), computed });
    }
    PortTrafficLog::decode(bytes.as_slice()).map_err(|e| DrmError::ReplayLogIo { path: cfg.log_path.clone(), detail: format!("decoding {} verified byte(s) as PortTrafficLog: {e}", bytes.len()) })
}

/// Every way [`ReplayModel::step_with_ports`] can fail -- one shape today. See this module's
/// own doc comment's "The missing-frame rule" section for exactly what does, and does not,
/// trip this.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReplayError {
    MissingFrame { instance: String, tai_ns: i64 },
}
impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::MissingFrame { instance, tai_ns } => write!(
                f,
                "replay: instance {instance:?} has no recorded OUT frame at emission epoch {tai_ns} tai_ns, and that epoch is strictly between its own first and last recorded epochs -- an interior gap (a deleted or corrupted PortTrafficRecord), not a legitimately quiet leading/trailing step"
            ),
        }
    }
}
impl std::error::Error for ReplayError {}

/// A [`DynamicsModel`] that plays one instance's own recorded OUT frames back instead of
/// running whatever process produced them (question 175's follow-on, M25.4b) -- see this
/// module's own doc comment for the full contract.
///
/// `info`/`state_dim` are supplied by the caller, not computed here (see this module's own doc
/// comment's "Reconstructing the ORIGINAL model's own ModelInfo" section for where they come
/// from and why that matters for a byte-identical replayed `Trajectory`).
pub(crate) struct ReplayModel {
    instance: String,
    info: ModelInfo,
    state_dim: usize,
    /// This instance's own recorded OUT frames, keyed by emission `tai_ns`; each value is that
    /// epoch's own `(port, payload)` pairs in the log's own recorded order (`PortTrafficLog.
    /// records`' own required `(sequence, instance, port)` order -- `crate::drm::executor::
    /// sort_port_traffic`'s own doc comment -- restricted to this one instance's own records,
    /// whose own `sequence` is monotonic with its own `tai_ns`, so this map's own ascending key
    /// order recovers this instance's true chronological emission order regardless of the log's
    /// own top-level sort key).
    frames_by_epoch: BTreeMap<i64, Vec<(String, Vec<u8>)>>,
    /// The earliest/latest emission epoch this instance has ANY recorded OUT frame at -- `None`
    /// for an instance that never emitted a FRAMED/BYTE_STREAM frame the whole run. Cached once
    /// at construction (`BTreeMap::first_key_value`/`last_key_value` over
    /// [`ReplayModel::frames_by_epoch`]) rather than recomputed every step. See the module doc
    /// comment's "missing-frame rule" section for exactly how these two bound it.
    first_epoch: Option<i64>,
    last_epoch: Option<i64>,
}

impl ReplayModel {
    pub(crate) fn new(instance: &str, info: ModelInfo, state_dim: usize, log: &PortTrafficLog) -> Self {
        let mut frames_by_epoch: BTreeMap<i64, Vec<(String, Vec<u8>)>> = BTreeMap::new();
        for record in &log.records {
            if record.instance != instance || record.direction != PortDirection::Out as i32 {
                continue;
            }
            frames_by_epoch.entry(record.tai_ns).or_default().push((record.port.clone(), record.payload.clone()));
        }
        let first_epoch = frames_by_epoch.keys().next().copied();
        let last_epoch = frames_by_epoch.keys().next_back().copied();
        Self { instance: instance.to_string(), info, state_dim, frames_by_epoch, first_epoch, last_epoch }
    }
}

impl DynamicsModel for ReplayModel {
    type Error = ReplayError;

    fn state_dim(&self) -> usize {
        self.state_dim
    }

    /// Zero-order hold: there is no bound process left to compute a real derivative from (the
    /// whole point of replay is that the process is removed) -- see the module doc comment's
    /// "What is, and is not, replayed" section. Every instance this task actually replays
    /// declares `state_dim() == 0` (a native controller, or a container instance), so this is
    /// exercised only defensively; a hypothetical replayed instance with a real physical state
    /// would simply never move under replay, honestly, rather than have this binding fabricate
    /// one.
    fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error> {
        state_dot.fill(0.0);
        Ok(())
    }

    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }

    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        Ok(self.step_with_ports(state, t_tai_ns, controls, dt_ns, &Inbox::empty())?.0)
    }

    /// The playback itself -- see the module doc comment's "The missing-frame rule" section.
    /// `state` passes through unchanged (zero-order hold, [`ReplayModel::derivatives`]'s own
    /// doc comment); `inbox` is read by nothing here -- a replay binding never computes an
    /// `AppliedCommand` (there is no real controller logic left behind it to have applied one).
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, _inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let emission_tai_ns = t_tai_ns + dt_ns;
        let mut outbox = Outbox::new();
        match self.frames_by_epoch.get(&emission_tai_ns) {
            Some(frames) => {
                for (port, payload) in frames {
                    outbox.push(port.clone(), emission_tai_ns, payload.clone());
                }
            }
            None => {
                let interior_gap = match (self.first_epoch, self.last_epoch) {
                    (Some(first), Some(last)) => emission_tai_ns > first && emission_tai_ns < last,
                    _ => false,
                };
                if interior_gap {
                    return Err(ReplayError::MissingFrame { instance: self.instance.clone(), tai_ns: emission_tai_ns });
                }
                // Legitimately before this instance's first recorded emission, or after its
                // last: emits nothing, no error -- see the module doc comment's own disclosed
                // limitation (indistinguishable from a deleted leading/trailing record).
            }
        }
        Ok((StepResult { state: state.to_vec(), t_tai_ns: emission_tai_ns, outputs: BTreeMap::new() }, outbox, Vec::new()))
    }

    /// A replay binding produces no fresh CDM measurement of its own -- see the module doc
    /// comment's "What is, and is not, replayed" section: a `Measurement` is decoded by a
    /// sensor's own codec-specific logic from a payload this binding has no decoder for at all
    /// (it only knows bytes and port names), so reproducing one here would either double the
    /// original run's own recorded measurement or require this generic binding to carry
    /// sensor-specific knowledge it deliberately does not have.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::PortTrafficRecord;

    fn info() -> ModelInfo {
        ModelInfo { id: "test.replay".to_string(), version: "1".to_string(), state_space_id: "test.replay.space".to_string(), settings_hash: "deadbeef".to_string(), depth: "native".to_string(), ..Default::default() }
    }

    fn out_rec(instance: &str, port: &str, tai_ns: i64, payload: &[u8]) -> PortTrafficRecord {
        PortTrafficRecord { instance: instance.to_string(), port: port.to_string(), direction: PortDirection::Out as i32, tai_ns, payload: payload.to_vec(), sequence: 0 }
    }
    fn in_rec(instance: &str, port: &str, tai_ns: i64, payload: &[u8]) -> PortTrafficRecord {
        PortTrafficRecord { instance: instance.to_string(), port: port.to_string(), direction: PortDirection::In as i32, tai_ns, payload: payload.to_vec(), sequence: 0 }
    }

    /// Break-and-restore target for T2/T3's own "typed, never held" claim, proven directly here
    /// against the unit rather than only end to end: a step at a recorded epoch plays back
    /// exactly that epoch's own frame, on its own recorded port.
    #[test]
    fn a_step_at_a_recorded_epoch_replays_exactly_that_epochs_own_frame() {
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![out_rec("ctrl", "wheel_torque_out", 1_000, b"payload-a")], provenance: None };
        let model = ReplayModel::new("ctrl", info(), 0, &log);
        let (result, outbox, applied) = model.step_with_ports(&[], 0, &[], 1_000, &Inbox::empty()).unwrap();
        assert_eq!(result.t_tai_ns, 1_000);
        assert!(applied.is_empty());
        let sent = outbox.messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].port, "wheel_torque_out");
        assert_eq!(sent[0].payload, b"payload-a");
    }

    /// IN records for this same instance are never replayed as if they were its own emissions
    /// -- would fail (doubling the frame count, or replaying the wrong payload) against an
    /// implementation that indexed both directions.
    #[test]
    fn in_records_for_this_instance_are_never_played_back() {
        let log = PortTrafficLog {
            run_id: "r".to_string(),
            records: vec![out_rec("ctrl", "wheel_torque_out", 1_000, b"real-out"), in_rec("ctrl", "star_in", 1_000, b"someone-elses-out")],
            provenance: None,
        };
        let model = ReplayModel::new("ctrl", info(), 0, &log);
        let (_result, outbox, _applied) = model.step_with_ports(&[], 0, &[], 1_000, &Inbox::empty()).unwrap();
        let sent = outbox.messages();
        assert_eq!(sent.len(), 1, "the IN record must not also be replayed as an emission: {sent:?}");
        assert_eq!(sent[0].port, "wheel_torque_out");
    }

    /// A step whose own emission epoch has no recorded frame at all, but is BEFORE this
    /// instance's first recorded epoch, is legitimate silence -- no error, an empty Outbox.
    #[test]
    fn a_step_before_the_first_recorded_epoch_emits_nothing_and_is_not_an_error() {
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![out_rec("ctrl", "p", 5_000, b"x")], provenance: None };
        let model = ReplayModel::new("ctrl", info(), 0, &log);
        let (_result, outbox, _applied) = model.step_with_ports(&[], 0, &[], 1_000, &Inbox::empty()).unwrap();
        assert!(outbox.is_empty());
    }

    /// A step AFTER this instance's last recorded epoch is likewise legitimate silence.
    #[test]
    fn a_step_after_the_last_recorded_epoch_emits_nothing_and_is_not_an_error() {
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![out_rec("ctrl", "p", 1_000, b"x")], provenance: None };
        let model = ReplayModel::new("ctrl", info(), 0, &log);
        let (_result, outbox, _applied) = model.step_with_ports(&[], 9_000, &[], 1_000, &Inbox::empty()).unwrap();
        assert!(outbox.is_empty());
    }

    /// A step whose own emission epoch is strictly between this instance's own first and last
    /// recorded epoch, but has nothing recorded, is the interior-gap case -- the ONE thing this
    /// binding refuses rather than silently skips. Break-and-restore target: an implementation
    /// that returns `Ok` with an empty Outbox here instead (i.e. treats every miss as
    /// legitimate silence) would pass every other test in this module but fail this one.
    #[test]
    fn a_step_strictly_between_the_first_and_last_recorded_epoch_with_no_frame_is_a_typed_error() {
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![out_rec("ctrl", "p", 1_000, b"x"), out_rec("ctrl", "p", 3_000, b"y")], provenance: None };
        let model = ReplayModel::new("ctrl", info(), 0, &log);
        // 2_000 is strictly between 1_000 and 3_000, but nothing was recorded there.
        let err = model.step_with_ports(&[], 1_000, &[], 1_000, &Inbox::empty()).unwrap_err();
        assert_eq!(err, ReplayError::MissingFrame { instance: "ctrl".to_string(), tai_ns: 2_000 });
    }

    /// [`ReplayModel::describe`]/`state_dim` report exactly the caller-supplied values, not
    /// anything derived from the log -- proves `crate::registry::ModelRegistry::wrap_replay`'s
    /// own captured-`ModelInfo` design is what a `ReplayModel` actually exposes.
    #[test]
    fn describe_and_state_dim_report_exactly_the_caller_supplied_values() {
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![], provenance: None };
        let model = ReplayModel::new("ctrl", info(), 7, &log);
        assert_eq!(model.state_dim(), 7);
        assert_eq!(model.describe().id, "test.replay");
        assert_eq!(model.describe().settings_hash, "deadbeef");
    }

    /// An instance with NO recorded OUT frames at all (first_epoch/last_epoch both `None`) never
    /// errors, for any epoch -- the degenerate "this instance never emitted anything the whole
    /// run" case the interior-gap rule must not misfire on.
    #[test]
    fn an_instance_with_no_recorded_frames_at_all_never_errors() {
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![], provenance: None };
        let model = ReplayModel::new("ctrl", info(), 0, &log);
        for t in [0i64, 1_000, 1_000_000_000] {
            let (_result, outbox, _applied) = model.step_with_ports(&[], t, &[], 1_000, &Inbox::empty()).unwrap();
            assert!(outbox.is_empty());
        }
    }

    /// `verify_and_load` refuses a byte mismatch before ever trying to decode the file as a
    /// `PortTrafficLog` -- proves the hash check is a real gate, not merely "decode succeeded so
    /// probably fine."
    #[test]
    fn verify_and_load_refuses_a_hash_mismatch() {
        let dir = std::env::temp_dir().join(format!("av_kernel_replay_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("port_traffic.pb");
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![out_rec("ctrl", "p", 1_000, b"x")], provenance: None };
        std::fs::write(&path, log.encode_to_vec()).unwrap();
        let cfg = ReplayConfig { log_path: path.clone(), expected_hash: "0".repeat(64), instances: vec![] };
        let err = verify_and_load(&cfg).unwrap_err();
        assert!(matches!(err, DrmError::ReplayLogHashMismatch { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `verify_and_load` accepts a matching hash and returns the decoded log.
    #[test]
    fn verify_and_load_accepts_a_matching_hash_and_decodes_the_log() {
        let dir = std::env::temp_dir().join(format!("av_kernel_replay_test_ok_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("port_traffic.pb");
        let log = PortTrafficLog { run_id: "r".to_string(), records: vec![out_rec("ctrl", "p", 1_000, b"x")], provenance: None };
        let bytes = log.encode_to_vec();
        std::fs::write(&path, &bytes).unwrap();
        let expected_hash = hash::sha256_hex(&bytes);
        let cfg = ReplayConfig { log_path: path.clone(), expected_hash, instances: vec![] };
        let loaded = verify_and_load(&cfg).expect("matching hash must load");
        assert_eq!(loaded.records.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
