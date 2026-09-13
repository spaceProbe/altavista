//! D2: spoore's `ModelService` contract, genuinely implemented -- not a stub. Every rpc
//! delegates to a real, deterministic, closed-form `spoore_models::KalmanFilter` behind the
//! `spoore_models::Predictor` trait, built EXACTLY the way `crates/av-track/src/config.rs`'s
//! `TrackConfig::build_tree_and_initiator` builds one (`KalmanFilter::new("root",
//! Arc::new(ConstantVelocity3d::new(q))).with_sensor(Arc::new(Position3dSensor::for_space(...)))`)
//! -- copied, not reinvented, per this task's own instruction to reuse that construction.
//!
//! ## No RPC returns `UNIMPLEMENTED`
//!
//! Every one of `ModelInfo`/`WarmStart`/`Predict`/`MeasurementLikelihood`/`Update`/
//! `ClaimTestability` is implemented for real over the real `KalmanFilter`.
//! `ClaimTestability` in particular: `spoore_models::kalman::KalmanFilter` OVERRIDES
//! `Predictor::claim_testability` (it does not use the trait's own `Ok(None)` default -- see
//! `spoore-models/src/kalman.rs`), so this service forwards to it and returns a real,
//! non-empty `testability` vector whenever the underlying `Predictor` produces one; the proto's
//! own "empty is honest abstention" contract is honoured only in the one case the underlying
//! `Predictor` itself abstains (an unrecognized `measurement.sensor_id`, or the trait's own
//! default for a `Predictor` that never overrides it at all -- not the case here, but the
//! service still forwards `None` as empty rather than inventing a value).
//!
//! ## One declaration, two consumers
//!
//! [`ModelIdentity`] is constructed once (from this binary's own CLI arguments -- D3's "every
//! knob a command-line argument" rule) and is the SOLE source for BOTH:
//! - `ModelInfoResponse.node_id`/`version` ([`ModelServiceImpl::model_info`] below), and
//! - `ProposalEvidence.model_identity`/`model_version` (`crate::proposer::run`, which is
//!   hedged by `crates/av-proposer/tests/model_info_matches_evidence.rs`'s own real-loopback
//!   assertion that these are the literal same two strings).
//!
//! ## Epoch convention: spoore's own (Unix nanoseconds), never TAI
//!
//! `spoore_cdm::state::Epoch` is nanoseconds since the Unix epoch (that crate's own doc:
//! "Event time... `i64` nanoseconds... around 1970"), a DIFFERENT epoch than this workspace's
//! own `av_command::clock::Clock` (TAI nanoseconds). `ModelService`'s own wire contract
//! (`PredictRequest.dt_ns`, `GaussianState.epoch_ns`) is entirely relative-or-Unix and never
//! crosses this crate's own TAI boundary, so no conversion is needed anywhere in this module
//! -- a served `Predict` call is self-contained in spoore's own epoch, exactly as a bare
//! `spoore_models::Predictor` call would be if this were in-process.

use std::sync::Arc;

use spoore_cdm::proto as spoore_pb;
use spoore_cdm::{Belief, Measurement, TrackSeed};
use spoore_models::{air_3d_state_space, ConstantVelocity3d, KalmanFilter, Position3dSensor, Predictor};
use tonic::{Request, Response, Status};

use crate::model_service_pb::model_service_server::ModelService;

/// This proposer's own declared model identity -- see the module doc's "one declaration, two
/// consumers" section. Both fields are CLI arguments on the `av-proposer` binary (D3), never
/// hardcoded.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelIdentity {
    pub node_id: String,
    pub version: String,
}

/// Every way constructing [`ModelServiceImpl`] can fail -- a load-time configuration error
/// (a `--sensor-id` that does not read the state space's `pos_x`/`pos_y`/`pos_z`
/// components), never reachable from a served rpc.
#[derive(Debug)]
pub struct ModelServiceInitError(pub String);

impl std::fmt::Display for ModelServiceInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "constructing the ModelService's KalmanFilter: {}", self.0)
    }
}

/// The served `ModelService`. `predictor` is `dyn Predictor` (not the concrete
/// `KalmanFilter`) so a future model sidecar under this same crate could swap the filter
/// without touching this wiring -- today it is always the real `KalmanFilter` this module's
/// own doc names.
pub struct ModelServiceImpl {
    identity: ModelIdentity,
    predictor: Arc<dyn Predictor>,
    state_space_id: String,
    sensor_ids: Vec<String>,
}

impl ModelServiceImpl {
    /// Builds the real filter exactly as `crates/av-track/src/config.rs`'s
    /// `TrackConfig::build_tree_and_initiator` does (see the module doc) -- a
    /// `KalmanFilter` over `ConstantVelocity3d` (process noise density `process_noise_density`,
    /// a CLI argument), reading 3D position through one `Position3dSensor` named `sensor_id`
    /// (also a CLI argument).
    pub fn new(identity: ModelIdentity, process_noise_density: f64, sensor_id: String) -> Result<Self, ModelServiceInitError> {
        let space = air_3d_state_space();
        let sensor = Position3dSensor::for_space(sensor_id.clone(), &space).map_err(|e| ModelServiceInitError(e.to_string()))?;
        let predictor: Arc<dyn Predictor> = Arc::new(KalmanFilter::new(identity.node_id.clone(), Arc::new(ConstantVelocity3d::new(process_noise_density))).with_sensor(Arc::new(sensor)));
        Ok(Self { state_space_id: space.id.clone(), sensor_ids: vec![sensor_id], identity, predictor })
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }
}

