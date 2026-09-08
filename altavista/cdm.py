"""altavista <-> ``altavista.v1`` CDM v1 adapter (M1.3, ADR-001).

This is the one place altavista crosses the GMAT-adapter boundary in the other direction
from :mod:`altavista.frames`: :mod:`altavista.frames` converts *states* (km/km-s <-> SI) at a
given epoch; this module converts whole *trajectories and events* (:mod:`altavista.model`,
positions km / velocities km/s / epochs A1MJD) to and from ``altavista.v1`` CDM messages
(SI metres/metres-per-second, TAI nanoseconds) -- and back, so the viewer server can
render a CDM ``Trajectory`` it did not itself produce.

Units and epoch: SI and TAI ns outside this module, km/km-s and A1MJD only inside it
--------------------------------------------------------------------------------------
Per ADR-001 and :mod:`altavista.frames`'s module docstring, "kilometres exist only inside
the GMAT adapter" and the CDM's time is TAI nanoseconds; GMAT/altavista's own time is an
A.1 Modified Julian Date float. The unit conversion (:data:`M_PER_KM`) and the epoch
conversion (:func:`a1mjd_to_tai_ns` / :func:`tai_ns_to_a1mjd`) both happen in exactly one
place each, here.

Epoch conversion, matching ``crates/av-cdm/src/time.rs`` exactly
------------------------------------------------------------------
``crates/av-cdm/src/time.rs`` (worker A's Rust crate, read but not owned by this worker)
is the reference implementation this module mirrors bit-for-bit:

* ``A1 = TAI + 0.0343817 s`` *exactly* -- a fixed constant (:data:`A1_MINUS_TAI_NS`), not
  sourced from the leap-second table. This is GMAT's own definition of its A.1 atomic
  time scale (ADR-001), unrelated to UTC leap seconds.
* GMAT's "Modified Julian Date" is ``MJD = JD - 2430000.0`` (not the IAU MJD, which
  subtracts 2400000.5); the Unix epoch's Julian Date is ``2440587.5``, so
  ``GMAT_MJD(unix epoch) = 10587.5`` (:data:`GMAT_MJD_AT_UNIX_EPOCH`).
* :func:`a1mjd_to_tai_ns` / :func:`tai_ns_to_a1mjd` are therefore *pure constant
  arithmetic* -- they do not consult the leap-second table at all, exactly like
  ``Tai::from_a1_mjd`` / ``Tai::to_a1_mjd`` in ``time.rs``. Nothing else about "TAI
  nanoseconds since the Unix epoch" needs leap seconds either, *except* that the
  ``Tai(0)`` origin time.rs chooses is defined via a UTC instant (see below), which is
  where the shared table comes in.

This module still loads ``data/time/leap_seconds.json`` once, at module level
(:func:`_leap_table`), and never hardcodes a leap-second offset or invents a second copy
of the table -- for two reasons: (1) full parity with ``time.rs``'s ``Tai`` also
includes table-driven UTC<->TAI conversions (:func:`utc_ns_to_tai_ns` /
:func:`tai_ns_to_utc_ns`), included here even though the trajectory/event adapters below
never call them, purely so this module is never tempted to invent its own second table if
a UTC boundary shows up here later; and (2) ``tests/test_cdm_adapter.py`` uses them to
cross-check this module's constants against GMAT's own ``TimeSystemConverter`` across
real leap-second boundaries (see that test's docstring for exactly what is and is not
proven this way -- there is no Rust toolchain in this environment to run ``time.rs``
directly).

Documented approximations, carried over unchanged from ``time.rs``'s module docs:
pre-1972 clamps to the table's first entry (offset 10s); post-2017 extrapolates the last
entry (offset 37s); UTC->TAI resolves the one-UTC-instant-maps-to-two-TAI-instants
ambiguity at a positive leap second to the *post*-insertion reading. None of this affects
:func:`a1mjd_to_tai_ns` / :func:`tai_ns_to_a1mjd` (no table lookup happens there at all);
it only applies to :func:`utc_ns_to_tai_ns` / :func:`tai_ns_to_utc_ns`.

The covariance placeholder (question 11)
------------------------------------------
:func:`trajectory_to_cdm` never fills ``TrajectorySample.cov``. Covariance is always
optional and explicitly requested by a DRM -- never a profile default -- and this adapter
has no DRM to consult, so it leaves ``cov`` empty rather than fabricating one (e.g. from
GMAT's own error covariance, which altavista's ``Scenario`` does not even propagate today).
A caller that has real covariance can set ``TrajectorySample.cov`` on the returned message
itself; this module will not silently invent zeros or an identity matrix.

Event ``values`` (no guessing)
---------------------------------
altavista's ``Event.detail`` is a free-form string. The only shape this module trusts
enough to turn into structured, SI ``values`` is the exact one
``Scenario.maneuver`` generates: ``"dv = 20.00 m/s (VNB)"`` (see :func:`_parse_dv_detail`).
Even then, only the scalar magnitude (``dv_mps``) is recovered -- altavista's own string
never carries the individual V/N/B (or inertial) components, so this module does not
invent a ``dv_v`` / ``dv_n`` / ``dv_b`` breakdown, and it does not coerce the frame name
("VNB") into a number. Any other ``detail`` shape (e.g. the multi-line text
``Scenario._event_from_summary`` copies out of a GMAT command summary) is left as
``detail`` only, with ``values`` empty -- never a guess.

Frame mapping (no silent default)
------------------------------------
:func:`frame_definition_for` maps an altavista :class:`~altavista.model.Frame`'s ``axes``
string (``"MJ2000Eq"``, ``"MJ2000Ec"``, ``"BodyFixed"``, ``"ICRF"``) to the matching
``altavista.v1.AxesKind``. Anything else altavista's own ``parse_frame`` accepts (e.g.
``"BodyInertial"`` from the ``Inertial`` suffix, or GMAT's ``Topocentric``) has no
``AxesKind`` counterpart and raises :class:`UnmappedAxesError` rather than approximating
one -- the same "never silently approximate an axes kind" rule
:mod:`altavista.frames` follows.
"""
from __future__ import annotations

import hashlib
import json
import math
import re
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

from .model import (STATE_SPACE_ID_CARTESIAN_POS_VEL_6, STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4,
                   BodyTrack, Event, Frame, ScenarioData, Trajectory)
from .pb import core_pb2, entity_pb2, trajectory_pb2

REPO_ROOT = Path(__file__).resolve().parent.parent
LEAP_SECONDS_PATH = REPO_ROOT / "data" / "time" / "leap_seconds.json"

TOOL_NAME = "altavista.cdm"
# Aliases of altavista/model.py's canonical id constants, kept under their original names for
# every existing caller (services/gmat-service/gmat_service/config.py imports
# DEFAULT_STATE_SPACE_ID directly; tests/test_cdm_adapter.py asserts against both).
DEFAULT_STATE_SPACE_ID = STATE_SPACE_ID_CARTESIAN_POS_VEL_6
# M6.3: state space id used when a Trajectory carries a real attitude quaternion
# stream (altavista/model.py's additive Trajectory.attitude). No proto change was
# needed for this -- trajectory.proto's TrajectorySample.mean is already a generic
# "length n, in the trajectory's state space and frame" vector (proto/altavista/v1/
# trajectory.proto), so a 10-component mean (position xyz, velocity xyz, quaternion
# xyzw scalar-last) fits the existing shape; only the id string that names that shape
# is new. **M7.1 (docs/open-questions.md question 88):** the lead's condition (a) on
# accepting attitude in the state vector was that this id name an actual *declared*
# `StateSpace` message (labels and units), not merely exist as a string -- see
# `state_space_for` below, which is that declaration, and `CdmBundle.state_spaces` /
# `scenario_to_cdm`, which emit it alongside every `Trajectory` a scenario produces.
DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE = STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4

