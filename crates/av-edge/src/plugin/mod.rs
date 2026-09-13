//! The first plugin: a simulated asset (`docs/edge-plan.md` milestone E4; question 200(a)).
//!
//! # What this plugin replays, and why
//!
//! The asset is `drms/demo_ground_segment_flight.system.yaml`'s flight instance
//! (`ground_segment_flight_sys`, bound as `"flight"` in `drms/demo_ground_segment.sos.yaml`);
//! the measurements are its own position telemetry, decoded from the run's `PortTrafficLog`
//! sidecar (question 175) through the DRM's own declared `PacketCodec`
//! (`flight_tm_out_codec`, three FLOAT64 fields `x`/`y`/`z`, Earth-fixed metres) --
//! `docs/edge-plan.md` milestone E4's own explicitly-allowed route, not
//! `RunProducts.measurements`. Confirmed by reading the real code, not assumed:
//!
//! - `flight_tm_out_codec`'s three fields declare **no `PacketField.target`** at all, so
//!   `av_codec::measurements_from_field_values` -- confirmed by grep, and by that
//!   function's own "never invents a measurement" doc comment -- produces nothing for this
//!   DRM; `RunProducts.measurements` is empty for it.
//! - `crate::drm::ground::GroundStationModel::last_measurements` returns `Vec::new()` by
//!   design (`av_kernel::drm::ground`'s own module doc): the ground station never produces
//!   a CDM measurement either.
//! - So the only place this run's position telemetry exists as recoverable data is the
//!   `PortTrafficLog` sidecar itself -- exactly the route this module implements.
//!
//! These are **position** measurements (three-component ECEF, metres) that correspond
//! directly to `"flight"`'s own truth `Trajectory` (`crate::drm::binding::ConstantAccelModel`
//! propagates `state[0..3]` and broadcasts exactly that on `tm_out` every step) -- what E5's
//! engine-consumption exit needs. `crates/av-edge/tests/plugin_replay.rs` measures and prints
//! the maximum deviation between this module's decoded positions and the run's own truth
//! trajectory directly, rather than merely asserting it is small.
//!
//! # Module map
//!
//! - [`packet`] -- a thin adapter over `av_codec::decode_packet` (question 205; that module's
//!   own doc comment has the detail, including the one narrow, documented ordering
//!   difference from the from-scratch decoder it replaced).
//! - [`PluginConfig`] -- every declared knob this plugin's behaviour depends on, in one
//!   serialisable, hashable value (see [`PluginConfig::config_hash`]) -- nothing here is a
//!   hard-coded constant.
//! - [`MeasurementSource`] -- the trait a replay source implements; [`PortTrafficSource`] is
//!   this milestone's one implementation.
//! - [`BatchingRule`] / [`Pacing`] -- the two configurable, pure policies
//!   [`BatchBuilder`]/a plugin binary apply; see [`Pacing::due_at`]'s own doc comment for why
//!   there is no sleeping anywhere in this crate.
//! - [`BatchBuilder`] -- turns a source's groups into signed, chained `MeasurementBatch`es.
//!
//! # No transport, no clock, no sleeping -- verbatim, in this module too
//!
//! Nothing in this module opens a socket, reads a file, or reads a clock. [`PortTrafficSource::
//! from_log`] takes an already-decoded `pb::PortTrafficLog` (the plugin binary reads and
//! hash-verifies the sidecar bytes -- via [`verify_port_traffic_log`], which is itself a pure
//! function of already-read bytes, never touching the filesystem); [`Pacing::due_at`] is a
//! pure function of caller-supplied epochs, never a sleep -- see that method's own doc
//! comment. This is what lets `crates/av-edge/tests/plugin_replay.rs` exercise this module
//! with no wire, no wall-clock dependency, and byte-identical results run to run.

pub mod packet;

use std::collections::BTreeMap;

use openssl::ec::EcKeyRef;
use openssl::pkey::Private;

use crate::hash;
use crate::pb;
use crate::sign::{self, SigningError};

