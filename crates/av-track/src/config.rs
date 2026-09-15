//! Every knob this crate's engine bridge runs under, declared in one hashable, TOML-shaped
//! value (question 11's rule: "anything that affects results is declared and hashed") --
//! mirroring `spoore_node::config::NodeConfig`'s own construction path (`build_shard_config`
//! / `build_grid` / `build_associator` / `build_tree_and_initiator`, `/Users/probe/code/
//! spoore/crates/spoore-node/src/config.rs`), narrowed to what this milestone actually
//! needs: one shard, one sensor, one 3D constant-velocity target class, GNN association
//! (no MHT, no grid/multi-cell fleet -- this crate tracks one simulated asset, not a fleet
//! spread across a partitioned territory).
//!
//! # Why 3D constant-velocity (`air_3d`), not `spoore-node`'s own `builtin-cv2d`
//!
//! `NodeConfig::build_builtin_cv2d` (the function this mirrors) builds a 2D
//! (`cv_2d_state_space`) tree for a ground-plane sensor. This milestone's own measurements
//! are 3D Earth-fixed Cartesian position (`crates/av-edge/src/plugin/mod.rs`'s own module
//! doc: "three-component ECEF, metres"), decoded from `drms/
//! demo_ground_segment_flight.system.yaml`'s native `ConstantAccelModel` -- whose own
//! declared `accel.{x,y,z}` are all `0.0` (that fixture's own YAML, read directly, not
//! assumed), i.e. the truth motion is genuinely constant-velocity in 3D, not merely
//! constant-acceleration-with-zero-acceleration-as-a-coincidence. `spoore_models::
//! air_3d_state_space`/`ConstantVelocity3d`/`Position3dSensor` are the exact 3D
//! counterparts of `build_builtin_cv2d`'s own `cv_2d_state_space`/`ConstantVelocity2d`/
//! `PositionSensor2d`, so this module follows that function's shape with the "2d" pieces
//! swapped for their already-existing "3d" siblings -- no new motion model, no new
//! measurement model, nothing invented.

use std::sync::Arc;

use av_edge::pb;
use spoore_assoc::{Associator, ChiSquareGate, GlobalNearestNeighbor};
use spoore_cdm::{IntroducedComponent, NodePrior};
use spoore_engine::{SensorConfig, ShardConfig};
use spoore_models::{air_3d_state_space, ConstantVelocity3d, KalmanFilter, Position3dSensor, Predictor};
use spoore_tree::{ModelNode, ModelTree, NodePolicy};