# M20.1 (`docs/open-questions.md` question 133): a second, pre-existing id for exactly the
# same 6-component Cartesian position/velocity shape `DEFAULT_STATE_SPACE_ID` already
# declares -- `crates/av-kernel/src/trajectory.rs::GMAT_ORBITAL_CARTESIAN6_ID`'s own doc
# comment: "names the same physical concept ... under a different naming convention, not a
# different space." Every real GMAT-bound instance this crate binds (`crates/av-kernel/src/
# drm/binding.rs`) stamps `ModelInfo.state_space_id = SystemDefinition.state_space_id`
# verbatim, and every committed `drms/*.system.yaml` GMAT fixture declares this id, not
# `DEFAULT_STATE_SPACE_ID` -- so a real `RunProducts.trajectories` entry from `av-run` names
# *this* id, and `state_space_for`/`has_position_class` below must recognize it too, or every
# real ingested GMAT trajectory would be an `UnknownStateSpaceError`.
GMAT_ORBITAL_CARTESIAN6_ID = "gmat.orbital.cartesian6"
# M20.1 (question 133, decided by the lead): the state space a non-physical native instance
# (e.g. a pure port controller with no trajectory of its own, `crate::drm::binding::
# ConstantAccelModel` bound to `dynamics_model: native.*` with no physical motion) declares
# for itself -- six independent, unitless scalars, never a Cartesian position/velocity
# shape. Matches `crates/av-kernel/src/trajectory.rs::NATIVE_CONTROLLER_SCALAR6_ID` and
# `drms/demo_two_instance_ctrl.system.yaml`'s own declared `state_space` verbatim (same id,
# same component labels/units/order).
NATIVE_CONTROLLER_SCALAR6_ID = "native.controller.scalar6"

M_PER_KM = 1000.0
NS_PER_DAY = 86_400_000_000_000.0
# GMAT's A.1 atomic time: A1 = TAI + 0.0343817 s exactly (ADR-001). A fixed constant, not
# sourced from the leap-second table -- matches crates/av-cdm/src/time.rs's
# `A1_MINUS_TAI_NS`.
A1_MINUS_TAI_NS = 34_381_700
# GMAT "Modified Julian" of the Unix epoch: JD(1970-01-01T00:00:00) = 2440587.5 and GMAT's
# MJD = JD - 2430000.0, so GMAT_MJD(unix epoch) = 10587.5. Matches time.rs's
# `GMAT_MJD_AT_UNIX_EPOCH`.
GMAT_MJD_AT_UNIX_EPOCH = 10_587.5


# --------------------------------------------------------------------------- exceptions
class CdmAdapterError(Exception):
    """Base class for every error this module raises."""


class UnmappedAxesError(CdmAdapterError):
    """An altavista ``Frame.axes`` string has no ``altavista.v1.AxesKind`` counterpart."""

    def __init__(self, axes: str):
        self.axes = axes
        super().__init__(
            f"altavista Frame.axes {axes!r} has no altavista.v1.AxesKind mapping; known "
            f"axes are {sorted(_AXES_KIND_BY_ALTAVISTA_AXES)}")


class UnknownBodyError(CdmAdapterError):
    """A ``RunProducts.frames`` entry names an origin body this process's GMAT solar
    system has no data for (M18.2, ``docs/open-questions.md`` question 125): raised by
    :func:`bodies_from_frames` rather than silently dropping that body from the scene
    or falling back to a default body list -- the same "never silently skip" rule
    :class:`UnmappedAxesError`/:class:`UnknownStateSpaceError` already follow."""

    def __init__(self, body: str, detail: str = ""):
        self.body = body
        msg = (f"RunProducts frame names origin body {body!r}, which this process's "
              f"GMAT solar system has no data for")
        if detail:
            msg += f": {detail}"
        super().__init__(msg)


class UnknownStateSpaceError(CdmAdapterError):
    """A ``state_space_id`` this module has no declared ``StateSpace`` for (M7.1,
    ``docs/open-questions.md`` question 88's condition (a)): raised by
    :func:`state_space_for` rather than guessing at a shape from the id string alone or
    from how many components a sample happens to carry."""

    def __init__(self, state_space_id: str):
        self.state_space_id = state_space_id
        super().__init__(
            f"no declared StateSpace for id {state_space_id!r}; known ids are "
            f"{sorted(_STATE_SPACE_BUILDERS)}")


# --------------------------------------------------------------------------- leap-second table
@dataclass(frozen=True)
class _LeapEntry:
    offset_s: int
    utc_effective_unix_ns: int
    tai_effective_ns: int


@lru_cache(maxsize=1)
def _leap_table() -> Tuple[_LeapEntry, ...]:
    """The shared leap-second table (``data/time/leap_seconds.json``), loaded once.

    Mirrors ``crates/av-cdm/src/time.rs``'s ``table()``: same file, same fields, loaded
    lazily and cached for the life of the process. Never hand-edit or duplicate this data
    -- ``scripts/gen_leap_seconds.py`` (not owned by this worker) is the only generator.
    """
    doc = json.loads(LEAP_SECONDS_PATH.read_text())
    entries = tuple(
        _LeapEntry(int(e["tai_utc_offset_seconds"]), int(e["utc_effective_unix_ns"]), int(e["tai_effective_ns"]))
        for e in doc["entries"]
    )
    if not entries:
        raise CdmAdapterError(f"{LEAP_SECONDS_PATH} has no entries")
    return entries


def _offset_for_tai_ns(tai_ns: int) -> int:
    """The TAI-UTC offset (seconds) in force at ``tai_ns``. Clamps before the first
    entry, extrapolates after the last -- see the module docstring."""
    entries = _leap_table()
    offset = entries[0].offset_s
    for e in entries:
        if e.tai_effective_ns <= tai_ns:
            offset = e.offset_s
        else:
            break
    return offset


def _offset_for_utc_ns(utc_ns: int) -> int:
    """The TAI-UTC offset (seconds) in force at proleptic UTC nanosecond ``utc_ns``.

    At a leap-second boundary this resolves the ambiguity by choosing the offset newly in
    force at (and after) ``utc_ns`` -- the "post-insertion reading" time.rs documents and
    tests at ``from_utc_nanos_resolves_the_leap_second_collision_to_the_post_insertion_reading``.
    """
    entries = _leap_table()
    offset = entries[0].offset_s
    for e in entries:
        if e.utc_effective_unix_ns <= utc_ns:
            offset = e.offset_s
        else:
            break
    return offset


def tai_ns_to_utc_ns(tai_ns: int) -> int:
    """Proleptic UTC nanoseconds (every day exactly 86 400 SI seconds) for a TAI instant.

    Table-driven (see the module docstring); not used by the trajectory/event adapters
    below (which only ever cross the A1<->TAI boundary), kept for parity with
    ``time.rs``'s ``Tai::to_utc_nanos`` and for the leap-second cross-check tests.
    """
    return int(tai_ns) - _offset_for_tai_ns(int(tai_ns)) * 1_000_000_000


