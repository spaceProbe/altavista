"""``altavista.v1.DynamicsService``, hosted over altavista (ADR-002 depth 1: "Python API
in-process, now"). See this package's README for the threading and evidence-log contract.
"""
from __future__ import annotations

import time
import uuid
from typing import Optional

import grpc

from altavista import cdm as cdm_adapter
from altavista.pb import dynamics_service_pb2, dynamics_service_pb2_grpc

from . import config
from .evidence import EvidenceLog
from .model import STEP_COV_NOT_IMPLEMENTED_MSG, GmatModel, ModelError

_GRPC_CODE = {
    "INVALID_ARGUMENT": grpc.StatusCode.INVALID_ARGUMENT,
    "FAILED_PRECONDITION": grpc.StatusCode.FAILED_PRECONDITION,
    "UNIMPLEMENTED": grpc.StatusCode.UNIMPLEMENTED,
    "NOT_FOUND": grpc.StatusCode.NOT_FOUND,
}

def _capabilities() -> list:
    """The capability list :meth:`DynamicsServiceServicer.Describe` reports.

    A function, not a module-level constant, because ``MODEL_CAPABILITY_STM`` is conditional
    on ``config.HAS_RELATIVISTIC_CORRECTION`` (``docs/open-questions.md`` question 82): a force
    model that includes ``RelativisticCorrection`` does not actually have the STM capability
    (GMAT fills that force's A-matrix contribution with an unconditional zero), so the platform
    declares it absent in ``Describe()`` -- mirroring ``gmat_sys::model::GmatModel::
    stm_capable()``/``describe()`` (Rust side), which withholds the same capability at
    construction time for the equivalent reason. Always ``True`` includes ``MODEL_CAPABILITY_
    STM`` today (this service's fixed force model never sets ``HAS_RELATIVISTIC_CORRECTION``),
    matching the module-level list this replaced.
    """
    caps = [
        dynamics_service_pb2.MODEL_CAPABILITY_DERIVATIVES,
        dynamics_service_pb2.MODEL_CAPABILITY_STEP,
        dynamics_service_pb2.MODEL_CAPABILITY_PROPAGATE,
        dynamics_service_pb2.MODEL_CAPABILITY_DETERMINISTIC,
    ]
    # M3.2: Propagate propagates a declared P0 through GMAT's own STM
    # (PropagationStateManager.SetProperty("STM", sc)) -- see model.GmatModel.propagate_covariance.
    # Step's `cov` field is still not implemented (model.STEP_COV_NOT_IMPLEMENTED_MSG); the
    # capability is declared model-wide, matching MODEL_CAPABILITY_STEP/PROPAGATE's own
    # granularity (also declared model-wide despite Solve being the only capability with its
    # own separate UNIMPLEMENTED path here).
    if not config.HAS_RELATIVISTIC_CORRECTION:
        caps.append(dynamics_service_pb2.MODEL_CAPABILITY_STM)
    # Deliberately never MODEL_CAPABILITY_SOLVE (Solve is UNIMPLEMENTED).
    return caps


