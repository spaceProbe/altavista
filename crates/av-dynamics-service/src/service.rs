//! `altavista.v1.DynamicsService`, hosted over `gmat-sys`/`av-dynamics` (ADR-002 depth 2,
//! `"gmat-ffi"`). See `crate::worker`'s module doc for the GMAT single-thread contract
//! every RPC below honours: every GMAT-touching closure runs through
//! `WorkerHandle::run` (dispatched to the one dedicated GMAT worker thread), never
//! directly on the tokio thread this async method happens to be polled from.
//!
//! Mirrors `services/gmat-service/gmat_service/service.py`'s RPC-by-RPC behaviour (error
//! codes, validation order, the evidence-log call after every success) at this crate's own
//! ADR-002 depth. Two differences from that service are deliberate, not bugs -- see
//! `crate::config`'s module doc and this crate's README: `dynamics_depth = "gmat-ffi"`
//! (not `"gmat-api"`), and a different `settings_hash` (this server integrates with its
//! own `Dopri5`, not GMAT's native `PrinceDormand78` propagator).
use std::sync::Arc;

use tonic::{Request, Response, Status};

use av_cdm::pb::{
    AuthorKind, DerivativesRequest, DerivativesResponse, DescribeRequest, Interpolation, ModelInfo,
    PropagateRequest, PropagateResponse, Provenance, SolveRequest, SolveResponse, StateVector, StepRequest,
    StepResponse, Trajectory, TrajectorySample, TrajectorySegment,
};
use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;

use crate::config;
use crate::evidence::EvidenceLog;
use crate::pb::dynamics_service_server::DynamicsService;
use crate::propagate::{self, PropagateError};
use crate::worker::WorkerHandle;

pub struct DynamicsServiceImpl {
    worker: WorkerHandle,
    evidence: Arc<EvidenceLog>,
    run_id: String,
}

impl DynamicsServiceImpl {
    pub fn new(worker: WorkerHandle, evidence: Arc<EvidenceLog>, run_id: String) -> Self {
        Self { worker, evidence, run_id }
    }

    fn record<Req: prost::Message, Resp: prost::Message>(&self, method: &str, request: &Req, response: &Resp) {
        self.evidence.record(method, request, response, &self.worker.settings_hash, &self.run_id);
    }

    /// TAI nanoseconds "now" (wall clock), for `Provenance.created_tai_ns` only -- the
    /// artifact's own creation timestamp, not a physics result (ADR-004/ADR-001's "no wall
    /// clock affecting results" binds the *dynamics*, not the provenance envelope every
    /// artifact carries; `gmat_service.service.DynamicsServiceServicer._created_tai_ns`
    /// makes the identical call for the identical reason).
    fn created_tai_ns() -> i64 {
        let utc_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_nanos() as i64;
        Tai::from_utc_nanos(utc_ns).as_nanos()
    }
}

#[tonic::async_trait]
impl DynamicsService for DynamicsServiceImpl {
    async fn describe(&self, request: Request<DescribeRequest>) -> Result<Response<ModelInfo>, Status> {
        let req = request.into_inner();
        if !req.model_id.is_empty() && req.model_id != config::MODEL_ID {
            return Err(Status::not_found(format!(
                "unknown model_id {:?}; this server hosts {:?}",
                req.model_id, config::MODEL_ID
            )));
        }
        // Precomputed once at warm-up (crate::worker::Worker::build) -- no worker-thread
        // round trip needed for a fixed, already-known answer.
        let response = (*self.worker.model_info).clone();
        self.record("Describe", &req, &response);
        Ok(Response::new(response))
    }

    async fn derivatives(&self, request: Request<DerivativesRequest>) -> Result<Response<DerivativesResponse>, Status> {
        let req = request.into_inner();
        if !req.controls.is_empty() {
            return Err(Status::invalid_argument(format!(
                "{} declares no ControlComponents (no actuation force is in the force model); got {} control value(s)",
                config::MODEL_ID,
                req.controls.len()
            )));
        }
        let state = req.state.clone().ok_or_else(|| Status::invalid_argument("state is required"))?;
        if state.state.len() < 6 {
            return Err(Status::invalid_argument(format!(
                "state vector needs 6 components (pos xyz, vel xyz); got {}",
                state.state.len()
            )));
        }
        let state6: [f64; 6] = state.state[0..6].try_into().unwrap();
        let tai_ns = state.tai_ns;

        let dot = self
            .worker
            .run(move |w| {
                let mut out = [0.0; 6];
                w.model.derivatives(&state6, tai_ns, &[], &mut out).map(|_| out)
            })
            .await
            .map_err(|e| Status::internal(format!("GMAT derivatives failed: {e}")))?;

        let response = DerivativesResponse { state_dot: dot.to_vec(), jacobian: vec![] };
        self.record("Derivatives", &req, &response);
        Ok(Response::new(response))
    }