def utc_ns_to_tai_ns(utc_ns: int) -> int:
    """The TAI instant for a proleptic UTC nanosecond count. Table-driven; see
    :func:`tai_ns_to_utc_ns`."""
    utc_ns = int(utc_ns)
    return utc_ns + _offset_for_utc_ns(utc_ns) * 1_000_000_000


def _round_half_away_from_zero(x: float) -> int:
    """``round()`` matching Rust's ``f64::round()`` (ties away from zero), not Python's
    builtin ``round()`` (banker's rounding) -- so :func:`a1mjd_to_tai_ns` reproduces
    ``Tai::from_a1_mjd`` bit-for-bit at a tie, not merely "close"."""
    return math.floor(x + 0.5) if x >= 0 else math.ceil(x - 0.5)


def a1mjd_to_tai_ns(a1_mjd: float) -> int:
    """GMAT A1MJD float -> TAI nanoseconds. Pure constant arithmetic (no leap-second
    table lookup) -- matches ``Tai::from_a1_mjd`` in ``crates/av-cdm/src/time.rs``
    exactly, including its rounding rule."""
    a1_ns = _round_half_away_from_zero((float(a1_mjd) - GMAT_MJD_AT_UNIX_EPOCH) * NS_PER_DAY)
    return int(a1_ns) - A1_MINUS_TAI_NS


def tai_ns_to_a1mjd(tai_ns: int) -> float:
    """TAI nanoseconds -> GMAT A1MJD float. Pure constant arithmetic -- matches
    ``Tai::to_a1_mjd`` exactly."""
    a1_ns = int(tai_ns) + A1_MINUS_TAI_NS
    return GMAT_MJD_AT_UNIX_EPOCH + a1_ns / NS_PER_DAY


# --------------------------------------------------------------------------- frame mapping
_AXES_KIND_BY_ALTAVISTA_AXES = {
    "MJ2000Eq": core_pb2.AXES_KIND_MJ2000_EQ,
    "MJ2000Ec": core_pb2.AXES_KIND_MJ2000_EC,
    "BodyFixed": core_pb2.AXES_KIND_BODY_FIXED,
    "ICRF": core_pb2.AXES_KIND_ICRF,
}
# The inverse of the map above (M17.1, question 122): every body-axes AxesKind this
# module's frame_definition_for() can produce, mapped back to altavista's own axes string
# -- used by frames_to_viewer_json()/viewer_frame_for() below to turn a *validated*
# altavista.v1.FrameDefinition (e.g. one of RunProducts.frames) back into a
# altavista.model.Frame for ScenarioData.frame, the same (origin, axes) shape a Python
# scenario's own Frame carries. Exactly the four kinds altavista/frames.py's
# _BODY_AXES_GMAT_NAME table realizes (ICRF/MJ2000Eq/MJ2000Ec/BodyFixed) -- RIC/VNB/VVLH
# and every other AxesKind have no altavista.model.Frame counterpart (that dataclass only
# ever models a body-centred coordinate system: "origin body + axes type"), so they are
# simply absent from this dict and viewer_frame_for() returns None for them, never a
# guessed (origin, axes) pair.
_ALTAVISTA_AXES_BY_AXES_KIND = {v: k for k, v in _AXES_KIND_BY_ALTAVISTA_AXES.items()}


def frame_definition_for(frame: Frame, *, frame_id: Optional[str] = None) -> core_pb2.FrameDefinition:
    """An altavista :class:`~altavista.model.Frame` -> ``altavista.v1.FrameDefinition``.

    Raises :class:`UnmappedAxesError` for any ``axes`` value core.proto's ``AxesKind``
    cannot express (e.g. ``"BodyInertial"``, GMAT's ``Topocentric``) -- never picks a
    nearby AxesKind as an approximation. Does not run :class:`altavista.frames.FrameRegistry`
    (no live GMAT/frame-registry validation happens here); ``gmat_name`` is left unset,
    exactly as core.proto documents ("set by the frame service after validation").
    """
    axes = _AXES_KIND_BY_ALTAVISTA_AXES.get(frame.axes)
    if axes is None:
        raise UnmappedAxesError(frame.axes)
    return core_pb2.FrameDefinition(
        id=frame_id or frame.name, body=frame.origin, axes=axes,
        description=f"altavista frame {frame.name!r} ({frame.origin} {frame.axes})")


def viewer_frame_for(frame_def: core_pb2.FrameDefinition) -> Optional[Frame]:
    """The inverse of :func:`frame_definition_for`: a validated ``altavista.v1.
    FrameDefinition`` (body-axes only: ``WhichOneof("origin") == "body"`` and ``axes``
    one of ICRF/MJ2000Eq/MJ2000Ec/BodyFixed) -> the matching
    :class:`~altavista.model.Frame`. Returns ``None`` -- never a guessed ``(origin,
    axes)`` pair -- for anything else (RIC/VNB/VVLH, ENU/NED, PLATFORM_BODY,
    LOCAL_CARTESIAN, or an unset/unspecified ``axes``): those have no
    :class:`~altavista.model.Frame` shape to fall into (see
    :data:`_ALTAVISTA_AXES_BY_AXES_KIND`'s own doc comment). M17.1 (question 122): used
    to give an ingested CDM run's ``ScenarioData.frame`` its *real* ``origin``/``axes``
    (read off the run's own declared ``FrameDefinition``) instead of the
    origin="Earth"/axes="MJ2000Eq" ``altavista.model.Frame`` default every axes/body this
    function cannot place used to fall back to silently before this task.
    """
    if frame_def.WhichOneof("origin") != "body":
        return None
    axes = _ALTAVISTA_AXES_BY_AXES_KIND.get(frame_def.axes)
    if axes is None:
        return None
    return Frame(name=frame_def.id, origin=frame_def.body, axes=axes)


