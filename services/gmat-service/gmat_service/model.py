"""Owns the one GMAT configuration gmat-service hosts (ADR-002 depth 1, "Python API
in-process, now").

GMAT holds ONE configuration per process and is not thread-safe
(``docs/adr/002-dynamics-contract.md``, amendment 2026-09-02: "all handles are !Send;
parallelism is by process"). Every function in this module touches ``gmatpy`` global state
(via ``altavista``) and MUST be called only from the single worker thread
:mod:`gmat_service.server` pins for GMAT work -- nothing in this module does its own
locking; serialization is the caller's job.

Units: this module speaks GMAT's native units (km, km/s, A1MJD) internally, exactly like
``altavista.scenario``. Every public method's inputs/outputs are SI metres/metres-per-second
and TAI nanoseconds (CDM v1); the km<->m and A1MJD<->TAI-ns conversions happen only at the
edges, reusing :mod:`altavista.cdm`'s ``a1mjd_to_tai_ns`` / ``tai_ns_to_a1mjd`` (never a
second implementation of either).

Why this does not call ``altavista.scenario.Scenario.propagate()``
--------------------------------------------------------------------
``Scenario.propagate(step=...)`` uses ``step`` for two different things at once: the size
of each external ``Propagator.Step(dt)`` call, and the trajectory recording cadence.
Corrected in lead review (2026-09-02, open-questions.md question 77): ``Propagator.Step(dt)``
**does** sub-step internally to honour ``MaxStep``, but ``RungeKutta::Step(Real dt)`` gives up
and returns ``False`` once more than ``MaxStepAttempts`` (default 50, rejected attempts
included) substeps have been attempted, leaving the state only partly advanced. A caller that
ignores that boolean gets a plausible wrong trajectory: a single 3600 s external step at
``MaxStep = 600`` diverged by roughly 1e7 m over the golden arc, while the same chunks are
exact once ``MaxStepAttempts`` is raised (``tests/test_gmat_step_return.py``). So a
``PropagateRequest`` whose ``sample_interval_s`` is large relative to ``MaxStep`` must be
chunked, and every ``Step()`` return value must be checked.

This module decouples the two concerns instead: internal ``Propagator.Step()`` calls are
always chunked at ``min(sample_interval_s, MaxStep)`` (see :func:`GmatModel._run`), while
trajectory samples are recorded only at (a whole-step-aligned approximation of) the
caller's requested cadence. ``altavista/scenario.py`` is not owned by this worker (M2.2's
file list draws the line there); this is reported as an escalation for its owner, not
fixed in place.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import List, Sequence, Tuple

from altavista import cdm as cdm_adapter
from altavista.model import Trajectory as GvTrajectory
from altavista.scenario import Scenario

from . import config
from . import covariance as cov_check

LOG = logging.getLogger(__name__)

M_PER_KM = 1000.0


def _propagate_covariance(phi: Sequence[float], p0: Sequence[float], n: int = 6) -> Tuple[List[float], float]:
    """``P(t) = Phi P0 Phi^T``, row-major ``n x n`` throughout, explicitly symmetrized.
    Returns ``(flat row-major P, max pre-symmetrization asymmetry)`` so a caller can report
    the correction rather than let an asymmetric matrix through silently -- mirrors
    ``av_dynamics::propagate_covariance`` (Rust) and ``goldens/gen_leo_1day.py``'s
    ``_propagate_covariance`` exactly (three independent implementations of the same
    three-line formula, one per language/binding, so a bug shared by copy-paste is not
    silently agreed on by all three)."""
    tmp = [sum(phi[i * n + k] * p0[k * n + j] for k in range(n)) for i in range(n) for j in range(n)]
    p = [sum(tmp[i * n + k] * phi[j * n + k] for k in range(n)) for i in range(n) for j in range(n)]
    max_asym = max((abs(p[i * n + j] - p[j * n + i]) for i in range(n) for j in range(i + 1, n)), default=0.0)
    sym = [0.5 * (p[i * n + j] + p[j * n + i]) for i in range(n) for j in range(n)]
    return sym, max_asym


class ModelError(Exception):
    """Raised for a request this model legitimately cannot honour. Caught by
    :mod:`gmat_service.service` and turned into a ``grpc.StatusCode`` via ``code``."""

    def __init__(self, message: str, code: str = "INVALID_ARGUMENT"):
        super().__init__(message)
        self.code = code


STEP_COV_NOT_IMPLEMENTED_MSG = (
    "Step does not propagate an input covariance (StepRequest.cov). This service's STM "
    f"path ({config.MODEL_ID}, ADR-002 depth 1, M3.2) is wired into Propagate only "
    "(PropagateRequest.covariance / GaussianState.cov), which requests the STM through "
    "PropagationStateManager.SetProperty('STM', sc) and propagates P0 through it over the "
    "whole horizon in one call; Step's per-substep covariance propagation was not built. "
    "Use Propagate for covariance.")


def _require_no_controls(controls: Sequence[float]) -> None:
    if controls:
        raise ModelError(
            f"{config.MODEL_ID} declares no ControlComponents (no actuation force is in "
            f"the force model); got {len(controls)} control value(s)")


def _state_to_km(state_si: Sequence[float]) -> List[float]:
    if len(state_si) < 6:
        raise ModelError(f"state vector needs 6 components (pos xyz, vel xyz); got {len(state_si)}")
    return [float(x) / M_PER_KM for x in state_si[:6]]


def _state_to_si(state_km: Sequence[float]) -> List[float]:
    return [float(x) * M_PER_KM for x in state_km[:6]]


def _write_back(obj, epoch_a1mjd: float, state_km: Sequence[float]) -> None:
    """Push (epoch, Cartesian state) into a GMAT Spacecraft object's fields.

    The same field sequence ``altavista/scenario.py``'s ``Spacecraft._write_back`` and
    ``goldens/gen_leo_1day.py`` both already use (independently, twice) -- a third small
    copy here follows that established pattern rather than reaching into a "private"
    method of a class this worker does not own.
    """
    obj.SetField("DateFormat", "A1ModJulian")
    obj.SetField("Epoch", repr(epoch_a1mjd))
    obj.SetField("CoordinateSystem", config.FRAME_ID)
    obj.SetField("DisplayStateType", "Cartesian")
    for k, v in zip(("X", "Y", "Z", "VX", "VY", "VZ"), state_km):
        obj.SetField(k, float(v))


@dataclass
class StepResult:
    state_si: List[float]
    tai_ns: int


class GmatModel:
    """The one GMAT configuration this server hosts. Construct once; call :meth:`warm_up`
    (on the single GMAT worker thread) before serving; every other method must also run on
    that same thread -- see the module docstring."""

    def __init__(self, run_id: str):
        self.run_id = run_id
        self._sc: "Scenario | None" = None
        self._prop_fields = {}
        self._step_obj = None
        self._step_prop = None
        self._propagate_obj = None
        self._propagate_prop = None
        self._cov_obj = None
        self._cov_prop = None
        self._deriv_fm = None
        self._deriv_psm = None
        self._deriv_sat = None
        self._deriv_base_tai_ns = 0

    # ------------------------------------------------------------------ setup
    def warm_up(self) -> None:
        """Load GMAT and build the one force model / propagator configuration this server
        hosts, matching ``goldens/gen_leo_1day.py`` exactly (see ``config.SETTINGS``).
        Idempotent -- safe to call more than once."""
        if self._sc is not None:
            return
        sc = Scenario(name="gmat_service", frame=config.FRAME_ID)
        g = sc.gmat
        fm = sc.force_model(
            central_body=config.SETTINGS["central_body"],
            degree=config.SETTINGS["gravity"]["degree"], order=config.SETTINGS["gravity"]["order"],
            point_masses=tuple(config.SETTINGS["point_masses"]), drag=config.SETTINGS["drag"],
            srp=config.SETTINGS["srp"], name="gv_svc_fm")
        prop_fields = {
            "InitialStepSize": config.SETTINGS["initial_step_s"],
            "Accuracy": config.SETTINGS["accuracy"],
            "MinStep": config.SETTINGS["min_step_s"],
            "MaxStep": config.SETTINGS["max_step_s"],
        }

        def _build_propagator(name: str):
            prop = g.Construct("Propagator", name)
            prop.SetReference(g.Construct(config.SETTINGS["integrator"], name + "_gator"))
            prop.SetReference(fm)
            for k, v in prop_fields.items():
                prop.SetField(k, float(v))
            return prop

        self._sc = sc
        self._prop_fields = prop_fields
        self._step_obj = g.Construct("Spacecraft", "gv_svc_step_sat")
        self._step_prop = _build_propagator("gv_svc_step_prop")
        self._propagate_obj = g.Construct("Spacecraft", "gv_svc_propagate_sat")
        self._propagate_prop = _build_propagator("gv_svc_propagate_prop")
        # A dedicated Spacecraft/Propagator pair for covariance propagation (M3.2): kept
        # separate from `_propagate_obj`/`_propagate_prop` above rather than requesting the
        # STM on the same pair, because `PropagationStateManager.SetProperty("STM", sc)`
        # permanently grows that pair's own propagation state from 6 to 42 -- reusing it for a
        # later, non-covariance Propagate call would carry the STM's extra cost and state for
        # no reason, and no attempt was made to verify the property can be un-set once
        # requested (see this module's covariance code for the exact call sequence, validated
        # against `goldens/gen_leo_1day.py`'s own STM block).
        self._cov_obj = g.Construct("Spacecraft", "gv_svc_cov_sat")
        self._cov_prop = _build_propagator("gv_svc_cov_prop")
        self._build_derivatives_engine(g)

    def _build_derivatives_engine(self, g) -> None:
        """A raw ``ForceModel`` + ``PropagationStateManager``, evaluated with
        ``GetDerivatives(state, dt)`` -- see
        ``GMAT R2026a/api/Ex_R2020a_BasicForceModel.py`` and ADR-002's measured ``dt``
        contract. Confirmed here (see the M2.2 worklog) that
        ``GetDerivatives(state, dt=D)`` on a model built at epoch T0 is bit-identical to
        ``GetDerivatives(state, dt=0)`` on a *fresh* model built at T0+D, and that this
        engine keeps giving correct, unchanged answers even after other GMAT objects
        (the Step/Propagate propagators above) are built and re-initialized -- so it is
        built once here and reused for the life of the process, like the Step/Propagate
        propagators.
        """
        self._deriv_base_tai_ns = cdm_adapter.a1mjd_to_tai_ns(config.DERIVATIVES_BASE_EPOCH_A1MJD)
        sat = g.Construct("Spacecraft", "gv_svc_deriv_sat")
        # A representative non-degenerate LEO-ish state -- never actually propagated, only
        # used to build the PropagationStateManager/ForceModel graph; GetDerivatives(state,
        # dt) below always overrides `state` with the caller's real one.
        _write_back(sat, config.DERIVATIVES_BASE_EPOCH_A1MJD, [7000.0, 0.0, 0.0, 0.0, 7.5, 0.0])
        fm = g.Construct("ForceModel", "gv_svc_deriv_fm")
        fm.SetField("CentralBody", config.SETTINGS["central_body"])
        grav = g.Construct("GravityField")
        grav.SetField("BodyName", config.SETTINGS["central_body"])
        grav.SetField("PotentialFile", config.SETTINGS["gravity"]["file"])
        grav.SetField("Degree", config.SETTINGS["gravity"]["degree"])
        grav.SetField("Order", config.SETTINGS["gravity"]["order"])
        fm.AddForce(grav)
        for b in config.SETTINGS["point_masses"]:
            pm = g.Construct("PointMassForce")
            pm.SetField("BodyName", b)
            fm.AddForce(pm)
        psm = g.PropagationStateManager()
        psm.SetObject(sat)
        psm.BuildState()
        fm.SetPropStateManager(psm)
        fm.SetState(psm.GetState())
        g.Initialize()
        fm.BuildModelFromMap()
        fm.UpdateInitialData()
        self._deriv_fm = fm
        # `psm` (unlike Spacecraft/ForceModel/GravityField/PointMassForce, which are all
        # built via g.Construct() and stay alive in GMAT's global Moderator regardless of
        # Python reference count) is built via the standalone g.PropagationStateManager()
        # constructor, NOT Construct() -- it is never registered anywhere else, so it is
        # ONLY kept alive by this Python reference. Letting `psm` go out of scope here
        # (i.e. NOT storing it) let the SWIG wrapper's __del__ free the underlying C++
        # object while `fm` still held a raw pointer to it via SetPropStateManager(psm) --
        # confirmed by reproduction to SIGSEGV inside GetDerivatives on the next call, well
        # after this method had already returned successfully. `sat` is stored for the same
        # defensive reason even though it is Construct()'d (cheap, and rules out any doubt).
        self._deriv_psm = psm
        self._deriv_sat = sat

    # ------------------------------------------------------------------ shared stepping loop
    def _run(self, obj, prop, epoch_a1mjd: float, state_km: Sequence[float],
             duration_s: float, sample_interval_s: float, *, with_stm: bool = False
             ) -> List[Tuple[float, List[float]]]:
        """Advance ``obj``/``prop`` from ``state_km`` at ``epoch_a1mjd`` for
        ``duration_s`` seconds, recording ``(elapsed_s, state_km)`` samples at
        ``sample_interval_s`` cadence (always including ``t=0`` and the final point).
        Internal ``Propagator.Step()`` calls are always chunked at
        ``min(sample_interval_s, MaxStep)`` -- see this module's docstring for why.

        ``with_stm=True`` (M3.2) additionally requests ``obj``'s orbit State Transition
        Matrix from ``prop``'s ``PropagationStateManager`` before ``PrepareInternals()``
        (``PropagationStateManager.SetProperty("STM", obj)``, per ``GMAT_API_Cookbook``'s
        "STM and Covariance Propagation" chapter, exactly as
        ``goldens/gen_leo_1day.py``'s STM block and ``crates/gmat-sys``'s
        ``derivative_model_with_stm`` both do), which grows every recorded sample's state
        from 6 to 42 elements (index ``6 + row*6 + col`` is STM element ``(row, col)``,
        row-major -- ADR-002's second amendment) with no other change to this method: the
        physical 6-state block is unaffected by requesting the STM (measured bit-identical
        in the Rust spike; not re-verified independently here, this call sequence *is* GMAT's
        own reference computation, not a reproduction of it -- see ``propagate_covariance``'s
        doc comment for why that is the correct mechanism at this depth). Default ``False``:
        every existing caller (`step`, `propagate`) is unaffected."""
        _write_back(obj, epoch_a1mjd, state_km)
        self._sc.gmat.Initialize()
        prop.AddPropObject(obj)
        if with_stm:
            psm = prop.GetPropStateManager()
            if not psm.SetProperty("STM", obj):
                raise ModelError("PropagationStateManager.SetProperty('STM', ...) returned False", code="FAILED_PRECONDITION")
        prop.PrepareInternals()
        gator = prop.GetPropagator()

        max_step_s = self._prop_fields["MaxStep"]
        internal_step = min(sample_interval_s, max_step_s)
        steps_per_sample = max(1, round(sample_interval_s / internal_step))

        samples: List[Tuple[float, List[float]]] = [(0.0, [float(x) for x in gator.GetState()])]
        elapsed = 0.0
        n = 0
        while elapsed < duration_s - 1e-9:
            dt = min(internal_step, duration_s - elapsed)
            if not gator.Step(dt):
                raise RuntimeError(
                    f"GMAT Propagator.Step({dt}) failed at elapsed={elapsed:.1f} s: MaxStepAttempts exhausted "
                    f"(question 77); reduce the internal chunk or raise MaxStepAttempts")
            elapsed += dt
            n += 1
            if n % steps_per_sample == 0 or elapsed >= duration_s - 1e-9:
                samples.append((elapsed, [float(x) for x in gator.GetState()]))
        return samples

    # ------------------------------------------------------------------ Step
    def step(self, state_si: Sequence[float], tai_ns: int, controls: Sequence[float], dt_s: float) -> StepResult:
        _require_no_controls(controls)
        if dt_s <= 0:
            raise ModelError("dt_s must be positive")
        self.warm_up()
        epoch_a1mjd = cdm_adapter.tai_ns_to_a1mjd(tai_ns)
        state_km = _state_to_km(state_si)
        samples = self._run(self._step_obj, self._step_prop, epoch_a1mjd, state_km, dt_s, dt_s)
        elapsed, final_km = samples[-1]
        new_tai_ns = cdm_adapter.a1mjd_to_tai_ns(epoch_a1mjd + elapsed / 86400.0)
        return StepResult(state_si=_state_to_si(final_km), tai_ns=new_tai_ns)

    # ------------------------------------------------------------------ Propagate
    def propagate(self, *, seed_mean_si: Sequence[float], seed_epoch_tai_ns: int,
                  horizon_tai_ns: int, sample_interval_s: float, entity_id: str,
                  output_frame_id: str, controls: Sequence, impulses: Sequence) -> GvTrajectory:
        if controls:
            raise ModelError(
                f"{config.MODEL_ID} does not implement ControlSegment schedules (no "
                f"actuation force is in the force model); got {len(controls)} segment(s)")
        if impulses:
            raise ModelError(
                f"{config.MODEL_ID}'s Propagate does not implement impulsive maneuvers; "
                f"got {len(impulses)} impulse(s). altavista.scenario.Scenario.maneuver "
                f"supports these directly for interactive scenario-building, but this RPC "
                f"does not wrap that path (M2.2 scope).", code="UNIMPLEMENTED")
        if output_frame_id and output_frame_id != config.FRAME_ID:
            raise ModelError(
                f"{config.MODEL_ID} only propagates in {config.FRAME_ID!r}; converting to "
                f"output_frame_id={output_frame_id!r} needs a frame service, which is not "
                f"hosted by gmat-service", code="UNIMPLEMENTED")
        if len(seed_mean_si) < 6:
            raise ModelError(f"seed.mean needs >= 6 components (pos xyz, vel xyz); got {len(seed_mean_si)}")
        if sample_interval_s <= 0:
            raise ModelError("sample_interval_s must be positive")
        duration_s = (horizon_tai_ns - seed_epoch_tai_ns) / 1e9
        if duration_s <= 0:
            raise ModelError("horizon_tai_ns must be after seed.epoch_ns")

        self.warm_up()
        epoch_a1mjd = cdm_adapter.tai_ns_to_a1mjd(seed_epoch_tai_ns)
        state_km = _state_to_km(seed_mean_si)
        samples = self._run(self._propagate_obj, self._propagate_prop, epoch_a1mjd, state_km,
                            duration_s, sample_interval_s)

        traj = GvTrajectory(name=entity_id or "propagated")
        for elapsed, state_km_sample in samples:
            t = epoch_a1mjd + elapsed / 86400.0
            traj.append(t, state_km_sample)
        return traj

    # ------------------------------------------------------------------ Propagate (covariance)
    def propagate_covariance(self, *, seed_mean_si: Sequence[float], seed_epoch_tai_ns: int,
                             horizon_tai_ns: int, sample_interval_s: float,
                             seed_cov_si: Sequence[float], entity_id: str,
                             output_frame_id: str = "", controls: Sequence = (),
                             impulses: Sequence = (),
                             nearest_spd_projection: bool = False,
                             accept_missing_stm_terms: bool = False) -> Tuple[GvTrajectory, List[List[float]]]:
        """Like :meth:`propagate`, but additionally requests the STM
        (``PropagationStateManager.SetProperty("STM", sc)``, ``_run(..., with_stm=True)``)
        and propagates ``seed_cov_si`` (the declared P0, question 11: covariance is always
        explicitly requested, never a silent default) through the resulting ``Phi(t0, t)`` at
        every recorded sample: ``P(t) = Phi(t0, t) P0 Phi(t0, t)^T``
        (:func:`_propagate_covariance`, explicitly symmetrized).

        Depth 1 (ADR-002, this service): GMAT's own propagator computes the STM directly, so
        reading it from ``gator.GetState()`` (the raw 42-state array this loop already reads
        for the plain 6-state case) *is* the mechanism here -- unlike the kernel (ADR-002
        depth 2, ``crates/gmat-sys``/``crates/av-kernel``), which is required to integrate its
        own STM via ``GetDerivatives`` rather than ever reading GMAT's back after the fact.
        Both mechanisms are pinned against the same golden
        (``goldens/leo_1day_jgm2_8x8_sunmoon.json``'s ``"stm"`` block), which is itself GMAT's
        own propagator run the same way this method runs it.

        **Missing-STM-terms capability check (``docs/open-questions.md`` question 82).** Before
        anything else, if ``config.HAS_RELATIVISTIC_CORRECTION`` is true (this service's fixed
        force model never sets it -- see that constant's own comment) and
        ``accept_missing_stm_terms`` is not, this raises a :class:`ModelError`
        (``FAILED_PRECONDITION``) naming the reason rather than silently returning a covariance
        built from GMAT's zeroed ``RelativisticCorrection`` A-matrix contribution. Mirrors
        ``gmat_sys::model::GmatModel::stm_capable`` (``crates/gmat-sys/src/model.rs``, Rust
        side) at this service's own request boundary: the Rust side withholds the capability at
        model-construction time (a DRM-level decision baked into the bound model once);
        Python-side ``GmatModel`` is a long-lived singleton built once in :meth:`warm_up` with a
        fixed force model, so the equivalent check happens per-request instead, reading the same
        underlying fact (``config.HAS_RELATIVISTIC_CORRECTION``) through this service's own
        config rather than through a `GmatModel::new`-style constructor argument.

        **Covariance hygiene (``docs/open-questions.md`` question 80).** Every propagated
        ``P(t)`` is run through :func:`gmat_service.covariance.check_spd` -- the same
        Cholesky-based SPD bar ``spoore_cdm::GaussianState``'s Rust constructor enforces --
        *before* it is ever returned to the caller (and, in turn, before :mod:`.service`
        attaches it to ``TrajectorySample.cov``). A failure is re-raised as a
        :class:`ModelError` (``FAILED_PRECONDITION``), never a silent repair, *unless*
        ``nearest_spd_projection`` is ``True``, in which case
        :func:`gmat_service.covariance.nearest_spd` replaces the failing sample (logged at
        ``WARNING``, still counted as a hygiene failure) and the run continues. Off by default.

        Both ``nearest_spd_projection`` and ``accept_missing_stm_terms`` are real, additive
        fields on ``DrmOptions`` (``proto/altavista/v1/system.proto``, questions 82/83) -- what
        is *not* wired is a path from a ``PropagateRequest`` on the wire to either flag:
        ``PropagateRequest``/``StepRequest`` carry a plain ``bool covariance``, not a
        ``DrmOptions`` message, and no crate or module in this repo yet owns constructing a
        ``DesignReferenceMission`` and driving this service from it end to end. A caller that
        has read both flags off an actual DRM passes them straight through as plain keyword
        arguments; :mod:`gmat_service.service`'s ``Propagate`` RPC handler still calls this with
        both at their ``False`` defaults, unchanged by this task, since it has no request field
        to read them from.

        Returns ``(trajectory, covariances)`` -- ``covariances[i]`` is the row-major 6x6
        covariance at ``trajectory``'s ``i``-th sample; kept separate from ``GvTrajectory``
        (which has no covariance field) rather than smuggled through it, so
        :mod:`gmat_service.service` can zip them onto the CDM ``Trajectory.samples[i].cov``
        fields it already builds from ``trajectory`` via :func:`altavista.cdm.trajectory_to_cdm`.
        """
        if config.HAS_RELATIVISTIC_CORRECTION and not accept_missing_stm_terms:
            raise ModelError(
                f"{config.MODEL_ID}'s force model includes RelativisticCorrection, whose "
                "GetDerivatives fills its STM/A-matrix contribution with an unconditional zero "
                "(a stub, not a physically-absent term -- docs/open-questions.md question 82); "
                "the platform declares the STM capability absent for such a model, so a "
                "covariance request is refused unless the caller explicitly acknowledges "
                "DrmOptions.accept_missing_stm_terms", code="FAILED_PRECONDITION")
        if controls:
            raise ModelError(
                f"{config.MODEL_ID} does not implement ControlSegment schedules (no "
                f"actuation force is in the force model); got {len(controls)} segment(s)")
        if impulses:
            raise ModelError(
                f"{config.MODEL_ID}'s Propagate does not implement impulsive maneuvers; "
                f"got {len(impulses)} impulse(s).", code="UNIMPLEMENTED")
        if output_frame_id and output_frame_id != config.FRAME_ID:
            raise ModelError(
                f"{config.MODEL_ID} only propagates in {config.FRAME_ID!r}; converting to "
                f"output_frame_id={output_frame_id!r} needs a frame service, which is not "
                f"hosted by gmat-service", code="UNIMPLEMENTED")
        if len(seed_mean_si) < 6:
            raise ModelError(f"seed.mean needs >= 6 components (pos xyz, vel xyz); got {len(seed_mean_si)}")
        n = 6
        if not seed_cov_si:
            raise ModelError(
                "covariance=true requires seed.cov (a declared P0, row-major 6x6); got an "
                "empty covariance. Question 11: covariance is always explicitly requested, "
                "never a silent default -- there is no identity or zero P0 fallback.")
        if len(seed_cov_si) != n * n:
            raise ModelError(f"seed.cov must be {n * n} elements (row-major {n}x{n}); got {len(seed_cov_si)}")
        if sample_interval_s <= 0:
            raise ModelError("sample_interval_s must be positive")
        duration_s = (horizon_tai_ns - seed_epoch_tai_ns) / 1e9
        if duration_s <= 0:
            raise ModelError("horizon_tai_ns must be after seed.epoch_ns")

        self.warm_up()
        epoch_a1mjd = cdm_adapter.tai_ns_to_a1mjd(seed_epoch_tai_ns)
        state_km = _state_to_km(seed_mean_si)
        samples = self._run(self._cov_obj, self._cov_prop, epoch_a1mjd, state_km,
                            duration_s, sample_interval_s, with_stm=True)

        traj = GvTrajectory(name=entity_id or "propagated")
        covariances: List[List[float]] = []
        max_asym_over_run = 0.0
        # Smallest Cholesky-diagonal^2 proxy seen across every sample this run (see
        # gmat_service.covariance.CholeskyDiagnostics's docstring for precisely what this is
        # and is not) -- the *minimum*, not an average, so a single sample drifting toward
        # singularity is not diluted by the rest of the run.
        min_diag_sq_over_run = float("inf")
        for elapsed, state42 in samples:
            if len(state42) != n + n * n:
                raise ModelError(f"expected a {n + n * n}-element STM-augmented state (with_stm=True); got {len(state42)} components")
            t = epoch_a1mjd + elapsed / 86400.0
            traj.append(t, state42[:n])
            phi = state42[n:n + n * n]
            cov, max_asym = _propagate_covariance(phi, seed_cov_si, n)
            max_asym_over_run = max(max_asym_over_run, max_asym)

            sample_context = f"{config.MODEL_ID} propagate_covariance sample at elapsed={elapsed:.1f}s"
            try:
                diag = cov_check.check_spd(cov, n, sample_context)
            except cov_check.CovarianceHygieneError as e:
                if not nearest_spd_projection:
                    raise ModelError(
                        f"propagated covariance failed the SPD hygiene check: {e}",
                        code="FAILED_PRECONDITION") from e
                LOG.warning(
                    "propagate_covariance: %s failed the SPD hygiene check (%s); applying the "
                    "opt-in nearest-SPD projection (declared, not a silent repair -- "
                    "nearest_spd_projections_applied() now %d)",
                    sample_context, e, cov_check.nearest_spd_projections_applied() + 1)
                cov = cov_check.nearest_spd(cov, n)
                diag = cov_check.check_spd(cov, n, sample_context)
            min_diag_sq_over_run = min(min_diag_sq_over_run, diag.min_cholesky_diag_sq)
            covariances.append(cov)
        LOG.info(
            "propagate_covariance: %d sample(s) over %.1f s, max Phi P0 Phi^T asymmetry "
            "%.6e before symmetrizing (corrected on every sample); smallest Cholesky-diagonal^2 "
            "proxy seen this run: %.6e (an upper bound on the true smallest eigenvalue -- see "
            "gmat_service.covariance.CholeskyDiagnostics.min_cholesky_diag_sq)",
            len(samples), duration_s, max_asym_over_run, min_diag_sq_over_run)
        return traj, covariances

    # ------------------------------------------------------------------ Derivatives
    def derivatives(self, state_si: Sequence[float], tai_ns: int, controls: Sequence[float]) -> List[float]:
        _require_no_controls(controls)
        self.warm_up()
        state_km = _state_to_km(state_si)
        dt_s = (tai_ns - self._deriv_base_tai_ns) / 1e9
        self._deriv_fm.GetDerivatives(state_km, dt=dt_s)
        dv_km = list(self._deriv_fm.GetDerivativeArray())
        if len(dv_km) < 6:
            raise ModelError(f"GetDerivativeArray returned {len(dv_km)} component(s); expected >= 6")
        # dv[0:3] is velocity (km/s), dv[3:6] is acceleration (km/s^2); both scale km->m the
        # same way ("per second" order does not change the length-unit factor).
        return [x * M_PER_KM for x in dv_km[:6]]