/// Everything that can go wrong building or replaying a plugin's configuration. Every
/// variant names a refusal, never a silent fallback or a fabricated value (this crate's own
/// standing convention -- see `crate::sign::SigningError`'s identical framing).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PluginError {
    #[error("PluginConfig.{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("PluginConfig.component_fields must not be empty")]
    EmptyComponentFields,
    #[error("PluginConfig.noise_r has {actual} element(s); component_fields.len()^2 == {expected} elements are required for a {n}x{n} row-major covariance")]
    NoiseDimensionMismatch { expected: usize, actual: usize, n: usize },
    #[error("PluginConfig.noise_r is not a valid SPD covariance: {0}")]
    NoiseNotSpd(String),
    #[error("PluginConfig.codec_bytes does not decode as altavista.v1.PacketCodec: {0}")]
    CodecDecode(String),
    #[error("PluginConfig.label_bytes does not decode as altavista.v1.Label: {0}")]
    LabelDecode(String),
    #[error(transparent)]
    Packet(#[from] packet::PacketError),
    #[error("record for instance {instance:?} port {port:?} at epoch {epoch_ns} decoded no value for declared component field {field:?}")]
    MissingComponentField { instance: String, port: String, epoch_ns: i64, field: String },
    #[error("BatchingRule::PerNMeasurements(0) is not a valid batching rule -- there is no sensible way to chunk into runs of zero")]
    ZeroSizedBatchingRule,
    #[error("port traffic log hash mismatch: expected {expected}, computed {computed} -- refusing to decode unverified bytes")]
    PortTrafficHashMismatch { expected: String, computed: String },
    #[error("port traffic log bytes did not decode as altavista.v1.PortTrafficLog: {0}")]
    PortTrafficDecode(String),
    /// `crate::sign::SigningError`, stringified rather than wrapped with `#[from]`: that type
    /// implements neither `Clone` nor `PartialEq` (it carries an `openssl::error::ErrorStack`
    /// path, exactly like `crate::sign::SigningError::Openssl`'s own doc comment explains for
    /// itself), and this crate's own convention (`crate::sign::SigningError::Openssl`'s
    /// identical choice) is to stringify rather than derive those traits onto E1's own error
    /// type just to satisfy a caller two modules away.
    #[error("signing failed: {0}")]
    Signing(String),
}

impl From<SigningError> for PluginError {
    fn from(e: SigningError) -> Self {
        PluginError::Signing(e.to_string())
    }
}

// -----------------------------------------------------------------------------------------
// PluginConfig
// -----------------------------------------------------------------------------------------

/// One replay policy `crate::plugin::BatchBuilder` applies to turn a source's groups into
/// batches -- explicit and configurable, never implicit in `BatchBuilder`'s own code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BatchingRule {
    /// One `MeasurementBatch` per [`MeasurementSource`] group, unchanged: `batch_tai_ns` is
    /// exactly that group's own epoch. This milestone's own fixture (`drms/
    /// demo_ground_segment*.yaml`) records exactly one measurement per step, so this and
    /// `PerNMeasurements(1)` are equivalent for it -- the distinction matters for a future
    /// source whose steps carry more than one measurement each.
    PerEpoch,
    /// Flattens every group's measurements into overall (already epoch-ordered) sequence and
    /// re-chunks them into runs of exactly this many measurements (the last chunk short if the
    /// total does not divide evenly); `batch_tai_ns` is each chunk's own last measurement's
    /// `epoch_ns` (the batch is considered emitted once its final measurement is known). `0`
    /// is refused ([`PluginError::ZeroSizedBatchingRule`]) rather than silently treated as
    /// "one batch holding nothing" or panicking on `chunks(0)`.
    PerNMeasurements(usize),
}

impl BatchingRule {
    fn chunk(&self, groups: &[(i64, Vec<pb::Measurement>)]) -> Result<Vec<(i64, Vec<pb::Measurement>)>, PluginError> {
        match self {
            BatchingRule::PerEpoch => Ok(groups.to_vec()),
            BatchingRule::PerNMeasurements(0) => Err(PluginError::ZeroSizedBatchingRule),
            BatchingRule::PerNMeasurements(n) => {
                let all: Vec<pb::Measurement> = groups.iter().flat_map(|(_, ms)| ms.iter().cloned()).collect();
                Ok(all.chunks(*n).map(|chunk| (chunk.last().map(|m| m.epoch_ns).unwrap_or(0), chunk.to_vec())).collect())
            }
        }
    }
}