def frames_to_viewer_json(frame_defs: Sequence[core_pb2.FrameDefinition]) -> List[dict]:
    """``RunProducts.frames`` (or any sequence of already-declared ``FrameDefinition``s)
    -> the additive ``frames`` list ``ScenarioData.to_dict()`` carries (M4.1, question
    78) -- the *same* wire shape ``altavista/scenario.py``'s ``Scenario._build_frames``
    (read-only reference for this task) produces for a Python-built scenario: one
    protobuf-JSON-transcoded (``google.protobuf.json_format.MessageToDict``, default
    settings -- camelCase, enum names as strings) dict per frame, each one having gone
    through :class:`altavista.frames.FrameRegistry` for real GMAT validation and question
    76's deterministic ``parent_frame_id`` fill.

    M17.1 (question 122, docs/open-questions.md): the decided contract is
    "``RunProducts.frames`` carries the ``FrameDefinition``s every ``Trajectory.
    frame_id`` in the bundle resolves against ... and the altavista ingest builds the
    scene's frame list from them through the existing ``FrameRegistry``; a consumer
    needs nothing outside the bundle to place a trajectory." This function is that
    ingest step, called by ``altavista/server.py``'s ``POST /api/cdm/run`` (and, with
    frames supplied alongside via its JSON envelope, ``POST /api/cdm/trajectory``) --
    it never adds a frame ``frame_defs`` did not itself declare (in particular it does
    **not** invent an ``AXES_KIND_ICRF`` sibling for a body that only declared
    ``AXES_KIND_MJ2000_EQ`` -- the golden maneuver DRM's own ``RunProducts.frames`` has
    exactly one entry, ``EarthMJ2000Eq``, verified directly off a real ``av-run``
    binary; see this task's own report for why "the viewer offers ICRF" therefore only
    holds for a bundle whose producer actually declared an ICRF frame).

    Body-axes entries (``WhichOneof("origin") == "body"``, ``axes`` one of
    ICRF/MJ2000Eq/MJ2000Ec/BodyFixed -- the only kind
    ``av_kernel::drm::executor::collect_frames`` derives today, per that Rust
    function's own doc comment) are registered first, in ``id`` order; every other
    kind (ENU/NED, RIC/VNB/VVLH, PLATFORM_BODY, LOCAL_CARTESIAN) registers afterwards,
    also in ``id`` order -- so an entity-relative frame's auto-filled parent (question
    76: the reference body's MJ2000Eq frame) reuses an already-registered body-axes
    match instead of registering a redundant canonical one under a different id, the
    same ordering concern ``_build_frames`` handles by processing its own three
    sources (scenario frame, central-body frames, entity-relative declarations) in
    that fixed order. Every ``frame_defs`` entry has a distinct ``id`` on the wire
    (``RunProducts.frames`` is built server-side from a Rust ``BTreeMap<String,
    FrameDefinition>`` keyed by id -- ``crates/av-kernel/src/drm/executor.rs``'s
    ``collect_frames``), so unlike ``_build_frames`` this function does not need to
    dedupe two *different* ids describing the same ``(axes, body)`` pair.

    Raises :class:`CdmAdapterError` (never :class:`altavista.frames.FrameError`
    directly, so every caller of this module only ever has one exception hierarchy to
    catch, matching every other function here) if GMAT rejects a definition -- e.g. an
    unknown reference entity, a missing required field, or an ``AxesKind`` this
    process's GMAT build cannot realize. Never substitutes a validated definition for
    a rejected one.

    **Known, documented gap** (not papered over -- see this task's own honesty
    section): an entity-relative RIC/VNB/VVLH ``FrameDefinition`` arriving here
    registers correctly (validated against GMAT, ``parent_frame_id`` filled) but never
    gets the additive ``originTrack`` wire extension (``altavista/model.py``'s
    ``ScenarioData`` docstring) that animates a moving-origin frame node in the
    viewer -- unlike ``_build_frames``, which has direct access to the declaring
    ``Scenario``'s own recorded per-entity trajectory to sample origin motion from,
    this function sees only the ``FrameDefinition``s themselves, never a per-entity
    trajectory to sample. This is consistent with, not an additional gap on top of,
    the limitation already flagged in question 122's own decision text and
    ``crates/av-kernel/src/drm/executor.rs``'s ``collect_frames`` doc comment:
    ``av-kernel`` does not derive a RIC/VNB/VVLH ``FrameDefinition`` at all today (the
    DRM YAML loader still refuses a non-empty ``Scenario.frames`` --
    ``crates/av-kernel/src/drm/schema.rs``'s ``refuse_if_nonempty``), so no producer
    this function has ever been exercised against sends one; a future producer that
    does would get a frame graph node with no motion until this function is extended
    to accept per-entity tracks the way ``_build_frames`` does.
    """
    from . import frames as frames_mod
    from google.protobuf import json_format

    registry = frames_mod.FrameRegistry()
    ordered = sorted(frame_defs, key=lambda fd: fd.id)
    body_axes = [fd for fd in ordered if fd.WhichOneof("origin") == "body" and fd.axes in _ALTAVISTA_AXES_BY_AXES_KIND]
    body_axes_ids = {fd.id for fd in body_axes}
    others = [fd for fd in ordered if fd.id not in body_axes_ids]

    registered: List[core_pb2.FrameDefinition] = []
    try:
        for fd in body_axes + others:
            copy = core_pb2.FrameDefinition()
            copy.CopyFrom(fd)  # register() mutates in place (gmat_name, parent_frame_id); never the caller's own fd
            registered.append(registry.register(copy))
    except frames_mod.FrameError as exc:
        raise CdmAdapterError(f"cannot register frame {fd.id!r} from RunProducts.frames: {exc}") from exc

    registered.sort(key=lambda fd: fd.id)  # determinism: explicit sort on the output, not registration order
    return [json_format.MessageToDict(fd) for fd in registered]


# --------------------------------------------------------------------------- bodies (M18.2)
def bodies_from_frames(frame_defs: Sequence[core_pb2.FrameDefinition], *, frame: Frame,
                       span: Optional[Tuple[float, float]] = None,
                       body_samples: int = 2000) -> List[BodyTrack]:
    """The scene's body list, derived from the origin bodies ``RunProducts.frames``
    actually names (M18.2, ``docs/open-questions.md`` question 125's decided contract:
    "body identity comes from the wire ... never invented").

    Every ``FrameDefinition`` whose ``origin`` oneof is ``body`` names one celestial
    body; a frame naming ``platform_id`` or ``entity_id`` names none and is simply
    skipped here (not an error -- only the ``body`` case can name one at all, per
    ``proto/altavista/v1/core.proto``'s own ``FrameDefinition.origin`` doc comment).
    ``frame_defs`` entries are processed in ``id`` order (the same explicit-sort
    determinism rule :func:`frames_to_viewer_json` already follows: this module has no
    ``BTreeMap`` of its own), and a body named by more than one frame -- e.g. a bundle
    declaring both ``EarthMJ2000Eq`` and a sibling ``EarthICRF``, both origin
    ``"Earth"`` -- is sampled exactly once, not once per frame naming it. A body no
    frame names is never added: this deliberately does **not** call
    ``altavista.scenario.Scenario.default_bodies()`` (which adds the frame's own origin
    plus a Sun/Luna convenience default for a Python-authored scenario) -- that
    convenience list has no wire justification for a CDM-ingested run, only
    ``RunProducts.frames`` does.

    Positions/orientations come from the *existing* ``altavista.bodies.BodySampler``
    (the same sampler ``altavista.scenario.Scenario.build()`` already uses for a
    Python-authored scenario -- no second sampler is written here), sampled at
    ``altavista.bodies.sample_times(span[0], span[1], max_samples=body_samples)`` and
    expressed in ``frame`` (the scene's own entities frame, so a body drawn here sits
    in exactly the frame the ingested trajectories are drawn in). ``span=None`` (no
    spacecraft in the bundle, so no epoch window to sample over) yields body entries
    with no ``t``/``pos``/``quat`` samples, exactly like ``BodySampler.track()``
    already does for an empty ``times`` list -- not an error, since a body's static
    fields (radius, texture, color) are still meaningful with no trajectory to pace it.

    Raises :class:`UnknownBodyError` -- a typed 4xx at the HTTP layer, never a silent
    skip or an unhandled GMAT exception -- if a named body is not one this process's
    GMAT solar system has data for.
    """
    from .bodies import BodySampler, sample_times

    names: List[str] = []
    for fd in sorted(frame_defs, key=lambda fd: fd.id):
        if fd.WhichOneof("origin") != "body" or not fd.body:
            continue
        if fd.body not in names:
            names.append(fd.body)
    if not names:
        return []

    times = sample_times(span[0], span[1], max_samples=body_samples) if span else []
    sampler = BodySampler(frame)
    tracks: List[BodyTrack] = []
    for name in names:
        try:
            tracks.append(sampler.track(name, times, central=(name == frame.origin)))
        except Exception as exc:  # GMAT's own exception type varies by build
            raise UnknownBodyError(name, str(exc)) from exc
    return tracks


