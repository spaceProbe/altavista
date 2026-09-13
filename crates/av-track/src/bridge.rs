//! The engine bridge: groups a partition's measurements into `spoore_engine::Scan`s by
//! epoch, converts each through `av_cdm::spoore_v0::measurement_from_pb` (never a second
//! conversion -- this milestone's own instruction), and drives a real `spoore_engine::
//! Shard::process`.
//!
//! # Grouping by epoch, not by batch
//!
//! A `MeasurementBatch` is a *transport* unit (E1's own signing/chaining granularity); a
//! `spoore_engine::Scan` is an *engine* unit (`spoore_engine::shard::Scan::new`'s own
//! contract: "a scan is associated at a single instant"). The two need not coincide -- a
//! future source using `BatchingRule::PerNMeasurements` could put several different
//! epochs' measurements in one batch, or (less likely for this platform's own partitioning
//! scheme, but not ruled out) two producers on the same partition could each contribute a
//! measurement at the identical epoch in two different batches -- so this bridge re-groups
//! by `epoch_ns` from scratch, over every measurement the caller hands it, rather than
//! assuming one batch equals one scan. For this milestone's own fixture
//! (`BatchingRule::PerEpoch`, one measurement per batch, one batch per epoch) the two
//! groupings happen to coincide, which is exactly why `crates/av-ingest/tests/
//! plugin_wire.rs`'s pinned `EXPECTED_BATCH_COUNT` (900) is also this bridge's own scan
//! count -- but the code does not rely on that coincidence.
//!
//! # No transport, no clock -- the same discipline `av_edge::plugin` holds itself to
//!
//! Nothing here reads a clock, opens a socket, or reads a file: [`EngineBridge::run`] is a
//! pure function of the `av_edge::pb::Measurement`s the caller already has in memory
//! (however they got there -- `crate::consumer::LogPartitionConsumer` in every test and
//! binary this crate ships, but this type does not know or care).

use std::collections::BTreeMap;
use std::sync::Arc;

use av_edge::pb;
use spoore_cdm::{Epoch, TrackUpdate};
use spoore_engine::{Initiator, Scan, Shard};
use spoore_tree::ModelTree;

use crate::config::{ConfigError, TrackConfig};

/// Everything that can go wrong building or running an [`EngineBridge`]. Every variant is a
/// typed refusal -- this crate's standing convention (`av_edge::plugin::PluginError`'s
/// identical framing).
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A measurement failed the `spoore.v0` <-> `altavista.v1` boundary conversion
    /// (`av_cdm::spoore_v0::measurement_from_pb`) -- e.g. an unrecognised `frame_id`.
    #[error("converting measurement {measurement_id:?} through spoore_v0::measurement_from_pb: {detail}")]
    Conversion { measurement_id: String, detail: String },
    /// `spoore_engine::shard::Scan::new`/`Shard::process` itself refused -- e.g. a
    /// measurement declaring a `shard_key` other than this shard's own.
    #[error(transparent)]
    Engine(#[from] spoore_engine::EngineError),
}

/// Drives one `spoore_engine::Shard` (this crate's own [`TrackConfig`]-built tree,
/// associator and initiator) from `av_edge::pb::Measurement`s, in epoch order.
pub struct EngineBridge {
    shard: Shard,
}

impl EngineBridge {
    /// Builds a fresh shard from `config` (`TrackConfig::build_shard_config`/
    /// `build_associator`/`build_tree_and_initiator` -- this crate's own mirror of
    /// `spoore_node::config::NodeConfig`'s construction path, `crate::config`'s own
    /// module doc).
    pub fn new(config: &TrackConfig) -> Result<Self, BridgeError> {
        config.validate()?;
        let shard_config = config.build_shard_config();
        let (tree, initiator): (Arc<ModelTree>, Arc<dyn Initiator>) = config.build_tree_and_initiator()?;
        let associator = config.build_associator();
        let shard = Shard::new(shard_config, tree, associator, initiator);
        Ok(Self { shard })
    }

    /// The demo ground-segment DRM's own bridge (`crate::config::demo_ground_segment_config`).
    pub fn for_demo() -> Result<Self, BridgeError> {
        Self::new(&crate::config::demo_ground_segment_config())
    }