/// A plugin's declared pacing: how a binary should space out sending the batches
/// [`BatchBuilder`] built, expressed as the pure decision [`Pacing::due_at`] -- **never**
/// as a sleep anywhere in this crate (question 199's sibling rule for this track,
/// `crate::chain`'s module doc's identical framing for "no clock read here" -- this is its
/// "no sleep here" counterpart).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Pacing {
    /// Send every batch back to back, with no pacing at all.
    AsFastAsPossible,
    /// Space batches out to reproduce the recorded epochs' own real-time cadence, scaled by
    /// `scale` (`1.0` = original speed, `2.0` = twice as fast, `0.5` = half speed). `scale`
    /// must be strictly positive; [`Pacing::due_at`] does not itself check this (it is a pure
    /// function, not a constructor), but a `PluginConfig` carrying a non-positive scale is
    /// refused by [`PluginConfig::validate`].
    RealTime { scale: f64 },
}

impl Pacing {
    /// The nanosecond offset, **relative to whenever the caller considers replay to have
    /// started**, at which the batch produced from `epoch_tai_ns` (the `batch_index`'th batch
    /// overall; `first_epoch_tai_ns` is the very first batch's own epoch) is due to be sent.
    ///
    /// A **pure** function of its four arguments -- no clock read, no sleep, callable from a
    /// test with no wall-clock dependency at all. The caller (a plugin binary, never this
    /// library) is responsible for turning this offset into an actual wait: record its own
    /// `Instant::now()` once at replay start, and before sending batch `i` sleep until
    /// `start + Duration::from_nanos(due_at(i, first_epoch, epoch) as u64)` -- exactly the
    /// "the library may not \[sleep\]; the binary may" split this crate's task brief states.
    ///
    /// [`Pacing::AsFastAsPossible`] returns `0` for every batch (always immediately due).
    /// [`Pacing::RealTime`] returns `(epoch_tai_ns - first_epoch_tai_ns) / scale`, so a
    /// `scale` of `1.0` reproduces the batches' own original cadence and `2.0` replays twice
    /// as fast. `batch_index` is accepted but unused by either policy today -- kept in the
    /// signature so a future policy (e.g. "at least N ms between batches regardless of how far
    /// apart their epochs are") can use it without changing every call site.
    pub fn due_at(&self, batch_index: usize, first_epoch_tai_ns: i64, epoch_tai_ns: i64) -> i64 {
        let _ = batch_index;
        match self {
            Pacing::AsFastAsPossible => 0,
            Pacing::RealTime { scale } => {
                let elapsed_ns = epoch_tai_ns.saturating_sub(first_epoch_tai_ns);
                ((elapsed_ns as f64) / scale).round() as i64
            }
        }
    }
}