# --------------------------------------------------------------------------- entities
def entity_for(name: str, *, entity_id: Optional[str] = None) -> entity_pb2.Entity:
    """A spacecraft name -> a bare ``altavista.v1.Entity`` (``ENTITY_KIND_SPACECRAFT``).

    ``provenance`` is left unset (all-zero) here; :func:`scenario_to_cdm` stamps it once
    the bundle's ``config_hash`` is known. A caller using this function on its own gets an
    ``Entity`` with no ``Provenance`` rather than one carrying a made-up ``config_hash``.
    """
    return entity_pb2.Entity(id=entity_id or name, kind=entity_pb2.ENTITY_KIND_SPACECRAFT, name=name)


# --------------------------------------------------------------------------- provenance
def _provenance(*, tool: str, config_hash: str = "", principal: str = "",
                created_tai_ns: Optional[int] = None, run_id: str = "") -> core_pb2.Provenance:
    p = core_pb2.Provenance(author_kind=core_pb2.AUTHOR_KIND_SERVICE, tool=tool)
    if config_hash:
        p.config_hash = config_hash
    if principal:
        p.principal = principal
    if run_id:
        p.run_id = run_id
    if created_tai_ns is not None:
        p.created_tai_ns = int(created_tai_ns)
    return p


# --------------------------------------------------------------------------- state spaces (M7.1)
def _cartesian_pos_vel_6(state_space_id: str) -> core_pb2.StateSpace:
    labels_units = [
        ("pos_x", core_pb2.UNIT_METER), ("pos_y", core_pb2.UNIT_METER), ("pos_z", core_pb2.UNIT_METER),
        ("vel_x", core_pb2.UNIT_METER_PER_SECOND), ("vel_y", core_pb2.UNIT_METER_PER_SECOND),
        ("vel_z", core_pb2.UNIT_METER_PER_SECOND),
    ]
    return core_pb2.StateSpace(
        id=state_space_id,
        components=[core_pb2.StateComponent(label=label, unit=unit) for label, unit in labels_units])


def _cartesian_pos_vel_6_attitude_quat_4(state_space_id: str) -> core_pb2.StateSpace:
    space = _cartesian_pos_vel_6(state_space_id)
    space.components.extend([
        core_pb2.StateComponent(label="q_x", unit=core_pb2.UNIT_DIMENSIONLESS),
        core_pb2.StateComponent(label="q_y", unit=core_pb2.UNIT_DIMENSIONLESS),
        core_pb2.StateComponent(label="q_z", unit=core_pb2.UNIT_DIMENSIONLESS),
        core_pb2.StateComponent(label="q_w", unit=core_pb2.UNIT_DIMENSIONLESS),
    ])
    return space


def _native_controller_scalar6(state_space_id: str) -> core_pb2.StateSpace:
    """M20.1 (question 133): six independent, unitless scalars -- a non-physical native
    instance's own state space (no position, no velocity; see `has_position_class` below).
    Matches `crates/av-kernel/src/trajectory.rs`'s identically-named builder and
    `drms/demo_two_instance_ctrl.system.yaml`'s own declared `state_space` component-for-
    component (same labels, same order, same unit)."""
    return core_pb2.StateSpace(
        id=state_space_id,
        components=[core_pb2.StateComponent(label=f"state_{i}", unit=core_pb2.UNIT_DIMENSIONLESS) for i in range(1, 7)])


_STATE_SPACE_BUILDERS = {
    STATE_SPACE_ID_CARTESIAN_POS_VEL_6: _cartesian_pos_vel_6,
    STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4: _cartesian_pos_vel_6_attitude_quat_4,
    # M20.1 (question 133): `gmat.orbital.cartesian6` is a second, pre-existing id for the
    # identical 6-component Cartesian shape (see `GMAT_ORBITAL_CARTESIAN6_ID`'s own doc
    # comment above) -- every real GMAT-bound `RunProducts.trajectories` entry `av-run`
    # produces names this id, not `STATE_SPACE_ID_CARTESIAN_POS_VEL_6`.
    GMAT_ORBITAL_CARTESIAN6_ID: _cartesian_pos_vel_6,
    NATIVE_CONTROLLER_SCALAR6_ID: _native_controller_scalar6,
}

# `crates/av-kernel/src/interpolate.rs::POSITION_VELOCITY_LABELS`/`POSITION_VELOCITY_UNITS`,
# mirrored here component-for-component (M20.1, question 133) so `has_position_class` below
# tests the identical "first six components are exactly this Cartesian position/velocity
# prefix" rule `crate::interpolate::is_position_velocity_prefix` enforces on the Rust side --
# not a second, independently-invented convention.
_POSITION_VELOCITY_LABELS = ("pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z")
_POSITION_VELOCITY_UNITS = (
    core_pb2.UNIT_METER, core_pb2.UNIT_METER, core_pb2.UNIT_METER,
    core_pb2.UNIT_METER_PER_SECOND, core_pb2.UNIT_METER_PER_SECOND, core_pb2.UNIT_METER_PER_SECOND,
)


def has_position_class(space: core_pb2.StateSpace) -> bool:
    """Whether ``space`` -- a resolved ``altavista.v1.StateSpace``, not a bare id string --
    declares the Cartesian position/velocity prefix (ADR-005 sec 3, `crates/av-kernel/src/
    interpolate.rs::classify`'s ``ComponentClass::PositionVelocity``): its own first six
    components' labels and units, in order, exactly matching :data:`_POSITION_VELOCITY_LABELS`/
    :data:`_POSITION_VELOCITY_UNITS`.

    Question 133 (`docs/open-questions.md`, decided by the lead): "the viewer renders only
    instances whose state space has [a position class]" -- this is that test, read from the
    declared state space's own components, never from a hardcoded id string comparison (a
    future non-Cartesian id gets the correct answer here without this function needing to
    know its name).
    """
    if len(space.components) < 6:
        return False
    first_six = space.components[:6]
    return tuple(c.label for c in first_six) == _POSITION_VELOCITY_LABELS and tuple(c.unit for c in first_six) == _POSITION_VELOCITY_UNITS


def state_space_for(state_space_id: str) -> core_pb2.StateSpace:
    """The declared ``altavista.v1.StateSpace`` for ``state_space_id``: component labels
    and units, per M7.1 / question 88's condition (a) ("the state space must be a
    declared StateSpace message ... not an ad hoc id string"). Matches
    ``crates/av-kernel/src/trajectory.rs``'s ``state_space_for`` component-for-component
    (same labels, same order, same units) for the two ids both sides know about, so a
    `StateSpace` built here and one built there for the same id are the same
    declaration, not two independently-invented ones -- checked directly by
    ``tests/test_cdm_adapter.py``.

    Raises :class:`UnknownStateSpaceError` for any id this module has no declared shape
    for, rather than guessing one from the id string or from a sample's own length.
    ``StateSpace.frame_id`` is left unset on every returned message: these shapes are
    frame-independent (the same declared 6- or 10-component space is reused across many
    different ``Trajectory.frame_id``s), so filling it in here would be a guess, not a
    property of the state space itself.
    """
    builder = _STATE_SPACE_BUILDERS.get(state_space_id)
    if builder is None:
        raise UnknownStateSpaceError(state_space_id)
    return builder(state_space_id)