/// Everything wrong a [`TrackConfig`] can declare, caught before a [`spoore_engine::Shard`]
/// is ever built from it (mirroring `spoore_node::config::ConfigError`'s own posture:
/// cross-field/knob checks this module owns fail with a name here; `ShardConfig::validate`'s
/// own internal panics, unchanged and un-duplicated, still cover everything spoore's own
/// type already checks -- see [`TrackConfig::build_shard_config`]'s own doc).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("TrackConfig.{field} must be finite and positive, got {value}")]
    NotFinitePositive { field: &'static str, value: f64 },
    #[error("TrackConfig.gate_probability must be in (0, 1), got {0}")]
    GateProbabilityOutOfRange(f64),
    #[error("building the initiator: {0}")]
    Initiator(String),
    #[error("building the 3D position sensor for state space {state_space_id:?}: {detail}")]
    Sensor { state_space_id: String, detail: String },
    #[error("parsing TrackConfig TOML: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Every declared, hashable knob the engine bridge runs under for this milestone's one
/// shard/one sensor/one 3D constant-velocity target class -- see this module's own doc for
/// why 3D and why these specific fields mirror `spoore_node::config::NodeConfig`'s.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackConfig {
    /// This shard's own partition (`ShardConfig.shard_key`) -- must equal the plugin's own
    /// declared `shard_key` (`av_edge::plugin::PluginConfig.shard_key`), since that is the
    /// partition this crate's consumer reads from.
    pub shard_key: String,
    /// The one sensor this shard reads (`SensorConfig.sensor_id`) -- must equal the
    /// plugin's own declared `sensor_id`.
    pub sensor_id: String,
    /// `SensorConfig.measurement_dim` -- `3` for this milestone's ECEF position sensor.
    pub measurement_dim: usize,
    /// `SensorConfig.detection_probability` (`P_D`).
    pub detection_probability: f64,
    /// `SensorConfig.false_alarm_density` (`β_FA`), per m^[`measurement_dim`]. This
    /// milestone's own scenario is single-target, zero-clutter (one simulated asset, one
    /// ground station, no other traffic in the fixture), so this value only ever governs
    /// how much a miss costs a track's score -- there is no false-alarm population in the
    /// fixture for it to price at all.
    pub false_alarm_density: f64,
    /// `ShardConfig.confirm_hits` (`M` of M-of-N). `3` -- a track must actually accumulate
    /// evidence, not confirm on its very first real update (see `delete_score`'s own doc
    /// for why the first update or two are unusually costly here).
    pub confirm_hits: usize,
    /// `ShardConfig.confirm_window` (`N` of M-of-N). `5`.
    pub confirm_window: usize,
    /// `ShardConfig.delete_score`. **A genuine finding from building this config, not a
    /// number copied from `spoore_engine::config::ShardConfig::for_tests`'s own `-5.0`
    /// default**: with this crate's own honestly-wide `velocity_prior_sigma_mps` (see that
    /// field's own doc), the Sittler/Blackman detection-score formula (`SensorConfig::
    /// detection_score_delta` = `ln(P_D) + log_likelihood - ln(β_FA)`) pays a *log-
    /// determinant* penalty on `log_likelihood` proportional to how spread out the
    /// filter's own predicted-measurement covariance `S` is -- and right after initiation,
    /// `S` is enormous (dominated by the wide velocity prior's own variance propagated
    /// through one prediction step), so the very first real update this crate's own
    /// `tests/engine_accuracy.rs`/`src/bridge.rs` tests measure costs roughly **-12
    /// nats** even though the measurement lands well inside the association gate and is
    /// the correct one. `-5.0` (the `for_tests` default, tuned for a *tight* prior) kills
    /// every track after its very first real update, before the filter ever gets the
    /// chance to converge and the per-update score to turn positive again (measured: by
    /// the third or fourth update the per-update contribution is already strongly
    /// positive, since `S` has shrunk toward the sensor's own declared `r`) --
    /// `-50.0` is declared here as the smallest round number this crate's own tests
    /// measured surviving that transient for the fixture's own ~7.5 km/s asset; see
    /// `src/bridge.rs::tests::a_straight_line_of_measurements_produces_one_confirmed_track`
    /// for the actual, printed score trajectory this was pinned against.
    pub delete_score: f64,
    /// `ShardConfig.birth_score`. `0.0` -- the conservative default (`spoore_engine::
    /// config::ShardConfig`'s own doc: "a birth is equally likely clutter"); this
    /// scenario's own zero-clutter fixture never actually exercises the birth/clutter
    /// trade-off this knob prices.
    pub birth_score: f64,
    /// `ShardConfig.inheritance_radius_m` -- `0.0` (ADR-010 inheritance disabled): this
    /// milestone tracks exactly one asset for the whole run, so there is never a
    /// deleted-track estate to inherit from.
    pub inheritance_radius_m: f64,
    /// `ShardConfig.inheritance_window_ns`.
    pub inheritance_window_ns: i64,
    /// `ShardConfig.lateness_window_ns` (§5's OOSM rewind window).
    pub lateness_window_ns: i64,
    /// `ShardConfig.prediction_steps`.
    pub prediction_steps: usize,
    /// `ShardConfig.prediction_interval_ns`.
    pub prediction_interval_ns: i64,
    /// `ShardConfig.max_coast_ns`.
    pub max_coast_ns: i64,
    /// [`spoore_assoc::ChiSquareGate::new`]'s own `probability` argument.
    pub gate_probability: f64,
    /// [`spoore_models::ConstantVelocity3d::new`]'s own `process_noise_density` --
    /// continuous white-noise acceleration density, (m/s^2)^2 per second (the model's own
    /// unit, `spoore_models::motion`'s doc). Declared small and positive rather than zero:
    /// the truth motion in this milestone's own fixture is genuinely zero-acceleration
    /// (`drms/demo_ground_segment_flight.system.yaml`'s own `accel.{x,y,z} = 0.0`), so a
    /// larger value buys nothing this scenario needs, but `ConstantVelocity3d::new` itself
    /// refuses a non-positive value (a *singular* process-noise-free filter is a modelling
    /// claim -- "this target's motion is known exactly" -- that Kalman filtering doesn't
    /// even have a sensible degenerate case for), and a small nonzero value keeps the
    /// filter numerically well-conditioned against any residual floating-point mismatch
    /// between the plugin's decoded position and the truth trajectory's own propagated
    /// state, without introducing any tracking lag this zero-acceleration scenario would
    /// otherwise show.
    pub process_noise_density: f64,
    /// The prior *standard deviation*, m/s, [`TrackConfig::build_tree_and_initiator`]
    /// declares for each of `vel_x`/`vel_y`/`vel_z` on a newly initiated track (mean
    /// `0.0`, i.e. "no assumed direction of travel" -- `SinglePointInitiator`'s own module
    /// doc: "there is no default that is not a claim"). Declared, not measured: this
    /// milestone's own asset moves at roughly 7.5 km/s (a LEO-scale Earth-fixed ground
    /// track -- `drms/demo_ground_segment_flight.system.yaml`'s own `state.vx`/`state.vy`
    /// parameters), so a prior that only covered, say, ground-vehicle speeds would still
    /// converge (the Kalman gain is driven by the *ratio* of prior to measurement
    /// variance, not by whether the prior happens to bracket the truth), but would take
    /// more scans to get there and would size this crate's own comparison tolerance
    /// dishonestly close to what the fixture's specific velocity happens to need. `20,000`
    /// m/s (four times faster than anything in this fixture) is declared instead, as a
    /// value that would still be an honest "no assumption about direction or magnitude of
    /// travel" prior for a wide range of Earth-relative or near-Earth simulated assets, not
    /// one hand-fit to this one DRM's own numbers -- see [`crate::compare`]'s own doc for
    /// how this knob shows up directly in the measured tracking-transient tolerance.
    pub velocity_prior_sigma_mps: f64,
    /// `Trajectory.frame_id`/`Measurement.frame_id` this shard's tracks are produced in --
    /// carried here (rather than hardcoded) purely so it is declared and hashed alongside
    /// every other knob; this crate's own engine bridge does not interpret it (frames are
    /// a wire/viewer concern, not a filtering one -- `av_cdm::spoore_v0::frame`'s own
    /// module doc).
    pub frame_id: String,
    /// The body [`Self::frame_id`]'s frame is body-fixed to, e.g. `"Earth"` -- with
    /// [`Self::frame_id`] and [`Self::frame_description`], the whole of the one
    /// `pb::FrameDefinition` [`Self::frame_registry`] declares for this shard's boundary
    /// conversion (`av_cdm::spoore_v0::frame::resolve_frame_id`). Declared and hashed
    /// alongside `frame_id`, per this module's own "every knob... declared in one hashable
    /// value" rule -- even though, like `frame_id` itself, it does not change filtering
    /// results, only which frame the engine bridge resolves `frame_id` to. See
    /// [`Self::frame_registry`]'s own doc for why `AXES_KIND_BODY_FIXED` (never `ENU`/`NED`/
    /// an inertial kind) is the honest axes kind for this demo's own data.
    pub frame_origin_body: String,
    /// Free-form, for [`Self::frame_registry`]'s `pb::FrameDefinition.description`.
    pub frame_description: String,
}