/// Every declared knob this plugin's own behaviour depends on -- "declared configuration,
/// not a hard-coded constant" (this crate's task brief, verbatim). Every field is a plain,
/// `serde`-serialisable type (never a raw `prost`-generated type directly, since those do not
/// derive `Serialize` -- `codec_bytes`/`label_bytes` below are each a declared message's own
/// canonical `prost::Message::encode_to_vec` bytes, decoded back on demand by
/// [`PluginConfig::codec`]/[`PluginConfig::label`]), so a whole `PluginConfig` can be
/// serialised for provenance/debugging and content-hashed
/// ([`PluginConfig::config_hash`]) -- what a run records it replayed under
/// (`PluginConfig::batch_provenance`'s own `"plugin_config_hash"` attribute).
///
/// Deliberately plain public fields, not a many-argument constructor function: a
/// configuration this large (sixteen independent knobs) would trip `clippy::
/// too_many_arguments` as a function signature, and a builder adds indirection this task's
/// own tests do not need -- every caller (a test, or `av-edge-plugin`'s own `main`) builds
/// one with an ordinary struct literal and then calls [`PluginConfig::validate`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginConfig {
    /// `MeasurementBatch.producer_id` / `PluginManifest.producer_id`.
    pub producer_id: String,
    /// `PluginManifest.plugin_version`.
    pub plugin_version: String,
    /// `PortTrafficRecord.instance` this source reads from.
    pub instance: String,
    /// `PortTrafficRecord.port` this source reads from.
    pub port: String,
    /// `PortTrafficRecord.direction` (`pb::PortDirection` as `i32`) this source reads --
    /// `PORT_DIRECTION_OUT` for this milestone's own fixture (`crate`'s own module doc: only
    /// OUT frames are ever the emitting instance's own telemetry).
    pub direction: i32,
    /// The declared `PacketCodec`'s own canonical `prost::Message::encode_to_vec` bytes
    /// (`PluginConfig::encode_codec` builds this from a real `pb::PacketCodec`;
    /// [`PluginConfig::codec`] decodes it back).
    pub codec_bytes: Vec<u8>,
    /// The ordered `PacketField.name`s that make up one `Measurement.z`, e.g. `["x", "y",
    /// "z"]` for this milestone's own fixture. Order matters: `z[i]` is always
    /// `component_fields[i]`'s own decoded value.
    pub component_fields: Vec<String>,
    /// `Measurement.frame_id`.
    pub frame_id: String,
    /// `Measurement.sensor_id`.
    pub sensor_id: String,
    /// `Measurement.measurement_id`.
    pub measurement_id: String,
    /// `Measurement.shard_key` / `MeasurementBatch.shard_key` (both must agree, per
    /// `edge.proto`'s own `BATCH_REJECTION_SHARD_MISMATCH` rule -- [`BatchBuilder`] sets both
    /// from this one field so they never can disagree).
    pub shard_key: String,
    /// `Measurement.r`: the declared measurement-noise covariance, row-major, SPD, exactly
    /// `component_fields.len()^2` entries. Checked via
    /// [`av_cdm::covariance::check_spd_row_major`] at [`PluginConfig::validate`] time --
    /// never shipped un-checked (`av_codec::measurements_from_field_values`'s own
    /// identical rule).
    pub noise_r: Vec<f64>,
    /// The declared `Label`'s own canonical `prost::Message::encode_to_vec` bytes.
    pub label_bytes: Vec<u8>,
    /// `PluginManifest.clearance`.
    pub clearance: String,
    /// `PluginManifest.leaf_fingerprint_sha256` / `sign::sign_batch_with_signer`'s own
    /// `fingerprint_sha256` argument -- the *same* value goes on both the manifest
    /// ([`PluginConfig::manifest`]) and every batch this config signs
    /// ([`BatchBuilder::build_batches`]), so the two can never drift apart. Empty for E1's own
    /// no-certificate-in-the-loop path.
    pub leaf_fingerprint_sha256: String,
    /// [`BatchBuilder`]'s grouping rule.
    pub batching: BatchingRule,
    /// The pacing a plugin binary applies between batches ([`Pacing::due_at`]) -- carried on
    /// this config so it is recorded (and hashed) alongside every other replay decision, even
    /// though this library itself never reads a clock or sleeps.
    pub pacing: Pacing,
}

impl PluginConfig {
    /// [`PluginConfig::codec_bytes`]'s own canonical encoding, from a real `pb::PacketCodec`.
    pub fn encode_codec(codec: &pb::PacketCodec) -> Vec<u8> {
        prost::Message::encode_to_vec(codec)
    }

    /// [`PluginConfig::label_bytes`]'s own canonical encoding, from a real `pb::Label`.
    pub fn encode_label(label: &pb::Label) -> Vec<u8> {
        prost::Message::encode_to_vec(label)
    }

    /// Decodes [`PluginConfig::codec_bytes`] back into a `pb::PacketCodec`.
    pub fn codec(&self) -> Result<pb::PacketCodec, PluginError> {
        <pb::PacketCodec as prost::Message>::decode(self.codec_bytes.as_slice()).map_err(|e| PluginError::CodecDecode(e.to_string()))
    }