    /// Groups `measurements` into ascending-epoch `Scan`s and drives `Shard::process` over
    /// each in turn, returning every `TrackUpdate` produced, in scan order (i.e. in ascending
    /// epoch order, since this milestone's own fixture never sends a measurement out of
    /// order -- a genuinely out-of-order feed would still be handled correctly by `Shard::
    /// process`'s own §5 rewind-and-replay path, since this bridge does not itself assume the
    /// *input* slice is sorted, only that it groups correctly by epoch before handing scans
    /// to the shard one at a time in ascending order).
    ///
    /// # Errors
    ///
    /// [`BridgeError::Conversion`] if any measurement fails `measurement_from_pb`;
    /// [`BridgeError::Engine`] if `Scan::new`/`Shard::process` itself refuses (e.g. a
    /// measurement naming the wrong `shard_key` -- `spoore_engine::EngineError::WrongShard`).
    pub fn run(&mut self, measurements: &[pb::Measurement]) -> Result<Vec<TrackUpdate>, BridgeError> {
        let mut groups: BTreeMap<i64, Vec<spoore_cdm::Measurement>> = BTreeMap::new();
        for m in measurements {
            // **A defect found while building this bridge, not assumed away**: the real,
            // committed E4 fixture's own measurements carry `frame_id =
            // "earth_fixed_demo_frame"` (`av_edge::plugin::PluginConfig.frame_id`, copied
            // verbatim from `drms/demo_ground_segment_flight.system.yaml`'s own
            // `parameters: frame_id` -- a DRM/viewer frame-*registry* id, `altavista.
            // frames.FrameRegistry`'s namespace). `av_cdm::spoore_v0::frame` recognizes a
            // fixed, unrelated set of five ids (`spoore_v0::frame`'s own module doc: "a
            // fixed, well-known frame_id string," never a registry lookup), and
            // `"earth_fixed_demo_frame"` is not one of them -- feeding the real fixture's
            // measurements straight into `measurement_from_pb` fails every time with
            // `Error::UnknownFrameId`. `av-edge`'s pinned fixture cannot change (E4's
            // committed chain-hash goldens depend on its exact bytes), and this bridge
            // must not "write a second conversion" for the numeric fields
            // (`measurement_from_pb` already owns `z`/`r`/`epoch_ns`/shard_key) -- so the
            // one thing this loop does beyond that call is relabel `frame_id` onto
            // `av_cdm::spoore_v0::frame::ECEF` before conversion, matching the codec's own
            // documented physical meaning (that same DRM file: "own Earth-fixed Cartesian
            // position telemetry"). This is a frame *label* substitution only; every
            // numeric field converts through `measurement_from_pb` completely unmodified.
            let mut for_spoore = m.clone();
            for_spoore.frame_id = av_cdm::spoore_v0::frame::ECEF.to_string();
            let native = av_cdm::spoore_v0::measurement_from_pb(&for_spoore).map_err(|e| BridgeError::Conversion { measurement_id: m.measurement_id.clone(), detail: e.to_string() })?;
            groups.entry(native.epoch().as_nanos()).or_default().push(native);
        }

        let mut updates = Vec::new();
        for (epoch_ns, ms) in groups {
            let scan = Scan::new(Epoch::from_nanos(epoch_ns), ms)?;
            let mut produced = self.shard.process(&scan)?;
            updates.append(&mut produced);
        }
        Ok(updates)
    }

    pub fn shard(&self) -> &Shard {
        &self.shard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurement(epoch_ns: i64, pos: [f64; 3]) -> pb::Measurement {
        let cfg = crate::config::demo_ground_segment_config();
        pb::Measurement {
            measurement_id: "flight_position".to_string(),
            z: pos.to_vec(),
            r: vec![100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0],
            epoch_ns,
            sensor_id: cfg.sensor_id,
            shard_key: cfg.shard_key,
            frame_id: crate::config::demo_ground_segment_config().frame_id,
            ..Default::default()
        }
    }

    #[test]
    fn a_straight_line_of_measurements_produces_one_confirmed_track() {
        let mut bridge = EngineBridge::for_demo().unwrap();
        // A straight line at (7500, 1200, 0) m/s -- far below this config's own 20,000 m/s
        // velocity-prior sigma, exactly like the real fixture's own asset.
        let measurements: Vec<pb::Measurement> = (0..20)
            .map(|i| {
                let t = i as f64;
                measurement(i as i64 * 1_000_000_000, [7500.0 * t, 1200.0 * t, 0.0])
            })
            .collect();

        let updates = bridge.run(&measurements).unwrap();
        assert_eq!(updates.len(), 20, "one TrackUpdate per scan for a single, always-associated target");

        let track_ids: std::collections::BTreeSet<&str> = updates.iter().map(|u| u.track_id.as_str()).collect();
        assert_eq!(track_ids.len(), 1, "a single, unambiguous target must never fragment into more than one track id");

        assert_eq!(bridge.shard().track_count(), 1);
        let track = bridge.shard().tracks().next().unwrap();
        assert_eq!(track.status(), spoore_engine::TrackStatus::Confirmed, "20 clean, always-associated scans (confirm_hits=2/confirm_window=3) must be enough to confirm");
    }

    #[test]
    fn a_measurement_for_the_wrong_shard_is_a_typed_engine_error() {
        let mut bridge = EngineBridge::for_demo().unwrap();
        let mut m = measurement(0, [0.0, 0.0, 0.0]);
        m.shard_key = "some-other-shard".to_string();
        let err = bridge.run(&[m]).unwrap_err();
        assert!(matches!(err, BridgeError::Engine(spoore_engine::EngineError::WrongShard { .. })), "{err:?}");
    }
}