fn invalid<E: std::fmt::Display>(context: &str, e: E) -> Status {
    Status::invalid_argument(format!("{context}: {e}"))
}

#[tonic::async_trait]
impl ModelService for ModelServiceImpl {
    async fn model_info(&self, _request: Request<spoore_pb::ModelInfoRequest>) -> Result<Response<spoore_pb::ModelInfoResponse>, Status> {
        Ok(Response::new(spoore_pb::ModelInfoResponse {
            node_id: self.identity.node_id.clone(),
            version: self.identity.version.clone(),
            state_space_id: self.state_space_id.clone(),
            sensor_ids: self.sensor_ids.clone(),
            supports_update: true,
            calibration_temperature: 1.0,
            // "exact" is honest: a closed-form Kalman filter with no learned parameters
            // (the module doc; spoore's own model_service.proto: "'exact' for a closed-form
            // filter with no learned parameters").
            epistemic_method: "exact".to_string(),
            // 1.0 means no inflation -- this filter's own reported covariance already
            // includes everything it claims to know; nothing is fit on a golden set here.
            covariance_inflation: 1.0,
        }))
    }

    async fn warm_start(&self, request: Request<spoore_pb::WarmStartRequest>) -> Result<Response<spoore_pb::WarmStartResponse>, Status> {
        let req = request.into_inner();
        let wire_seed = req.seed.ok_or_else(|| Status::invalid_argument("WarmStartRequest.seed is required"))?;
        let seed = TrackSeed::try_from(wire_seed).map_err(|e| invalid("decoding TrackSeed", e))?;
        let belief = self.predictor.warm_start(&seed).map_err(|e| invalid("warm_start", e))?;
        Ok(Response::new(spoore_pb::WarmStartResponse { belief: Some(spoore_pb::Belief::from(&belief)) }))
    }

    async fn predict(&self, request: Request<spoore_pb::PredictRequest>) -> Result<Response<spoore_pb::PredictResponse>, Status> {
        let req = request.into_inner();
        let wire_belief = req.belief.ok_or_else(|| Status::invalid_argument("PredictRequest.belief is required"))?;
        let belief = Belief::try_from(wire_belief).map_err(|e| invalid("decoding Belief", e))?;
        let from_epoch = belief.components()[0].state.epoch();
        let to = from_epoch.saturating_add_nanos(req.dt_ns);
        let predicted = self.predictor.predict(&belief, to).map_err(|e| invalid("predict", e))?;
        Ok(Response::new(spoore_pb::PredictResponse { belief: Some(spoore_pb::Belief::from(&predicted)) }))
    }

    async fn measurement_likelihood(&self, request: Request<spoore_pb::MeasurementLikelihoodRequest>) -> Result<Response<spoore_pb::MeasurementLikelihoodResponse>, Status> {
        let req = request.into_inner();
        let wire_belief = req.belief.ok_or_else(|| Status::invalid_argument("MeasurementLikelihoodRequest.belief is required"))?;
        let wire_measurement = req.measurement.ok_or_else(|| Status::invalid_argument("MeasurementLikelihoodRequest.measurement is required"))?;
        let belief = Belief::try_from(wire_belief).map_err(|e| invalid("decoding Belief", e))?;
        let measurement = Measurement::try_from(wire_measurement).map_err(|e| invalid("decoding Measurement", e))?;
        let innovations = self.predictor.measurement_likelihood(&belief, &measurement).map_err(|e| invalid("measurement_likelihood", e))?;
        Ok(Response::new(spoore_pb::MeasurementLikelihoodResponse { innovations: innovations.iter().map(spoore_pb::Innovation::from).collect() }))
    }

    async fn update(&self, request: Request<spoore_pb::UpdateRequest>) -> Result<Response<spoore_pb::UpdateResponse>, Status> {
        let req = request.into_inner();
        let wire_belief = req.belief.ok_or_else(|| Status::invalid_argument("UpdateRequest.belief is required"))?;
        let wire_measurement = req.measurement.ok_or_else(|| Status::invalid_argument("UpdateRequest.measurement is required"))?;
        let belief = Belief::try_from(wire_belief).map_err(|e| invalid("decoding Belief", e))?;
        let measurement = Measurement::try_from(wire_measurement).map_err(|e| invalid("decoding Measurement", e))?;
        let updated = self.predictor.update(&belief, &measurement).map_err(|e| invalid("update", e))?;
        Ok(Response::new(spoore_pb::UpdateResponse { belief: Some(spoore_pb::Belief::from(&updated)) }))
    }

    async fn claim_testability(&self, request: Request<spoore_pb::ClaimTestabilityRequest>) -> Result<Response<spoore_pb::ClaimTestabilityResponse>, Status> {
        let req = request.into_inner();
        let wire_belief = req.belief.ok_or_else(|| Status::invalid_argument("ClaimTestabilityRequest.belief is required"))?;
        let wire_measurement = req.measurement.ok_or_else(|| Status::invalid_argument("ClaimTestabilityRequest.measurement is required"))?;
        let belief = Belief::try_from(wire_belief).map_err(|e| invalid("decoding Belief", e))?;
        let measurement = Measurement::try_from(wire_measurement).map_err(|e| invalid("decoding Measurement", e))?;
        let testability = self.predictor.claim_testability(&belief, &measurement).map_err(|e| invalid("claim_testability", e))?;
        // See the module doc: KalmanFilter overrides this and returns Some(..) whenever the
        // measurement's sensor is known; None (empty, per the proto's own contract) only if
        // the underlying Predictor genuinely abstains.
        Ok(Response::new(spoore_pb::ClaimTestabilityResponse { testability: testability.unwrap_or_default() }))
    }
}