    /// Decodes [`PluginConfig::label_bytes`] back into a `pb::Label`.
    pub fn label(&self) -> Result<pb::Label, PluginError> {
        <pb::Label as prost::Message>::decode(self.label_bytes.as_slice()).map_err(|e| PluginError::LabelDecode(e.to_string()))
    }

    /// Refuses an obviously-broken configuration up front, before it is ever handed to
    /// [`PortTrafficSource::from_log`] or [`BatchBuilder`]: every declared identifier
    /// non-empty, `component_fields` non-empty, `noise_r`'s shape SPD-checked against
    /// `component_fields.len()`, both embedded messages decode, and a `PerNMeasurements(0)`
    /// batching rule or a non-positive `RealTime` scale refused.
    pub fn validate(&self) -> Result<(), PluginError> {
        for (name, value) in [
            ("producer_id", &self.producer_id),
            ("plugin_version", &self.plugin_version),
            ("instance", &self.instance),
            ("port", &self.port),
            ("frame_id", &self.frame_id),
            ("sensor_id", &self.sensor_id),
            ("measurement_id", &self.measurement_id),
            ("shard_key", &self.shard_key),
            ("clearance", &self.clearance),
        ] {
            if value.is_empty() {
                return Err(PluginError::EmptyField { field: name });
            }
        }
        if self.component_fields.is_empty() {
            return Err(PluginError::EmptyComponentFields);
        }
        let n = self.component_fields.len();
        if self.noise_r.len() != n * n {
            return Err(PluginError::NoiseDimensionMismatch { expected: n * n, actual: self.noise_r.len(), n });
        }
        av_cdm::covariance::check_spd_row_major(&self.noise_r, n, &format!("PluginConfig({:?}).noise_r", self.measurement_id)).map_err(|e| PluginError::NoiseNotSpd(e.to_string()))?;
        self.codec()?;
        self.label()?;
        if let BatchingRule::PerNMeasurements(0) = self.batching {
            return Err(PluginError::ZeroSizedBatchingRule);
        }
        if let Pacing::RealTime { scale } = self.pacing {
            // `!scale.is_finite() || scale <= 0.0`, not `!(scale > 0.0)` --
            // clippy::neg_cmp_op_on_partial_ord refuses negating a comparison operator
            // directly on a partially-ordered type (f64: NaN makes `!(a > b)` and `a <=
            // b` genuinely different claims). This refuses non-finite (NaN/+-inf) and
            // non-positive scales alike, without negating a `>`/`<`/`>=`/`<=` expression.
            if !scale.is_finite() || scale <= 0.0 {
                return Err(PluginError::EmptyField { field: "pacing.scale (must be a positive, finite number)" });
            }
        }
        Ok(())
    }

    /// A SHA-256 content hash of this entire configuration (via the system OpenSSL, ADR-004
    /// -- never `sha2`/`ring`), over its own canonical JSON encoding
    /// (`serde_json::to_vec`). Deterministic because every field is a plain type with no
    /// `HashMap` anywhere in this struct (structs serialise fields in declaration order;
    /// every collection here is a `Vec`, which preserves its own order) -- this is what
    /// [`PluginConfig::batch_provenance`] records so a run's provenance names *exactly* what
    /// it replayed under, not merely that it replayed something.
    pub fn config_hash(&self) -> [u8; 32] {
        let json = serde_json::to_vec(self).expect("PluginConfig serialises to JSON: every field is a plain, non-map type");
        openssl::sha::sha256(&json)
    }

    /// The `PluginManifest` this config declares -- built from the *same* `PluginConfig` a
    /// [`BatchBuilder`] signs batches from, so the manifest a plugin announces and the
    /// batches it actually sends cannot drift apart (this crate's task brief, verbatim).
    pub fn manifest(&self) -> Result<pb::PluginManifest, PluginError> {
        let label = self.label()?;
        Ok(pb::PluginManifest {
            producer_id: self.producer_id.clone(),
            plugin_version: self.plugin_version.clone(),
            output_schemas: vec![pb::MeasurementSchema { measurement_id: self.measurement_id.clone(), sensor_id: self.sensor_id.clone(), z_len: self.component_fields.len() as u32 }],
            frame_ids: vec![self.frame_id.clone()],
            label: Some(label),
            clearance: self.clearance.clone(),
            shard_keys: vec![self.shard_key.clone()],
            leaf_fingerprint_sha256: self.leaf_fingerprint_sha256.clone(),
        })
    }