    async fn step(&self, request: Request<StepRequest>) -> Result<Response<StepResponse>, Status> {
        let req = request.into_inner();
        if !req.cov.is_empty() {
            return Err(Status::failed_precondition(
                "Step does not propagate an input covariance (StepRequest.cov). This \
                 server's STM path is wired into Propagate only (PropagateRequest.covariance \
                 / GaussianState.cov); Step's per-substep covariance propagation is not \
                 built. Use Propagate for covariance.",
            ));
        }
        if !req.controls.is_empty() {
            return Err(Status::invalid_argument(format!(
                "{} declares no ControlComponents (no actuation force is in the force model); got {} control value(s)",
                config::MODEL_ID,
                req.controls.len()
            )));
        }
        if req.dt_s <= 0.0 {
            return Err(Status::invalid_argument("dt_s must be positive"));
        }
        let state = req.state.clone().ok_or_else(|| Status::invalid_argument("state is required"))?;
        if state.state.len() < 6 {
            return Err(Status::invalid_argument(format!(
                "state vector needs 6 components (pos xyz, vel xyz); got {}",
                state.state.len()
            )));
        }
        let state6: [f64; 6] = state.state[0..6].try_into().unwrap();
        let tai_ns = state.tai_ns;
        let dt_ns = (req.dt_s * 1e9).round() as i64;

        let result = self
            .worker
            .run(move |w| w.model.step(&state6, tai_ns, &[], dt_ns))
            .await
            .map_err(|e| Status::internal(format!("GMAT step failed: {e}")))?;

        let response = StepResponse {
            state: Some(StateVector { state: result.state, tai_ns: result.t_tai_ns }),
            cov: vec![],
            outputs: Default::default(),
        };
        self.record("Step", &req, &response);
        Ok(Response::new(response))
    }