# --------------------------------------------------------------------------- trajectories
def trajectory_to_cdm(traj: Trajectory, *, entity_id: str, frame_id: str,
                      state_space_id: str = DEFAULT_STATE_SPACE_ID, trajectory_id: Optional[str] = None,
                      segment_name: Optional[str] = None, dynamics_model: str = "", dynamics_hash: str = "",
                      dynamics_depth: str = "", config_hash: str = "", tool: str = TOOL_NAME,
                      principal: str = "", created_tai_ns: Optional[int] = None,
                      run_id: str = "") -> trajectory_pb2.Trajectory:
    """An altavista :class:`~altavista.model.Trajectory` (km, km/s, A1MJD) ->
    ``altavista.v1.Trajectory`` (m, m/s, TAI ns).

    ``interpolation`` is always ``INTERPOLATION_HERMITE_VELOCITY`` -- what the viewer
    actually does (``web/js/interp.js``). ``TrajectorySample.cov`` is always left empty
    (see the module docstring's "covariance placeholder" section). ``TrajectorySample.kind``
    (question 116) is always ``SAMPLE_KIND_NATIVE``: every sample altavista emits here comes
    straight off GMAT's own report grid (``traj.t``/``traj.pos``/``traj.vel``), never
    interpolated or held by this adapter. Samples are sorted by epoch explicitly before
    being written, regardless of the input's own order, so the output never depends on
    incoming iteration order.

    **Attitude (M6.3).** When ``traj.attitude`` is populated (one ``[x, y, z, w]``
    scalar-last quaternion per sample, parallel to ``traj.t`` -- see
    ``altavista/model.py``'s ``Trajectory.attitude`` docstring for the convention), each
    sample's ``mean`` grows from 6 to 10 components (position xyz, velocity xyz,
    quaternion xyzw) rather than a new proto message or field: ``TrajectorySample.mean``
    is already a generic "length n, in the trajectory's state space and frame" vector
    (``proto/altavista/v1/trajectory.proto``), so this is purely a state-space-shape
    choice, not a wire-format change -- no ``trajectory.proto`` edit was needed for
    this. ``state_space_id`` is left exactly as the caller passed it if given
    explicitly; when the caller left it at the default (:data:`DEFAULT_STATE_SPACE_ID`)
    and ``traj.attitude`` is populated, it is upgraded to
    :data:`DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE` so the id keeps naming the actual
    shape on the wire.
    """
    has_attitude = bool(traj.attitude) and len(traj.attitude) == len(traj.t)
    if traj.attitude and not has_attitude:
        raise CdmAdapterError(
            f"Trajectory {traj.name!r} has {len(traj.attitude)} attitude sample(s) but "
            f"{len(traj.t)} state sample(s); attitude must be empty or exactly parallel "
            f"to t (altavista/model.py's Trajectory.attitude contract)")
    if has_attitude and state_space_id == DEFAULT_STATE_SPACE_ID:
        state_space_id = DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE
    cdm = trajectory_pb2.Trajectory(
        id=trajectory_id or traj.name,
        entity_id=entity_id,
        state_space_id=state_space_id,
        frame_id=frame_id,
        interpolation=trajectory_pb2.INTERPOLATION_HERMITE_VELOCITY,
        config_hash=config_hash,
    )
    attitude_rows = traj.attitude if has_attitude else [None] * len(traj.t)
    rows = sorted(zip(traj.t, traj.pos, traj.vel, attitude_rows), key=lambda row: row[0])
    for t, pos, vel, quat in rows:
        mean = [pos[0] * M_PER_KM, pos[1] * M_PER_KM, pos[2] * M_PER_KM,
               vel[0] * M_PER_KM, vel[1] * M_PER_KM, vel[2] * M_PER_KM]
        if quat is not None:
            mean += [quat[0], quat[1], quat[2], quat[3]]  # dimensionless; no unit conversion
        cdm.samples.add(tai_ns=a1mjd_to_tai_ns(t), mean=mean, kind=trajectory_pb2.SAMPLE_KIND_NATIVE)
    if rows:
        seg = cdm.segments.add(name=segment_name or traj.name,
                               start_tai_ns=cdm.samples[0].tai_ns, end_tai_ns=cdm.samples[-1].tai_ns)
        if dynamics_model:
            seg.dynamics_model = dynamics_model
        if dynamics_hash:
            seg.dynamics_hash = dynamics_hash
        if dynamics_depth:
            seg.dynamics_depth = dynamics_depth
    cdm.provenance.CopyFrom(_provenance(tool=tool, config_hash=config_hash, principal=principal,
                                        created_tai_ns=created_tai_ns, run_id=run_id))
    return cdm


def cdm_trajectory_to_viewer_json(cdm: trajectory_pb2.Trajectory, *, name: Optional[str] = None,
                                  color: Optional[str] = None, label: Optional[str] = None) -> Optional[Trajectory]:
    """The inverse of :func:`trajectory_to_cdm`: ``altavista.v1.Trajectory`` (m, m/s, TAI
    ns) -> an altavista :class:`~altavista.model.Trajectory` (km, km/s, A1MJD) the existing
    viewer JSON contract (``ScenarioData.to_dict()``) already knows how to render.

    Raises :class:`CdmAdapterError` if ``interpolation`` is set to anything other than
    ``INTERPOLATION_HERMITE_VELOCITY`` (or left ``UNSPECIFIED``) -- the viewer always
    interpolates with cubic Hermite-using-velocity (``web/js/interp.js``), so silently
    rendering, say, a linear-interpolation trajectory that way would misrepresent it. Also
    raises if a sample has fewer than 6 ``mean`` components (position + velocity).

    **Non-physical instances (M20.1, question 133): returns ``None``, not a zero-filled
    track.** When ``cdm.state_space_id`` resolves (:func:`state_space_for`) to a *known*
    state space that has no position class (:func:`has_position_class` -- e.g. a native
    controller's own ``native.controller.scalar6``, six unitless scalars, no position or
    velocity component at all), there is no position trajectory to render, so this returns
    ``None`` instead of misreading six non-positional scalars as km position/velocity. This
    is deliberately conservative in the other direction: an *unresolvable* id (one neither
    this module nor ``crates/av-kernel/src/trajectory.rs`` declares -- e.g. a synthetic
    fixture's own placeholder id, never a real ``av-run`` output) falls through to the
    unchanged, pre-M20.1 behaviour below (treated as carrying position/velocity in its first
    six components) rather than a new hard refusal this task's own scope never asked for.
    Callers iterating a ``RunProducts.trajectories`` map (``altavista/server.py``) must skip a
    ``None`` result when building the spacecraft list, while still keeping this instance's
    own events on the timeline (``RunProducts.events`` is entity-tagged independently of
    ``RunProducts.trajectories`` and is never filtered by this function).

    **Attitude (M6.3).** A sample whose ``mean`` has 10 or more components carries a
    scalar-last quaternion in components 6-9 (see :func:`trajectory_to_cdm`); it is
    copied unchanged (dimensionless, no unit conversion) into the returned
    :class:`~altavista.model.Trajectory`'s additive ``attitude`` list. Raises
    :class:`CdmAdapterError` if only *some* samples carry one -- ``Trajectory.attitude``
    must be empty or exactly parallel to ``t`` (``web/js/scene.js`` checks
    ``s.attitude.length === s.t.length * 4`` to decide whether to trust the stream), so
    a partially-attituded CDM trajectory is rejected rather than silently truncated or
    padded.
    """
    if cdm.interpolation not in (trajectory_pb2.INTERPOLATION_UNSPECIFIED, trajectory_pb2.INTERPOLATION_HERMITE_VELOCITY):
        raise CdmAdapterError(
            f"Trajectory {cdm.id!r} declares interpolation "
            f"{trajectory_pb2.Interpolation.Name(cdm.interpolation)}; the viewer always "
            f"interpolates with cubic Hermite-using-velocity, so rendering this trajectory "
            f"that way would misrepresent it")
    try:
        resolved_space = state_space_for(cdm.state_space_id)
    except UnknownStateSpaceError:
        resolved_space = None
    if resolved_space is not None and not has_position_class(resolved_space):
        return None
    tr = Trajectory(name=name or cdm.entity_id or cdm.id or "cdm_trajectory", color=color, label=label)
    samples = sorted(cdm.samples, key=lambda sample: sample.tai_ns)
    n_with_attitude = 0
    for s in samples:
        if len(s.mean) < 6:
            raise CdmAdapterError(
                f"Trajectory {cdm.id!r} sample at tai_ns={s.tai_ns} has {len(s.mean)} mean "
                f"component(s); the viewer needs at least 6 (position xyz + velocity xyz)")
        t = tai_ns_to_a1mjd(s.tai_ns)
        tr.append(t, [s.mean[0] / M_PER_KM, s.mean[1] / M_PER_KM, s.mean[2] / M_PER_KM,
                      s.mean[3] / M_PER_KM, s.mean[4] / M_PER_KM, s.mean[5] / M_PER_KM])
        if len(s.mean) >= 10:
            tr.attitude.append([s.mean[6], s.mean[7], s.mean[8], s.mean[9]])
            n_with_attitude += 1
    if n_with_attitude not in (0, len(samples)):
        raise CdmAdapterError(
            f"Trajectory {cdm.id!r} has {n_with_attitude} sample(s) with a >=10-component "
            f"mean (attitude) out of {len(samples)} total; attitude must be present on "
            f"every sample or none")
    return tr