    /// The `Provenance` every batch this config signs carries (`MeasurementBatch.
    /// provenance`): `run_id` and `created_tai_ns` are the caller's own (a run id and an
    /// epoch this library never invents), `tool` names this plugin and its version, and
    /// `attributes["plugin_config_hash"]` is [`PluginConfig::config_hash`]'s own hex encoding
    /// -- "a run records what it replayed under" (this crate's task brief), directly on the
    /// signed artifact itself, not only in an external log line.
    pub fn batch_provenance(&self, run_id: &str, created_tai_ns: i64) -> pb::Provenance {
        let mut attributes = BTreeMap::new();
        attributes.insert("plugin_config_hash".to_string(), hash::hex_encode(&self.config_hash()));
        pb::Provenance {
            author_kind: pb::AuthorKind::External as i32,
            principal: format!("plugin:{}", self.producer_id),
            tool: format!("av-edge-plugin {}", self.plugin_version),
            created_tai_ns,
            run_id: run_id.to_string(),
            attributes,
            ..Default::default()
        }
    }
}

// -----------------------------------------------------------------------------------------
// MeasurementSource / PortTrafficSource
// -----------------------------------------------------------------------------------------

/// A source of measurement groups, in ascending epoch order -- what [`BatchBuilder`] reads
/// from. Deliberately a trait over an in-memory slice, not an iterator or a stream: every
/// value this milestone's own `PortTrafficLog` sidecar could ever produce comfortably fits
/// in memory (question 175's own sizing note is about the *log*, tens of megabytes for a
/// long run, not about one instance's own decoded measurements), and a slice is what lets
/// [`BatchBuilder::build_batches`] re-chunk across group boundaries for
/// [`BatchingRule::PerNMeasurements`] without re-deriving anything from the source itself.
pub trait MeasurementSource {
    /// Every group this source produces, `(epoch_tai_ns, measurements)`, sorted ascending by
    /// `epoch_tai_ns`. A pure accessor -- no I/O, no network, no clock read; whatever building
    /// the source needed (reading and hash-verifying a `PortTrafficLog`, decoding packets)
    /// already happened in the source's own constructor.
    fn groups(&self) -> &[(i64, Vec<pb::Measurement>)];
}

/// This milestone's one [`MeasurementSource`]: a run's `PortTrafficLog` sidecar, decoded
/// through one declared `PacketCodec` for one `(instance, port, direction)`. See `crate::
/// plugin`'s own module doc for why this, and not `RunProducts.measurements`, is this DRM's
/// only recoverable telemetry source.
#[derive(Debug, Clone)]
pub struct PortTrafficSource {
    groups: Vec<(i64, Vec<pb::Measurement>)>,
}