    async fn propagate(&self, request: Request<PropagateRequest>) -> Result<Response<PropagateResponse>, Status> {
        let req = request.into_inner();
        if !req.controls.is_empty() {
            return Err(Status::invalid_argument(format!(
                "{} does not implement ControlSegment schedules (no actuation force is in the force model); got {} segment(s)",
                config::MODEL_ID,
                req.controls.len()
            )));
        }
        if !req.impulses.is_empty() {
            return Err(Status::unimplemented(format!(
                "{}'s Propagate does not implement impulsive maneuvers; got {} impulse(s).",
                config::MODEL_ID,
                req.impulses.len()
            )));
        }
        if !req.output_frame_id.is_empty() && req.output_frame_id != config::FRAME_ID {
            return Err(Status::unimplemented(format!(
                "{} only propagates in {:?}; converting to output_frame_id={:?} needs a frame service, which is not hosted by av-dynamics-service",
                config::MODEL_ID,
                config::FRAME_ID,
                req.output_frame_id
            )));
        }
        let seed = req.seed.clone().ok_or_else(|| Status::invalid_argument("seed is required"))?;
        if seed.mean.len() < 6 {
            return Err(Status::invalid_argument(format!(
                "seed.mean needs >= 6 components (pos xyz, vel xyz); got {}",
                seed.mean.len()
            )));
        }
        if req.sample_interval_s <= 0.0 {
            return Err(Status::invalid_argument("sample_interval_s must be positive"));
        }
        if req.horizon_tai_ns <= seed.epoch_ns {
            return Err(Status::invalid_argument("horizon_tai_ns must be after seed.epoch_ns"));
        }

        let seed6: [f64; 6] = seed.mean[0..6].try_into().unwrap();
        let epochs = propagate::sample_epochs_ns(seed.epoch_ns, req.horizon_tai_ns, req.sample_interval_s);

        let samples = if req.covariance {
            if seed.cov.is_empty() {
                return Err(Status::invalid_argument(
                    "covariance=true requires seed.cov (a declared P0, row-major 6x6); got an \
                     empty covariance. Covariance is always explicitly requested, never a \
                     silent default -- there is no identity or zero P0 fallback.",
                ));
            }
            if seed.cov.len() != 36 {
                return Err(Status::invalid_argument(format!(
                    "seed.cov must be 36 elements (row-major 6x6); got {}",
                    seed.cov.len()
                )));
            }
            let p0 = seed.cov.clone();
            let epochs_for_job = epochs.clone();
            self.worker
                .run(move |w| propagate::run_with_covariance(&w.stm_model, seed6, &p0, &epochs_for_job))
                .await
                .map_err(|e| match e {
                    PropagateError::Model(ge) => Status::internal(format!("GMAT error: {ge}")),
                    PropagateError::CovarianceHygiene(he) => {
                        Status::failed_precondition(format!("propagated covariance failed the SPD hygiene check: {he}"))
                    }
                })?
        } else {
            let epochs_for_job = epochs.clone();
            self.worker
                .run(move |w| propagate::run_plain(&w.model, seed6, &epochs_for_job))
                .await
                .map_err(|e| Status::internal(format!("GMAT error: {e}")))?
        };

        let entity_id = if req.entity_id.is_empty() { "propagated".to_string() } else { req.entity_id.clone() };
        let settings_hash = (*self.worker.settings_hash).clone();

        let mut trajectory = Trajectory {
            id: entity_id.clone(),
            entity_id: entity_id.clone(),
            state_space_id: config::STATE_SPACE_ID.to_string(),
            frame_id: config::FRAME_ID.to_string(),
            interpolation: Interpolation::HermiteVelocity as i32,
            samples: Vec::with_capacity(samples.len()),
            segments: vec![],
            event_ids: vec![],
            label: None,
            provenance: None,
            config_hash: settings_hash.clone(),
        };
        for s in &samples {
            trajectory.samples.push(TrajectorySample {
                tai_ns: s.tai_ns,
                mean: s.mean.to_vec(),
                cov: s.cov.clone().unwrap_or_default(),
                // Question 116: `propagate::run_plain`/`run_with_covariance` step the model
                // directly to each requested epoch (`model.step(&state, t0, &[], t1 - t0)`) --
                // there is no coarser native grid this ever falls between, so every sample here
                // is on the model's own grid by construction.
                kind: av_cdm::pb::SampleKind::Native as i32,
            });
        }
        if let (Some(first), Some(last)) = (trajectory.samples.first(), trajectory.samples.last()) {
            trajectory.segments.push(TrajectorySegment {
                name: entity_id.clone(),
                start_tai_ns: first.tai_ns,
                end_tai_ns: last.tai_ns,
                dynamics_model: config::MODEL_ID.to_string(),
                dynamics_hash: settings_hash.clone(),
                // ADR-002 depth 2, not depth 1's "gmat-api" -- see this crate's README.
                dynamics_depth: "gmat-ffi".to_string(),
            });
        }
        trajectory.provenance = Some(Provenance {
            author_kind: AuthorKind::Service as i32,
            principal: String::new(),
            tool: "av-dynamics-service".to_string(),
            // This server has no DRM to hash a "configuration" out of; the force-model /
            // integrator settings ARE the whole configuration that produced this
            // trajectory (matches gmat_service.service.Propagate's identical reasoning).
            config_hash: settings_hash,
            data_pack_hash: String::new(),
            dataset_hash: String::new(),
            created_tai_ns: Self::created_tai_ns(),
            run_id: self.run_id.clone(),
            attributes: Default::default(),
        });

        let response = PropagateResponse { trajectory: Some(trajectory), events: vec![] };
        self.record("Propagate", &req, &response);
        Ok(Response::new(response))
    }

    async fn solve(&self, _request: Request<SolveRequest>) -> Result<Response<SolveResponse>, Status> {
        Err(Status::unimplemented(
            "Solve is not implemented by av-dynamics-service (M6.2 scope). GMAT's \
             differential corrector is the declared depth-2 candidate per ADR-002 but is \
             not wired into this service yet.",
        ))
    }
}