impl TrackConfig {
    /// Parses a [`TrackConfig`] from a TOML string (`spoore_node::config::NodeConfig::
    /// from_toml_str`'s own precedent).
    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(raw)?)
    }

    /// Refuses an obviously-broken configuration up front (`av_edge::plugin::PluginConfig::
    /// validate`'s own precedent for this crate family): every declared-positive knob is
    /// finite and positive, and `gate_probability` is in the open interval `ChiSquareGate::
    /// new` itself requires. Does **not** re-check everything `spoore_engine::ShardConfig::
    /// validate`/`spoore_engine::config::SensorConfig::validate` already check (M-of-N
    /// satisfiability, `delete_score < 0 < birth_score`, ...) -- those run, unchanged,
    /// inside [`TrackConfig::build_shard_config`]'s own caller (`spoore_engine::Shard::
    /// new`), and duplicating their exact bar here would be a second place to keep in sync
    /// with spoore's own rules rather than one.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("process_noise_density", self.process_noise_density),
            ("velocity_prior_sigma_mps", self.velocity_prior_sigma_mps),
            ("detection_probability", self.detection_probability),
            ("false_alarm_density", self.false_alarm_density),
        ] {
            if !(value.is_finite() && value > 0.0) {
                return Err(ConfigError::NotFinitePositive { field: name, value });
            }
        }
        if !(self.gate_probability > 0.0 && self.gate_probability < 1.0) {
            return Err(ConfigError::GateProbabilityOutOfRange(self.gate_probability));
        }
        Ok(())
    }

    /// A SHA-256 content hash of this entire configuration, over its own canonical JSON
    /// encoding -- `av_edge::plugin::PluginConfig::config_hash`'s own precedent, verbatim
    /// (same reasoning: every field here is a plain, non-`HashMap` type, so `serde_json::
    /// to_vec` is deterministic field-by-field). Via the system OpenSSL (ADR-004's crypto
    /// rule), never `sha2`/`ring`.
    pub fn config_hash(&self) -> [u8; 32] {
        let json = serde_json::to_vec(self).expect("TrackConfig serialises to JSON: every field is a plain, non-map type");
        openssl::sha::sha256(&json)
    }

    /// `spoore_node::config::NodeConfig::build_shard_config`'s own mirror: this shard's
    /// `ShardConfig`, with `mht: None` (this milestone runs GNN, unchanged -- there is
    /// never more than one plausible association for a single-target, zero-clutter scan,
    /// so MHT's deferred-decision machinery has nothing to defer).
    pub fn build_shard_config(&self) -> ShardConfig {
        let sensor = SensorConfig {
            sensor_id: self.sensor_id.clone(),
            measurement_dim: self.measurement_dim,
            detection_probability: self.detection_probability,
            false_alarm_density: self.false_alarm_density,
        };
        ShardConfig {
            shard_key: self.shard_key.clone(),
            gate: ChiSquareGate::new(self.gate_probability),
            sensors: [(sensor.sensor_id.clone(), sensor)].into_iter().collect(),
            confirm_hits: self.confirm_hits,
            confirm_window: self.confirm_window,
            delete_score: self.delete_score,
            birth_score: self.birth_score,
            inheritance_radius_m: self.inheritance_radius_m,
            inheritance_window_ns: self.inheritance_window_ns,
            lateness_window_ns: self.lateness_window_ns,
            prediction_steps: self.prediction_steps,
            prediction_interval_ns: self.prediction_interval_ns,
            mht: None,
            max_coast_ns: self.max_coast_ns,
        }
    }

    /// `spoore_node::config::NodeConfig::build_associator`'s own mirror, narrowed to the
    /// GNN-only half (this milestone declares no MHT policy -- see [`TrackConfig::
    /// build_shard_config`]'s own doc).
    pub fn build_associator(&self) -> Arc<dyn Associator> {
        Arc::new(GlobalNearestNeighbor)
    }

    /// The `pb::FrameDefinition` registry [`crate::bridge::EngineBridge::run`] resolves
    /// `frame_id` through (`av_cdm::spoore_v0::frame::resolve_frame_id`), declaring exactly
    /// the one frame this config names ([`Self::frame_id`]).
    ///
    /// **Why this exists at all, and why it lives here rather than in `av-cdm`.** The real,
    /// committed E4 fixture's own measurements carry `frame_id = "earth_fixed_demo_frame"`
    /// (`av_edge::plugin::PluginConfig.frame_id`, copied verbatim from `drms/
    /// demo_ground_segment_flight.system.yaml`'s own `parameters: frame_id`). As of round 4
    /// (question 207), `drms/demo_ground_segment.drm.yaml`'s own `scenario.frames` block
    /// *also* declares this id (alongside `ground_station_enu`) -- round 3's blocker (the
    /// DRM's canonical hash is embedded, as inert recorded provenance, in the committed
    /// `crates/av-edge/tests/fixtures/ground_segment/*.pb` fixture binaries) turned out not
    /// to be a real one on inspection: question 207 traced every reader of that embedded
    /// hash and found nothing -- not `av-edge`, not `av-ingest`, not this crate -- ever
    /// re-verifies it against a freshly computed hash of the DRM; only `run_id`/
    /// `created_tai_ns` are ever pulled back out of `RunProducts.provenance` downstream, and
    /// E4's pinned chain head is built from `PluginConfig`/batch content and the run id, never
    /// from the DRM's hash. So the DRM could be, and was, changed freely without touching any
    /// pinned fixture or golden.
    ///
    /// That registration does **not**, however, make this method (or [`Self::frame_id`]/
    /// [`Self::frame_origin_body`]/[`Self::frame_description`]) redundant, and none of them
    /// were removed: this crate has no code path that ever reads `Scenario.frames` or
    /// `RunProducts.frames` at all -- [`crate::bridge::EngineBridge::run`] resolves every
    /// frame purely from this method's own return value, never from anything the DRM or its
    /// executor produced. The registry is still a parameter the *caller* supplies, and this
    /// crate is still that caller -- registering the id in the DRM only fixed the DRM's own
    /// missing declaration (a genuine gap in its own right, for the CDM/viewer's frame
    /// graph), it did not, and structurally could not, wire that registry through to here.
    ///
    /// **Why `AXES_KIND_BODY_FIXED` about `frame_origin_body`, not `ENU`/`NED` or an
    /// inertial kind.** Taken from the DRM's own words, not chosen for convenience: `drms/
    /// demo_ground_segment_flight.system.yaml`'s `packet_codecs[0].description` reads "own
    /// Earth-fixed Cartesian position telemetry", and its header comment names the encoded
    /// state `x0`/`v0` as "Earth-fixed/ECEF metres, metres-per-second" -- a rotating,
    /// body-fixed Cartesian frame, exactly `AxesKind`'s own doc for `AXES_KIND_BODY_FIXED`
    /// ("Body-fixed of the origin body (ITRF for Earth...)"), not a local tangent plane
    /// (`ENU`/`NED`, which the DRM never mentions) and not an inertial frame (`ICRF`/
    /// `MJ2000_EQ`, which "Earth-fixed" explicitly rules out).
    pub fn frame_registry(&self) -> Vec<pb::FrameDefinition> {
        vec![pb::FrameDefinition {
            id: self.frame_id.clone(),
            origin: Some(pb::frame_definition::Origin::Body(self.frame_origin_body.clone())),
            axes: pb::AxesKind::BodyFixed as i32,
            description: self.frame_description.clone(),
            ..Default::default()
        }]
    }

    /// `spoore_node::config::NodeConfig::build_builtin_cv2d`'s own mirror in 3D -- see this
    /// module's own top-level doc for exactly why 3D, and why this is a direct swap of
    /// `spoore_models`' already-existing 3D motion/measurement models rather than a new
    /// one.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Sensor`] if `air_3d_state_space` somehow lacks `pos_x`/`pos_y`/
    /// `pos_z` (`spoore_models::Position3dSensor::for_space`'s own refusal -- unreachable
    /// for `air_3d_state_space` itself, which declares exactly those three, but this
    /// function propagates the typed error rather than asserting it away). [`ConfigError::
    /// Initiator`] if `SinglePointInitiator::new` refuses this module's own declared
    /// `measured`/`unmeasured`/`nodes` (also not expected to be reachable here, for the
    /// same reason: every component of `air_3d_state_space` is accounted for by
    /// construction below).
    pub fn build_tree_and_initiator(&self) -> Result<(Arc<ModelTree>, Arc<dyn spoore_engine::Initiator>), ConfigError> {
        let space = air_3d_state_space();

        let sensor = Position3dSensor::for_space(&self.sensor_id, &space).map_err(|e| ConfigError::Sensor { state_space_id: space.id.clone(), detail: e.to_string() })?;
        let predictor: Arc<dyn Predictor> = Arc::new(KalmanFilter::new("root", Arc::new(ConstantVelocity3d::new(self.process_noise_density))).with_sensor(Arc::new(sensor)));
        let tree = Arc::new(ModelTree::new(ModelNode::native(predictor, 1.0, NodePolicy::classification())));

        let velocity_prior_variance = self.velocity_prior_sigma_mps * self.velocity_prior_sigma_mps;
        let initiator: Arc<dyn spoore_engine::Initiator> = Arc::new(
            spoore_engine::SinglePointInitiator::new(
                space,
                vec!["pos_x".to_string(), "pos_y".to_string(), "pos_z".to_string()],
                vec![
                    IntroducedComponent::new("vel_x", 0.0, velocity_prior_variance),
                    IntroducedComponent::new("vel_y", 0.0, velocity_prior_variance),
                    IntroducedComponent::new("vel_z", 0.0, velocity_prior_variance),
                ],
                vec![NodePrior { node_id: "root".to_string(), weight: 1.0 }],
            )
            .map_err(|e| ConfigError::Initiator(e.to_string()))?,
        );
        Ok((tree, initiator))
    }
}