impl PortTrafficSource {
    /// Decodes every `log` record matching `config.instance`/`config.port`/`config.direction`
    /// through `config`'s own declared codec, building one `Measurement` per record (`z` in
    /// `config.component_fields`'s own declared order, `r` = `config.noise_r` verbatim, at
    /// the record's own `tai_ns` as `epoch_ns`) and grouping by epoch (a `BTreeMap` keyed by
    /// `tai_ns`, so two records recorded at the identical epoch -- not expected for this
    /// milestone's own one-packet-per-step fixture, but not assumed away either -- land in
    /// one group rather than two, and the result is deterministic regardless of `log.records`'
    /// own incoming order).
    ///
    /// Every field this needs comes from `config` -- `crate`'s own module doc's "nothing here
    /// is a hard-coded constant" restated concretely: change `config.instance`/`.port` and
    /// this same function replays a different instance's telemetry with no code change.
    pub fn from_log(log: &pb::PortTrafficLog, config: &PluginConfig) -> Result<Self, PluginError> {
        config.validate()?;
        let codec = config.codec()?;
        let mut by_epoch: BTreeMap<i64, Vec<pb::Measurement>> = BTreeMap::new();
        for record in &log.records {
            if record.instance != config.instance || record.port != config.port || record.direction != config.direction {
                continue;
            }
            let fields = packet::decode_numeric_fields(&codec, &record.payload)?;
            let mut z = Vec::with_capacity(config.component_fields.len());
            for name in &config.component_fields {
                let value = fields
                    .get(name)
                    .copied()
                    .ok_or_else(|| PluginError::MissingComponentField { instance: record.instance.clone(), port: record.port.clone(), epoch_ns: record.tai_ns, field: name.clone() })?;
                z.push(value);
            }
            let measurement = pb::Measurement {
                measurement_id: config.measurement_id.clone(),
                z,
                r: config.noise_r.clone(),
                epoch_ns: record.tai_ns,
                sensor_id: config.sensor_id.clone(),
                shard_key: config.shard_key.clone(),
                meta: BTreeMap::new(),
                frame_id: config.frame_id.clone(),
                entity_hint: String::new(),
            };
            by_epoch.entry(record.tai_ns).or_default().push(measurement);
        }
        Ok(Self { groups: by_epoch.into_iter().collect() })
    }
}

impl MeasurementSource for PortTrafficSource {
    fn groups(&self) -> &[(i64, Vec<pb::Measurement>)] {
        &self.groups
    }
}

/// Hash-verifies `log_bytes` against `expected_hash_hex` (SHA-256 via the system OpenSSL,
/// `openssl::sha::sha256` -- ADR-004's crypto rule, never `sha2`) **before** ever decoding
/// them, mirroring `av_kernel::drm::replay::verify_and_load`'s own load-time contract but
/// through this crate's own hashing primitive rather than `av-kernel`'s (this crate still
/// depends on neither `av-kernel` nor `gmat-sys`/transport -- only on `av-codec`, question
/// 205's GMAT-free extraction; see `crate::plugin::packet`'s module doc). Pure: takes
/// already-read bytes, never touches a filesystem itself -- the plugin binary is what reads
/// the sidecar's bytes off disk.
pub fn verify_port_traffic_log(log_bytes: &[u8], expected_hash_hex: &str) -> Result<pb::PortTrafficLog, PluginError> {
    let computed = hash::hex_encode(&openssl::sha::sha256(log_bytes));
    if computed != expected_hash_hex {
        return Err(PluginError::PortTrafficHashMismatch { expected: expected_hash_hex.to_string(), computed });
    }
    <pb::PortTrafficLog as prost::Message>::decode(log_bytes).map_err(|e| PluginError::PortTrafficDecode(e.to_string()))
}

// -----------------------------------------------------------------------------------------
// BatchBuilder
// -----------------------------------------------------------------------------------------

/// Turns a [`MeasurementSource`]'s groups into signed, chained `MeasurementBatch`es, one
/// producer's chain at a time, starting from [`hash::GENESIS`] -- the "plugin library" half
/// of this milestone (`crate::plugin`'s own module doc).
#[derive(Debug, Clone, Copy)]
pub struct BatchBuilder {
    batching: BatchingRule,
}

/// The four per-producer identity fields every batch [`BatchBuilder::build_batches`]
/// produces carries verbatim -- bundled into one value rather than four separate
/// parameters so that function stays under `clippy::too_many_arguments`'s limit (a plain
/// borrowed view, not an owned copy of anything in [`PluginConfig`]: [`BatchIdentity::
/// from_config`] borrows straight from a `PluginConfig` plus its already-decoded
/// `Label`).
#[derive(Debug, Clone, Copy)]
pub struct BatchIdentity<'a> {
    pub producer_id: &'a str,
    pub label: &'a pb::Label,
    pub shard_key: &'a str,
    pub fingerprint_sha256: &'a str,
}