class DynamicsServiceServicer(dynamics_service_pb2_grpc.DynamicsServiceServicer):
    def __init__(self, evidence: EvidenceLog, run_id: Optional[str] = None):
        self.run_id = run_id or uuid.uuid4().hex
        self.model = GmatModel(self.run_id)
        self.evidence = evidence

    def warm_up(self) -> None:
        """Load GMAT and build this server's one configuration. Must run on the single
        GMAT worker thread -- see gmat_service.server.serve."""
        self.model.warm_up()

    # -- helpers -------------------------------------------------------------
    def _abort(self, context, err: ModelError) -> None:
        context.abort(_GRPC_CODE.get(err.code, grpc.StatusCode.INVALID_ARGUMENT), str(err))

    def _record(self, method: str, request, response) -> None:
        self.evidence.record(method=method, request=request, response=response,
                             settings_hash=config.settings_hash(), run_id=self.run_id)

    @staticmethod
    def _created_tai_ns() -> int:
        return cdm_adapter.utc_ns_to_tai_ns(time.time_ns())

    # -- RPCs ------------------------------------------------------------------
    def Describe(self, request, context):
        if request.model_id and request.model_id != config.MODEL_ID:
            context.abort(grpc.StatusCode.NOT_FOUND,
                          f"unknown model_id {request.model_id!r}; this server hosts {config.MODEL_ID!r}")
        response = dynamics_service_pb2.ModelInfo(
            id=config.MODEL_ID, version=config.GMAT_VERSION, state_space_id=config.STATE_SPACE_ID,
            frame_id=config.FRAME_ID, depth="gmat-api", settings_hash=config.settings_hash(),
            goldens=[config.GOLDEN_NAME], capabilities=_capabilities())
        self._record("Describe", request, response)
        return response

    def Derivatives(self, request, context):
        try:
            state_dot = self.model.derivatives(list(request.state.state), request.state.tai_ns,
                                               list(request.controls))
        except ModelError as e:
            self._abort(context, e)
            return None
        response = dynamics_service_pb2.DerivativesResponse(state_dot=state_dot)
        self._record("Derivatives", request, response)
        return response

    def Step(self, request, context):
        if request.cov:
            self._abort(context, ModelError(f"Step: {STEP_COV_NOT_IMPLEMENTED_MSG}", code="FAILED_PRECONDITION"))
            return None
        try:
            result = self.model.step(list(request.state.state), request.state.tai_ns,
                                     list(request.controls), request.dt_s)
        except ModelError as e:
            self._abort(context, e)
            return None
        response = dynamics_service_pb2.StepResponse(
            state=dynamics_service_pb2.StateVector(state=result.state_si, tai_ns=result.tai_ns))
        self._record("Step", request, response)
        return response

    def Propagate(self, request, context):
        covariances = None
        try:
            if request.covariance:
                # nearest_spd_projection and accept_missing_stm_terms both stay at their
                # `False` defaults here: PropagateRequest carries a plain `bool covariance`,
                # not a DrmOptions message, so this RPC has no request field to read either
                # flag from -- see model.GmatModel.propagate_covariance's own docstring.
                traj, covariances = self.model.propagate_covariance(
                    seed_mean_si=list(request.seed.mean), seed_epoch_tai_ns=request.seed.epoch_ns,
                    horizon_tai_ns=request.horizon_tai_ns, sample_interval_s=request.sample_interval_s,
                    seed_cov_si=list(request.seed.cov), entity_id=request.entity_id,
                    output_frame_id=request.output_frame_id,
                    controls=list(request.controls), impulses=list(request.impulses))
            else:
                traj = self.model.propagate(
                    seed_mean_si=list(request.seed.mean), seed_epoch_tai_ns=request.seed.epoch_ns,
                    horizon_tai_ns=request.horizon_tai_ns, sample_interval_s=request.sample_interval_s,
                    entity_id=request.entity_id, output_frame_id=request.output_frame_id,
                    controls=list(request.controls), impulses=list(request.impulses))
        except ModelError as e:
            self._abort(context, e)
            return None
        cdm_traj = cdm_adapter.trajectory_to_cdm(
            traj, entity_id=request.entity_id or "propagated", frame_id=config.FRAME_ID,
            state_space_id=config.STATE_SPACE_ID, dynamics_model=config.MODEL_ID,
            dynamics_hash=config.settings_hash(), dynamics_depth="gmat-api",
            # This service has no DRM to hash a "configuration" out of; the force-model /
            # integrator settings ARE the whole configuration that produced this
            # trajectory, so config_hash reuses the same settings_hash rather than being
            # left empty or faked from something else.
            config_hash=config.settings_hash(), tool="gmat-service", run_id=self.run_id,
            created_tai_ns=self._created_tai_ns())
        if covariances is not None:
            # trajectory_to_cdm never fills TrajectorySample.cov itself (altavista/cdm.py's own
            # "covariance placeholder" contract -- that module is not owned by this worker);
            # this is the one place `cov` is ever populated on a message this service returns,
            # and only when `covariance` was explicitly requested (question 11).
            if len(covariances) != len(cdm_traj.samples):
                self._abort(context, ModelError(
                    f"internal error: {len(covariances)} covariance sample(s) but "
                    f"{len(cdm_traj.samples)} trajectory sample(s)", code="FAILED_PRECONDITION"))
                return None
            for sample, cov in zip(cdm_traj.samples, covariances):
                sample.cov[:] = cov
        response = dynamics_service_pb2.PropagateResponse(trajectory=cdm_traj)
        self._record("Propagate", request, response)
        return response

    def Solve(self, request, context):
        context.abort(
            grpc.StatusCode.UNIMPLEMENTED,
            "Solve is not implemented by gmat-service (M2.2 scope). GMAT's differential "
            "corrector is the declared depth-1 candidate per ADR-002 but is not wired into "
            "this service yet.")