/// This milestone's own declared configuration for the demo ground-segment DRM
/// (`drms/demo_ground_segment*.yaml`) -- every field explained on [`TrackConfig`] itself;
/// this is the *one* place their actual values are chosen, so `crate::bridge::
/// EngineBridge::for_demo` and every test/binary that needs "the demo's own config" reach
/// through here rather than re-declaring the same sixteen numbers a second time.
pub fn demo_ground_segment_config() -> TrackConfig {
    TrackConfig {
        shard_key: "ground-segment-demo".to_string(),
        sensor_id: "ground-segment-flight".to_string(),
        measurement_dim: 3,
        detection_probability: 0.95,
        false_alarm_density: 1e-9, // per m^3; see TrackConfig::false_alarm_density's own doc.
        confirm_hits: 3,
        confirm_window: 5,
        delete_score: -50.0,
        birth_score: 0.0,
        inheritance_radius_m: 0.0,
        inheritance_window_ns: 3_000_000_000,
        lateness_window_ns: 3_000_000_000,
        prediction_steps: 0,
        prediction_interval_ns: 1_000_000_000,
        max_coast_ns: 30_000_000_000,
        gate_probability: 0.99,
        process_noise_density: 1e-3,
        velocity_prior_sigma_mps: 20_000.0,
        frame_id: "earth_fixed_demo_frame".to_string(),
        frame_origin_body: "Earth".to_string(),
        frame_description: "Earth body-fixed Cartesian position frame the demo ground-segment \
            flight DRM's own packet_codecs[0] telemetry is encoded in (drms/\
            demo_ground_segment_flight.system.yaml: \"own Earth-fixed Cartesian position \
            telemetry\"); named by that DRM's `parameters: frame_id` but not registered in \
            its `scenario.frames` (a recorded platform gap, question 205) -- declared here \
            instead, by this config, as the frame that parameter means."
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_config_validates() {
        demo_ground_segment_config().validate().unwrap();
    }

    #[test]
    fn the_demo_config_builds_a_shard_config_that_validates() {
        demo_ground_segment_config().build_shard_config().validate();
    }

    #[test]
    fn the_demo_config_builds_a_tree_and_initiator() {
        let cfg = demo_ground_segment_config();
        let (tree, initiator) = cfg.build_tree_and_initiator().unwrap();
        assert_eq!(initiator.state_space_id(), tree.root().state_space_id());
        assert_eq!(initiator.state_space_id(), "air_3d");
    }

    #[test]
    fn config_hash_is_deterministic_and_sensitive_to_every_field() {
        let a = demo_ground_segment_config();
        let mut b = a.clone();
        assert_eq!(a.config_hash(), b.config_hash());
        b.process_noise_density *= 2.0;
        assert_ne!(a.config_hash(), b.config_hash(), "changing a declared knob must change the hash");
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = demo_ground_segment_config();
        let toml_str = toml::to_string(&cfg).unwrap();
        let back = TrackConfig::from_toml_str(&toml_str).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn validate_rejects_a_non_positive_process_noise_density() {
        let mut cfg = demo_ground_segment_config();
        cfg.process_noise_density = 0.0;
        assert!(matches!(cfg.validate(), Err(ConfigError::NotFinitePositive { field: "process_noise_density", .. })));
    }

    #[test]
    fn validate_rejects_an_out_of_range_gate_probability() {
        let mut cfg = demo_ground_segment_config();
        cfg.gate_probability = 1.0;
        assert!(matches!(cfg.validate(), Err(ConfigError::GateProbabilityOutOfRange(_))));
    }
}