# --------------------------------------------------------------------------- events
_EVENT_KIND_BY_TYPE = {
    "maneuver": trajectory_pb2.EVENT_KIND_MANEUVER,
    "marker": trajectory_pb2.EVENT_KIND_MARKER,
}
# Matches exactly Scenario.maneuver's `detail=f"dv = {mag * 1000:.2f} m/s ({frame})"`
# (altavista/scenario.py). `mag` there is already m/s (km/s * 1000), so the parsed number is
# SI as-is -- no further unit conversion happens here.
_DV_DETAIL_RE = re.compile(r"dv\s*=\s*([+-]?\d+(?:\.\d+)?)\s*m/s\s*\([^)]*\)\s*", re.IGNORECASE)


def _parse_dv_detail(detail: str) -> Optional[Dict[str, float]]:
    """Best-effort, reliable-only parse of altavista's ``"dv = 20.00 m/s (VNB)"`` maneuver
    ``Event.detail`` into structured SI ``values``. Returns ``None`` (never a guess) for
    any string that is not exactly this altavista-generated shape -- in particular the
    free-form multi-line text ``Scenario._event_from_summary`` copies out of a GMAT
    command summary, which this function does not attempt to parse."""
    m = _DV_DETAIL_RE.fullmatch(detail.strip())
    if not m:
        return None
    return {"dv_mps": float(m.group(1))}


_VIEWER_EVENT_TYPE_BY_KIND = {
    trajectory_pb2.EVENT_KIND_MANEUVER: "maneuver",
    trajectory_pb2.EVENT_KIND_FAULT: "fault",
    trajectory_pb2.EVENT_KIND_LIFECYCLE: "lifecycle",
    trajectory_pb2.EVENT_KIND_MARKER: "marker",
    # M19.3 (docs/open-questions.md question 130): an applied SIGNAL port command
    # (crates/av-kernel/src/drm/events.rs::port_command_event) -- a distinct kind from
    # every one above, never conflated with "lifecycle"/"fault"/"maneuver" (the demo's
    # own `dropped_in_flight_messages` lifecycle event is a different thing entirely: a
    # count of messages never delivered, not a record of one that was). Explicit here
    # rather than left to the enum-name fallback below (which would already produce the
    # identical "port_command" label) so the mapping documents intent instead of relying
    # on introspection for a kind this module now actually expects to see.
    trajectory_pb2.EVENT_KIND_PORT_COMMAND: "port_command",
}


def cdm_event_to_viewer_event(ev: trajectory_pb2.Event) -> Event:
    """The inverse of :func:`event_to_cdm`: ``altavista.v1.Event`` -> an altavista
    :class:`~altavista.model.Event` the existing viewer JSON contract already renders on its
    event list and timeline ticks (``web/js/app.js``'s ``buildLists``/``buildTicks``, which
    treat ``Event.type`` as an opaque label -- no per-kind rendering branch to keep in sync
    here).

    M16.3 (question 5's first demo bridge): this is the one direction :func:`event_to_cdm`
    never needed before -- altavista only ever *produced* CDM events, never had to read one
    back that the Rust DRM executor (``crates/av-kernel/src/drm/events.rs``) emitted.
    :data:`_VIEWER_EVENT_TYPE_BY_KIND` covers the three kinds that module actually emits
    today (``EVENT_KIND_MANEUVER``, ``EVENT_KIND_FAULT``, ``EVENT_KIND_LIFECYCLE``) plus
    ``EVENT_KIND_MARKER`` (:func:`event_to_cdm`'s own default for an altavista event with no
    declared ``type``, kept symmetric). Any other ``EventKind`` -- including
    ``EVENT_KIND_UNSPECIFIED`` -- is never coerced into one of those four labels: this
    function falls back to the enum's own name, lowercased and with the ``EVENT_KIND_``
    prefix stripped (e.g. ``EVENT_KIND_CONTACT_START`` -> ``"contact_start"``), so a kind
    this function does not special-case is still labelled honestly rather than silently
    misreported as ``"marker"``. ``t`` converts ``tai_ns`` through :func:`tai_ns_to_a1mjd`
    (the same epoch conversion :func:`cdm_trajectory_to_viewer_json` uses); ``detail`` and
    ``entity_id`` (-> ``spacecraft``) are carried through unchanged. ``values`` (the
    structured SI fields the executor attaches, e.g. a maneuver's ``dv_mps``) has no slot on
    :class:`~altavista.model.Event` -- that dataclass's own ``detail`` free-text field is what
    the viewer displays; nothing is lost silently since ``detail`` (built by
    ``crates/av-kernel/src/drm/events.rs`` from the same values) already states the number in
    human-readable form (e.g. the maneuver detail's own summary) for every kind this
    function maps.

    M25.3e (question 177): ``reference_id`` and ``provenance.attributes`` are now also
    carried through (-> ``referenceId``/``attributes`` on the wire) -- see
    :class:`~altavista.model.Event`'s own docstring for the exact contract. Before this
    task, both were dropped here, forcing ``web/js/timeline_events.js`` to recover a
    command transition's id and ack level (when at all possible) from ``detail``'s free
    text; that recovery code stays as a fallback (a scenario with no ``referenceId``,
    e.g. one built before this task, still degrades gracefully) but is no longer the
    primary path.
    """
    label = _VIEWER_EVENT_TYPE_BY_KIND.get(ev.kind)
    if label is None:
        name = trajectory_pb2.EventKind.Name(ev.kind)
        label = name[len("EVENT_KIND_"):].lower() if name.startswith("EVENT_KIND_") else "custom"
    # M25.3e (question 177): ev.reference_id (for a command transition, the Command.id)
    # and ev.provenance.attributes (where ack_level lives on a command's ACKED
    # transition) now reach the viewer -- see altavista.model.Event's own docstring
    # section on this. Both additive: reference_id defaults to "" on the proto (-> None
    # here, matching every other optional string field this function already maps),
    # attributes defaults to an empty map (never None -- Event.attributes' own default).
    return Event(name=ev.name or ev.id, t=tai_ns_to_a1mjd(ev.tai_ns), type=label,
                spacecraft=ev.entity_id or None, detail=ev.detail or None,
                reference_id=ev.reference_id or None, attributes=dict(ev.provenance.attributes))