impl<'a> BatchIdentity<'a> {
    /// Borrows `producer_id`/`shard_key`/`leaf_fingerprint_sha256` straight from `config`,
    /// and `label` from `decoded_label` (the caller's own already-decoded `config.label()`
    /// -- kept as a separate parameter rather than decoded again here, since decoding can
    /// fail and this constructor is infallible by design).
    pub fn from_config(config: &'a PluginConfig, decoded_label: &'a pb::Label) -> Self {
        Self { producer_id: &config.producer_id, label: decoded_label, shard_key: &config.shard_key, fingerprint_sha256: &config.leaf_fingerprint_sha256 }
    }
}

impl BatchBuilder {
    /// Refuses `PerNMeasurements(0)` immediately (`PluginError::ZeroSizedBatchingRule`) --
    /// the same check [`PluginConfig::validate`] already runs, repeated here so a
    /// `BatchBuilder` built directly (not through a `PluginConfig`, e.g. in a unit test) still
    /// cannot be constructed with a rule that would panic later.
    pub fn new(batching: BatchingRule) -> Result<Self, PluginError> {
        if let BatchingRule::PerNMeasurements(0) = batching {
            return Err(PluginError::ZeroSizedBatchingRule);
        }
        Ok(Self { batching })
    }

    /// One producer's own chain (starting at `sequence = 1`, `prev_hash = GENESIS`), from
    /// `groups` (in epoch order, per [`MeasurementSource::groups`]'s own contract). Every
    /// batch carries `identity`'s four fields and `provenance` verbatim (the *same* values
    /// on every batch this call produces -- a single plugin process signs under one declared
    /// identity for its whole run) and is signed with `av_edge::sign::sign_batch_with_signer`
    /// (`crate::sign`), chained from the previous call's own `batch_hash` (or
    /// [`hash::GENESIS`] for the first).
    ///
    /// **Deterministic body bytes, non-deterministic signature bytes** -- calling this twice
    /// with the same `groups`/`identity`/`provenance`/`signing_key` produces batches whose
    /// `hash::canonical_body_bytes`/`batch_hash` are byte-identical both times, but whose
    /// `signature` differs (OpenSSL's ECDSA nonce is random -- `crate`'s own top-level module
    /// doc explains why a byte-pinned signature golden is not possible at all).
    /// `crates/av-edge/tests/plugin_replay.rs`'s own determinism test asserts exactly this
    /// split.
    pub fn build_batches(&self, groups: &[(i64, Vec<pb::Measurement>)], identity: &BatchIdentity<'_>, provenance: &pb::Provenance, signing_key: &EcKeyRef<Private>) -> Result<Vec<pb::MeasurementBatch>, PluginError> {
        let chunks = self.batching.chunk(groups)?;
        let mut batches = Vec::with_capacity(chunks.len());
        let mut prev_hash = hash::GENESIS.to_vec();
        for (index, (batch_tai_ns, measurements)) in chunks.into_iter().enumerate() {
            let mut batch = pb::MeasurementBatch {
                producer_id: identity.producer_id.to_string(),
                sequence: index as u64 + 1,
                label: Some(identity.label.clone()),
                measurements,
                provenance: Some(provenance.clone()),
                batch_tai_ns,
                shard_key: identity.shard_key.to_string(),
                ..Default::default()
            };
            sign::sign_batch_with_signer(&mut batch, &prev_hash, signing_key, identity.fingerprint_sha256)?;
            prev_hash = batch.batch_hash.clone();
            batches.push(batch);
        }
        Ok(batches)
    }

    /// [`BatchBuilder::build_batches`], building the [`BatchIdentity`] straight from
    /// `config` (via [`BatchIdentity::from_config`]) so a caller never has to keep those
    /// four values in sync with `config` by hand (they are exactly [`PluginConfig::
    /// manifest`]'s own inputs too -- "the manifest ... and the batches it sends cannot
    /// drift apart").
    pub fn build_batches_for_config(&self, source: &dyn MeasurementSource, config: &PluginConfig, provenance: &pb::Provenance, signing_key: &EcKeyRef<Private>) -> Result<Vec<pb::MeasurementBatch>, PluginError> {
        let label = config.label()?;
        let identity = BatchIdentity::from_config(config, &label);
        self.build_batches(source.groups(), &identity, provenance, signing_key)
    }
}