def event_to_cdm(ev: Event, *, entity_id: Optional[str] = None, event_id: Optional[str] = None,
                 frame_id: str = "", config_hash: str = "", tool: str = TOOL_NAME, principal: str = "",
                 created_tai_ns: Optional[int] = None, run_id: str = "") -> trajectory_pb2.Event:
    """An altavista :class:`~altavista.model.Event` -> ``altavista.v1.Event``.

    ``detail`` is always carried through unchanged. ``values`` is populated only when
    ``detail`` matches altavista's own maneuver-detail shape exactly (see
    :func:`_parse_dv_detail`); otherwise ``values`` is left empty rather than guessed.
    """
    kind = _EVENT_KIND_BY_TYPE.get(ev.type, trajectory_pb2.EVENT_KIND_CUSTOM if ev.type else trajectory_pb2.EVENT_KIND_MARKER)
    cdm = trajectory_pb2.Event(
        id=event_id or f"{ev.name}@{a1mjd_to_tai_ns(ev.t)}",
        entity_id=entity_id or ev.spacecraft or "",
        tai_ns=a1mjd_to_tai_ns(ev.t),
        kind=kind,
        name=ev.name,
        detail=ev.detail or "",
    )
    if frame_id:
        cdm.frame_id = frame_id
    values = _parse_dv_detail(ev.detail) if ev.detail else None
    if values:
        for k in sorted(values):
            cdm.values[k] = values[k]
    cdm.provenance.CopyFrom(_provenance(tool=tool, config_hash=config_hash, principal=principal,
                                        created_tai_ns=created_tai_ns, run_id=run_id))
    return cdm


# --------------------------------------------------------------------------- config hash
def _sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _config_hash(frame_def: core_pb2.FrameDefinition, entities: Sequence[entity_pb2.Entity],
                 state_space_id: str, tool: str) -> str:
    """A stable SHA-256 over the GMAT-adapter configuration that produced a CDM bundle.

    proto/altavista/v1 has no dedicated "adapter config" message (adding one is out of
    scope for this worker -- proto/** is not owned here), so the hash input is assembled
    from the deterministic protobuf serialization (``SerializeToString(deterministic=True)``,
    the same pattern ``tests/test_cdm_v1.py::test_drm_with_ric_frame_and_mixed_bindings_round_trips``
    uses) of the pieces that actually define the config -- the frame definition and the
    entity list -- length-prefixed and concatenated (so two different splits can never
    collide), plus the state-space id and tool name as plain UTF-8. Entities are sorted by
    id first, so the hash never depends on list/dict ordering (per the "determinism" rule).
    """
    parts = [frame_def.SerializeToString(deterministic=True), state_space_id.encode("utf-8"), tool.encode("utf-8")]
    for e in sorted(entities, key=lambda ent: ent.id):
        parts.append(e.SerializeToString(deterministic=True))
    buf = b"".join(len(p).to_bytes(8, "big") + p for p in parts)
    return _sha256_hex(buf)


# --------------------------------------------------------------------------- bundle
@dataclass
class CdmBundle:
    """Everything :func:`scenario_to_cdm` produces from one altavista
    :class:`~altavista.model.ScenarioData`."""
    frame: core_pb2.FrameDefinition
    entities: List[entity_pb2.Entity] = field(default_factory=list)
    trajectories: List[trajectory_pb2.Trajectory] = field(default_factory=list)
    events: List[trajectory_pb2.Event] = field(default_factory=list)
    # Additive, M7.1 (question 88): the declared StateSpace for every state_space_id
    # actually used by `trajectories` above, sorted by id (determinism -- this module has
    # no BTreeMap, so an explicit sort stands in for one, per this task's "sort explicitly"
    # rule). Populated by scenario_to_cdm; a caller building a Trajectory directly through
    # trajectory_to_cdm (bypassing scenario_to_cdm) can still get its declared StateSpace
    # by calling state_space_for(traj.state_space_id) itself.
    state_spaces: List[core_pb2.StateSpace] = field(default_factory=list)
    config_hash: str = ""


def scenario_to_cdm(scenario_data: ScenarioData, *, state_space_id: str = DEFAULT_STATE_SPACE_ID,
                    tool: str = TOOL_NAME, principal: str = "", created_tai_ns: Optional[int] = None,
                    run_id: str = "") -> CdmBundle:
    """An altavista :class:`~altavista.model.ScenarioData` -> a :class:`CdmBundle`: the
    scenario's frame, one ``Entity``/``Trajectory`` per spacecraft, and one ``Event`` per
    altavista event, all sharing one stable ``config_hash``.

    Spacecraft and events are sorted explicitly (by name, and by ``(epoch, name)``
    respectively) before conversion, so the bundle -- including ``config_hash`` -- does
    not depend on ``ScenarioData.spacecraft`` / ``.events`` list order. Building the same
    ``ScenarioData`` twice (independently) yields the same ``config_hash``.
    """
    frame_def = frame_definition_for(scenario_data.frame)
    trajs = sorted(scenario_data.spacecraft, key=lambda tr: tr.name)
    entities = [entity_for(tr.name) for tr in trajs]

    config_hash = _config_hash(frame_def, entities, state_space_id, tool)

    for e in entities:
        e.provenance.CopyFrom(_provenance(tool=tool, config_hash=config_hash, principal=principal,
                                          created_tai_ns=created_tai_ns, run_id=run_id))

    cdm_trajectories = [
        trajectory_to_cdm(tr, entity_id=tr.name, frame_id=frame_def.id, state_space_id=state_space_id,
                          config_hash=config_hash, tool=tool, principal=principal,
                          created_tai_ns=created_tai_ns, run_id=run_id)
        for tr in trajs
    ]

    events = sorted(scenario_data.events, key=lambda ev: (ev.t, ev.name))
    cdm_events = [
        event_to_cdm(ev, entity_id=ev.spacecraft, config_hash=config_hash, tool=tool, principal=principal,
                     created_tai_ns=created_tai_ns, run_id=run_id)
        for ev in events
    ]

    # M7.1 (question 88): declare a StateSpace for every state_space_id this bundle's
    # trajectories actually ended up with -- trajectory_to_cdm's own per-trajectory
    # attitude upgrade (has_attitude) means that can differ from the `state_space_id`
    # argument above, so this reads each trajectory's *own* final id rather than assuming
    # they all match the bundle-level default. Sorted for determinism (this module's
    # "no BTreeMap, sort explicitly" rule).
    used_ids = sorted({tr.state_space_id for tr in cdm_trajectories if tr.state_space_id})
    state_spaces = [state_space_for(sid) for sid in used_ids]

    return CdmBundle(frame=frame_def, entities=entities, trajectories=cdm_trajectories, events=cdm_events,
                     state_spaces=state_spaces, config_hash=config_hash)
