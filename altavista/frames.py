"""The frame service: validates CDM ``FrameDefinition``s against GMAT and converts
states between registered frames (ADR-001, `docs/architecture.md` section 2).

GMAT's ``CoordinateSystem`` is the reference implementation and the validator (per
ADR-001): :class:`FrameRegistry` builds the actual GMAT object a ``FrameDefinition``
names and refuses definitions GMAT cannot realize -- it never approximates one axes
kind with another.

AxesKind -> GMAT realization table
-----------------------------------

======================= ==================================================================
AxesKind                 GMAT realization
======================= ==================================================================
``ICRF``                 ``CoordinateSystem`` with ``Axes = ICRF``, ``Origin = <body>``.
``MJ2000_EQ``             ``Axes = MJ2000Eq``, ``Origin = <body>``.
``MJ2000_EC``             ``Axes = MJ2000Ec``, ``Origin = <body>``.
``BODY_FIXED``            ``Axes = BodyFixed``, ``Origin = <body>``.
``ENU``                   A ``GroundStation`` at ``origin_geodetic`` (``StateType =
                          Spherical``, ``HorizonReference = Ellipsoid``, ``Location1/2/3 =
                          lat_deg/lon_deg/alt_km``) plus a ``CoordinateSystem`` with
                          ``Axes = Topocentric`` about it -- GMAT's *only* local-horizon
                          axes kind, which is the classical geodesy **SEZ** convention
                          (South, East, Zenith/Up: y=East, z=Up, x completes the
                          right-handed set = East x Up = -North), **not ENU**. altavista
                          applies a fixed, documented rotation on top of GMAT's output to
                          produce true ENU ordering -- see ``_SEZ_TO_ENU`` below.
``NED``                   Same GroundStation/Topocentric (SEZ) realization as ENU, with the
                          complementary fixed rotation ``_SEZ_TO_NED`` applied instead.
``RIC``                   ``Axes = ObjectReferenced``, ``Primary = reference_body``,
                          ``Secondary = reference_entity_id``, ``XAxis = R``, ``ZAxis = N``
                          (GMAT derives ``YAxis = ZAxis x XAxis = N x R``), ``Origin =
                          entity_id``. Matches core.proto's stated convention (X=R, Z=N).
``VNB``                   ``Axes = ObjectReferenced``, same Primary/Secondary/Origin as
                          RIC, ``XAxis = V``, ``YAxis = N`` (GMAT derives ``ZAxis = XAxis x
                          YAxis = V x N``, the binormal). Matches core.proto (X=V, Y=N).
``VVLH``                  ``Axes = ObjectReferenced``, same Primary/Secondary/Origin
                          pattern, ``YAxis = -N``, ``ZAxis = -R`` (GMAT derives ``XAxis =
                          YAxis x ZAxis = N x R``, in-track). This realizes core.proto's
                          ``AXES_KIND_VVLH`` (Z = -R nadir, Y = -N, X = N x R in-track,
                          question 73 -- renamed from ``AXES_KIND_LVLH`` by question 106 so
                          the name would never collide with GMAT's own, *different*,
                          ``ImpulsiveBurn Axes = LVLH`` literal local burn axes (X=R, Y=N x R,
                          Z=N -- numerically this table's ``RIC`` row, not this one; see
                          ``altavista/scenario.py::Scenario._fire_impulsive_burn``'s own doc
                          comment and ``FRAMES.md``'s "GMAT ImpulsiveBurn LVLH vs
                          AXES_KIND_VVLH" section). GMAT's ``CoordinateSystem`` object itself
                          has no ``VVLH``/``LVLH`` ``Axes`` value at all (it is realized here
                          as ``ObjectReferenced`` like RIC/VNB) -- only ``ImpulsiveBurn`` has
                          the separate, literal, unrelated ``Axes = LVLH`` string above.
``PLATFORM_BODY``        **Declared, not convertible, when ``attitude_source`` is set**
                          (question 72). :meth:`FrameRegistry.register` validates the
                          referenced entity exists and, for a ``gmat_attitude_model``
                          source, that GMAT can actually construct that attitude type
                          (built on a scratch GMAT ``Spacecraft`` -- see
                          ``_validate_gmat_attitude_model``); it never builds an
                          ``AttitudeModel`` on the real entity, so there is no native
                          GMAT ``CoordinateSystem`` behind this frame.
                          :meth:`FrameRegistry.convert` raises
                          :class:`AttitudeServiceUnavailableError` for it -- distinct from
                          :class:`FrameNotRealizableError`, because the frame *is*
                          legitimately declared; only realizing the attitude for
                          conversion is deferred to the attitude service (P2). A
                          ``PLATFORM_BODY`` definition **without** ``attitude_source``
                          still raises :class:`FrameNotRealizableError` unconditionally
                          (there is nothing to validate against).
``LOCAL_CARTESIAN``       **No GMAT object.** Frameless by design (simulation/unit-test
                          space). :meth:`FrameRegistry.register` accepts it trivially (there
                          is nothing to validate) and sets a fixed sentinel ``gmat_name``;
                          :meth:`FrameRegistry.convert` refuses to convert into or out of it
                          -- there is no defined mapping, and inventing one (e.g. treating it
                          as an alias for the state's numeric frame) would be exactly the
                          kind of silent fallback this module must not do.
======================= ==================================================================

``parent_frame_id`` fill (question 76)
---------------------------------------
:meth:`FrameRegistry.register` fills ``FrameDefinition.parent_frame_id`` deterministically
when it arrives empty (proto3 gives ``parent_frame_id`` no presence tracking, so "empty" and
"omitted" are the same wire value -- see :meth:`_fill_parent_frame_id`'s docstring), and
rejects a non-empty supplied value that disagrees with the rule with
:class:`InconsistentParentFrameError`. The rule, by ``AxesKind``:

* ``ICRF`` / ``MJ2000_EQ`` / ``MJ2000_EC`` / ``BODY_FIXED`` / ``LOCAL_CARTESIAN`` -> ``""``
  (registry root). Body-centred frames are the top of the tree by construction; a frameless
  ``LOCAL_CARTESIAN`` frame has no body, entity or geodetic origin to derive a parent from,
  so treating it as anything but root would imply a relationship it does not have.
* ``ENU`` / ``NED`` -> the body's ``BODY_FIXED`` frame (``origin_geodetic.body``),
  registered automatically under a canonical id if no matching frame is registered yet.
* ``RIC`` / ``VNB`` / ``VVLH`` -> the reference body's ``MJ2000_EQ`` frame
  (``reference_body``), registered automatically the same way.
* ``PLATFORM_BODY`` -> ``attitude_source.reference_frame_id`` verbatim -- the schema
  already names exactly this ("frame the attitude is expressed relative to"), so it is the
  most precise parent available and altavista does not invent a different one. Required
  non-empty (:class:`MissingFieldError` otherwise); *not* auto-registered (unlike the
  ENU/NED and RIC/VNB/VVLH cases, an attitude reference frame is not derivable from axes
  alone) -- a dangling reference surfaces as :class:`FrameParentMissingError` from
  :meth:`FrameRegistry.path_to_root`, not at ``register()`` time, since frames may be
  registered in either order.

Auto-registering an ENU/NED or RIC/VNB/VVLH parent reuses any already-registered frame with
matching ``AxesKind`` and body (picking the lexicographically smallest id if more than one
matches, so this never depends on dict insertion order); only when none exists does it
register a new one under a canonical ``gv_auto_parent_<body>_<Axes>`` id.

Tree operations
----------------
:meth:`FrameRegistry.children` and :meth:`FrameRegistry.path_to_root` expose the
``parent_frame_id`` tree for the viewer's ``frames.js`` (via the scene service's JSON).
``children`` sorts its result lexicographically by id; ``path_to_root`` raises
:class:`FrameCycleError` or :class:`FrameParentMissingError` instead of recursing forever
on a malformed chain.

Units and the epoch type (read this before calling :meth:`FrameRegistry.convert`)
------------------------------------------------------------------------------------
* :meth:`FrameRegistry.register` takes and returns CDM ``FrameDefinition`` protos, whose
  ``Geodetic`` field is SI per ADR-001: ``latitude_rad``/``longitude_rad`` in radians,
  ``height_m`` in metres. GMAT wants degrees and kilometres. That conversion happens in
  exactly one place, :func:`_geodetic_to_gmat`, matching ADR-001's rule that kilometres
  (and, here, degrees) exist only inside the GMAT adapter.
* :meth:`FrameRegistry.convert` takes and returns a 6-vector ``[x, y, z, vx, vy, vz]`` in
  **metres** and **metres/second** -- the CDM's units -- even though GMAT itself works in
  km and km/s internally; that round trip also happens in exactly one place, inside
  ``convert``.
* :meth:`FrameRegistry.convert`'s epoch parameter is named ``epoch_a1mjd`` and is a GMAT
  **A1MJD float**, deliberately -- **not** a CDM ``epoch_ns`` (TAI nanoseconds). ADR-001
  says GMAT's A1 scale is TAI + 0.0343817 s and is converted "inside the GMAT adapter
  only", via a shared, versioned leap-second table (worker A's Rust crate + shared
  ``data/time/leap_seconds.json``) that does not exist yet at the time this module was
  written. A TAI-ns-facing wrapper around this method is out of scope here (M1.3, once
  that table lands); this module must not invent its own leap-second table to fill the
  gap in the meantime.
"""
from __future__ import annotations

import math
import re
from typing import Dict, List, Optional, Sequence

import numpy as np

from .bodies import Frame as _BodyFrame
from .bodies import coordinate_system as _body_axes_cs
from .gmat_env import gmat
from .pb import core_pb2

AxesKind = core_pb2.AxesKind

M_PER_KM = 1000.0
_DEG_PER_RAD = 180.0 / math.pi


# --------------------------------------------------------------------------- exceptions
class FrameError(Exception):
    """Base class for every error :class:`FrameRegistry` raises."""


class UnknownAxesKindError(FrameError):
    """``FrameDefinition.axes`` is unset or not a value core.proto declares."""

    def __init__(self, frame_id: str, axes_value: int):
        self.frame_id = frame_id
        self.axes_value = axes_value
        super().__init__(f"FrameDefinition {frame_id!r} has an unknown/unspecified AxesKind ({axes_value!r})")


class MissingFieldError(FrameError):
    """A ``FrameDefinition`` is missing a field its ``AxesKind`` requires."""

    def __init__(self, frame_id: str, field: str, reason: str = ""):
        self.frame_id = frame_id
        self.field = field
        msg = f"FrameDefinition {frame_id!r} is missing required field {field!r}"
        if reason:
            msg += f": {reason}"
        super().__init__(msg)


class UnknownEntityError(FrameError):
    """A ``FrameDefinition`` names an entity that is not a Spacecraft in GMAT's config."""

    def __init__(self, frame_id: str, entity_id: str):
        self.frame_id = frame_id
        self.entity_id = entity_id
        super().__init__(
            f"FrameDefinition {frame_id!r} references entity {entity_id!r}, which does not "
            f"exist as a Spacecraft in GMAT's current configuration")


class FrameNotRealizableError(FrameError, NotImplementedError):
    """A core.proto AxesKind that GMAT cannot realize without input altavista does not build.

    Only ``AXES_KIND_PLATFORM_BODY`` **without** ``attitude_source`` (needs an attitude
    model and none was declared -- see :class:`AttitudeServiceUnavailableError` for the
    *with-attitude_source* case, which is realized differently). Typed and a
    ``NotImplementedError`` subclass per M1.2's "no silent fallbacks" rule: this must be
    impossible to mistake for a validation failure the caller can just fix by filling in
    a field -- it names a capability the frame service does not have at all.
    """

    def __init__(self, frame_id: str, axes_kind_name: str, reason: str):
        self.frame_id = frame_id
        self.axes_kind_name = axes_kind_name
        super().__init__(f"FrameDefinition {frame_id!r} ({axes_kind_name}) cannot be realized by GMAT: {reason}")


class AttitudeServiceUnavailableError(FrameError):
    """:meth:`FrameRegistry.convert` was asked to convert through a ``PLATFORM_BODY``
    frame that *is* registered (it carried a valid ``attitude_source`` and passed
    validation against GMAT) -- but the frame service does not itself realize an
    attitude for conversion; that is the attitude service's job (P2, question 72).

    Deliberately **not** a :class:`FrameNotRealizableError`: that error means "this
    frame cannot be validated/declared at all". This one means the opposite -- the frame
    is legitimately declared -- so a caller must not treat the two the same way (e.g.
    retrying registration would be pointless for this one).
    """

    def __init__(self, frame_id: str):
        self.frame_id = frame_id
        super().__init__(
            f"cannot convert through frame {frame_id!r} (PLATFORM_BODY): attitude service "
            "not available -- the frame is declared and validated, but realizing its "
            "orientation for conversion needs the attitude service (P2), which the frame "
            "service does not implement")


class UnknownGmatAttitudeModelError(FrameError):
    """A ``PLATFORM_BODY`` FrameDefinition's ``attitude_source.gmat_attitude_model``
    names a string GMAT does not recognize as a constructible attitude type.

    Verified against real GMAT (attempting ``Spacecraft.SetField("Attitude", name)`` on a
    scratch spacecraft -- see :meth:`FrameRegistry._validate_gmat_attitude_model`), not by
    pattern-matching a hardcoded list of known-good names in Python.
    """

    def __init__(self, frame_id: str, model_name: str, gmat_reason: str):
        self.frame_id = frame_id
        self.model_name = model_name
        super().__init__(
            f"FrameDefinition {frame_id!r} names gmat_attitude_model {model_name!r}, which "
            f"GMAT rejected as an unknown/unconstructible attitude type: {gmat_reason}")


class InconsistentParentFrameError(FrameError):
    """A ``FrameDefinition`` supplied a non-empty ``parent_frame_id`` that disagrees with
    the frame service's deterministic fill rule (question 76). Never silently overwritten
    or silently accepted -- the caller must fix the supplied value or omit it.
    """

    def __init__(self, frame_id: str, supplied: str, computed: str):
        self.frame_id = frame_id
        self.supplied = supplied
        self.computed = computed
        super().__init__(
            f"FrameDefinition {frame_id!r} supplied parent_frame_id {supplied!r}, which is "
            f"inconsistent with the deterministic fill rule ({computed!r} expected)")


class FrameCycleError(FrameError):
    """:meth:`FrameRegistry.path_to_root` found a cycle in the ``parent_frame_id`` chain
    instead of reaching the registry root (``""``). Raised instead of recursing forever.
    """

    def __init__(self, frame_id: str, cycle: Sequence[str]):
        self.frame_id = frame_id
        self.cycle = list(cycle)
        super().__init__(
            f"parent_frame_id chain from {frame_id!r} contains a cycle: {' -> '.join(self.cycle)}")


class FrameParentMissingError(FrameError):
    """:meth:`FrameRegistry.path_to_root` (or :meth:`FrameRegistry.children`) followed a
    ``parent_frame_id`` that names a frame id which is not registered.
    """

    def __init__(self, frame_id: str, missing_parent_id: str):
        self.frame_id = frame_id
        self.missing_parent_id = missing_parent_id
        super().__init__(
            f"parent_frame_id chain from {frame_id!r} references {missing_parent_id!r}, "
            "which is not a registered frame")


# --------------------------------------------------------------------------- ENU / NED
# GMAT's `Topocentric` axes are the classical geodesy SEZ convention (South, East,
# Zenith), per docs/help/html/CoordinateSystem.html: "the y-axis points due East and
# the z-axis is normal to the local horizon[Up]. The x-axis completes the right handed
# set", i.e. x = y x z = East x Up = -North. GMAT has no native ENU or NED axes kind.
#
# Both are realized here as a *fixed* rotation applied on top of the state GMAT's
# CoordinateConverter returns for its native Topocentric (SEZ) system:
#
#   ENU (East, North, Up)   from SEZ (South, East, Up): E = SEZ_y, N = -SEZ_x, U = SEZ_z
#   NED (North, East, Down) from SEZ (South, East, Up): N = -SEZ_x, E = SEZ_y, D = -SEZ_z
#
# Both matrices are proper rotations (orthogonal, det = +1) -- SEZ, ENU and NED are all
# right-handed physical bases, just relabeled/resigned -- so the identical fixed matrix
# applies unchanged to a velocity vector: there is no epoch dependence in these
# matrices, hence no extra angular-rate term the way there would be for a rotating
# frame. This is a deliberate, documented deviation from "return GMAT's own axes
# unchanged" -- flagged here, in FRAMES.md, and in the M1.2 report.
_SEZ_TO_ENU = np.array([
    [0.0, 1.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0],
])
_SEZ_TO_NED = np.array([
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, -1.0],
])

_LOCAL_CARTESIAN_SENTINEL = "gv_local_cartesian"

# A scratch GMAT Spacecraft, shared process-wide (GMAT is a singleton -- see the class
# docstring), used only to ask GMAT "can you construct this attitude type at all" via
# Spacecraft.SetField("Attitude", name). Never the entity the FrameDefinition actually
# names -- validating a name must not mutate a real, possibly shared, Spacecraft.
_ATTITUDE_PROBE_SAT_NAME = "gv_frames_attitude_probe"

_BODY_AXES_GMAT_NAME = {
    core_pb2.AXES_KIND_ICRF: "ICRF",
    core_pb2.AXES_KIND_MJ2000_EQ: "MJ2000Eq",
    core_pb2.AXES_KIND_MJ2000_EC: "MJ2000Ec",
    core_pb2.AXES_KIND_BODY_FIXED: "BodyFixed",
}

# ObjectReferenced field settings for RIC / VNB / VVLH. See the module docstring's
# realization table for the convention each one follows and why.
_OBJECT_REFERENCED_FIELDS = {
    core_pb2.AXES_KIND_RIC: {"XAxis": "R", "ZAxis": "N"},
    core_pb2.AXES_KIND_VNB: {"XAxis": "V", "YAxis": "N"},
    core_pb2.AXES_KIND_VVLH: {"YAxis": "-N", "ZAxis": "-R"},
}

_NAME_RE = re.compile(r"[^A-Za-z0-9_]")


def _safe_name(s: str) -> str:
    """A CDM id/entity name, sanitized into a legal-looking GMAT object name."""
    out = _NAME_RE.sub("_", s) or "empty"
    if not (out[0].isalpha() or out[0] == "_"):
        out = "n" + out
    return out


def _geodetic_to_gmat(geo: core_pb2.Geodetic):
    """(body, lat_deg, lon_deg, alt_km) from a CDM ``Geodetic`` (radians, metres).

    The one place `latitude_rad`/`longitude_rad`/`height_m` are converted to the
    degrees/kilometres GMAT's `GroundStation` fields want (ADR-001: km exist only
    inside the GMAT adapter).
    """
    return geo.body, geo.latitude_rad * _DEG_PER_RAD, geo.longitude_rad * _DEG_PER_RAD, geo.height_m / M_PER_KM


class FrameRegistry:
    """Validates CDM ``FrameDefinition``s by building their GMAT realization, and
    converts states between registered frames via GMAT's ``CoordinateConverter``.

    See the module docstring for the full AxesKind -> GMAT table, and for the units and
    epoch type ``convert()`` uses.

    GMAT is a process-wide singleton (see ``altavista/bodies.py``'s module docstring): every
    GMAT-touching call on any ``FrameRegistry`` instance in this process shares the same
    configuration, so the GMAT object names this class builds are deterministic from frame
    content (never randomly generated) and existence-checked with ``gmat.Exists`` before
    construction, exactly like ``bodies.py`` does.
    """

    def __init__(self):
        self.g = gmat()
        self._defs: Dict[str, core_pb2.FrameDefinition] = {}
        # frame id -> the GMAT CoordinateSystem object CoordinateConverter calls actually
        # use. For ENU/NED this is the *underlying* Topocentric (SEZ) CS -- see convert().
        # None for LOCAL_CARTESIAN (no GMAT object backs it).
        self._native_cs: Dict[str, object] = {}
        # frame id -> fixed 3x3 rotation mapping this frame's vector to its native GMAT
        # CS's vector, or None when the frame's own axes already are the native ones.
        self._to_native: Dict[str, Optional[np.ndarray]] = {}
        self._cc = None

    # ------------------------------------------------------------------ validation / build
    def register(self, frame_def: core_pb2.FrameDefinition) -> core_pb2.FrameDefinition:
        """Validate ``frame_def`` by building the GMAT object(s) it names.

        Mutates ``frame_def.gmat_name`` in place and returns it (core.proto: "set by the
        frame service after validation. Empty until validated."). Field checks (missing
        required fields) are done in Python before anything touches GMAT's configuration;
        GMAT-level checks (e.g. an unknown reference entity) are done next, also before any
        GMAT object is constructed for this definition. Raises a :class:`FrameError`
        subclass on any failure -- never substitutes a different axes kind.
        """
        axes = frame_def.axes
        if axes == core_pb2.AXES_KIND_UNSPECIFIED:
            raise UnknownAxesKindError(frame_def.id, axes)
        elif axes in _BODY_AXES_GMAT_NAME:
            gmat_name = self._register_body_axes(frame_def)
        elif axes in (core_pb2.AXES_KIND_ENU, core_pb2.AXES_KIND_NED):
            gmat_name = self._register_topocentric(frame_def)
        elif axes in _OBJECT_REFERENCED_FIELDS:
            gmat_name = self._register_object_referenced(frame_def)
        elif axes == core_pb2.AXES_KIND_PLATFORM_BODY:
            gmat_name = self._register_platform_body(frame_def)
        elif axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN:
            gmat_name = self._register_local_cartesian(frame_def)
        else:  # pragma: no cover - core.proto grew an AxesKind this module has no case for
            raise UnknownAxesKindError(frame_def.id, axes)

        frame_def.gmat_name = gmat_name
        self._fill_parent_frame_id(frame_def)
        self._defs[frame_def.id] = frame_def
        return frame_def

    def get(self, frame_id: str) -> core_pb2.FrameDefinition:
        """The registered ``FrameDefinition`` for ``frame_id`` (raises if unregistered)."""
        return self._require(frame_id)

    def _require(self, frame_id: str) -> core_pb2.FrameDefinition:
        try:
            return self._defs[frame_id]
        except KeyError:
            raise FrameError(f"frame id {frame_id!r} is not registered; call register() first") from None

    # ---------------------------------------------------------- AXES_KIND_ICRF / MJ2000_* / BODY_FIXED
    def _register_body_axes(self, frame_def: core_pb2.FrameDefinition) -> str:
        which = frame_def.WhichOneof("origin")
        if which != "body" or not frame_def.body:
            raise MissingFieldError(
                frame_def.id, "body",
                f"{core_pb2.AxesKind.Name(frame_def.axes)} requires a body origin "
                f"(oneof 'origin' = 'body'); got {which!r}")
        origin = frame_def.body
        axes = _BODY_AXES_GMAT_NAME[frame_def.axes]
        name = f"{origin}{axes}"  # matches bodies.py's own naming (EarthMJ2000Eq, ...)
        cs = _body_axes_cs(_BodyFrame(name, origin, axes))
        self._native_cs[frame_def.id] = cs
        self._to_native[frame_def.id] = None
        return name

    # ---------------------------------------------------------------------- AXES_KIND_ENU / NED
    def _register_topocentric(self, frame_def: core_pb2.FrameDefinition) -> str:
        if not frame_def.HasField("origin_geodetic"):
            raise MissingFieldError(
                frame_def.id, "origin_geodetic",
                f"{core_pb2.AxesKind.Name(frame_def.axes)} requires origin_geodetic")
        geo = frame_def.origin_geodetic
        if not geo.body:
            raise MissingFieldError(frame_def.id, "origin_geodetic.body", "Geodetic.body is empty")
        body, lat_deg, lon_deg, alt_km = _geodetic_to_gmat(geo)

        g = self.g
        safe = _safe_name(frame_def.id)
        gs_name = f"gv_gs_{safe}"
        cs_name = f"gv_topo_{safe}"

        if g.Exists(gs_name):
            gs = g.GetObject(gs_name)
        else:
            gs = g.Construct("GroundStation", gs_name)
            gs.SetField("CentralBody", body)
            gs.SetField("StateType", "Spherical")
            gs.SetField("HorizonReference", "Ellipsoid")
            gs.SetField("Location1", lat_deg)
            gs.SetField("Location2", lon_deg)
            gs.SetField("Location3", alt_km)

        if g.Exists(cs_name):
            cs = g.GetObject(cs_name)
        else:
            cs = g.Construct("CoordinateSystem", cs_name, gs_name, "Topocentric")
            g.Initialize()

        self._native_cs[frame_def.id] = cs
        self._to_native[frame_def.id] = (
            _SEZ_TO_ENU.T if frame_def.axes == core_pb2.AXES_KIND_ENU else _SEZ_TO_NED.T
        )
        return cs_name

    # ------------------------------------------------------------- AXES_KIND_RIC / VNB / VVLH
    def _register_object_referenced(self, frame_def: core_pb2.FrameDefinition) -> str:
        axes_name = core_pb2.AxesKind.Name(frame_def.axes)
        if not frame_def.reference_entity_id:
            raise MissingFieldError(frame_def.id, "reference_entity_id", f"{axes_name} requires reference_entity_id")
        if not frame_def.reference_body:
            raise MissingFieldError(frame_def.id, "reference_body", f"{axes_name} requires reference_body")
        which_origin = frame_def.WhichOneof("origin")
        if which_origin != "entity_id" or not frame_def.entity_id:
            raise MissingFieldError(
                frame_def.id, "entity_id",
                f"{axes_name} is an entity-relative frame: the 'origin' oneof must be "
                f"'entity_id' (the state that defines the frame's origin point); got {which_origin!r}")

        g = self.g
        origin_entity = frame_def.entity_id
        ref_entity = frame_def.reference_entity_id
        for entity in dict.fromkeys((origin_entity, ref_entity)):  # dedupe, keep order
            self._require_spacecraft(frame_def.id, entity)

        axes_word = axes_name.rsplit("_", 1)[-1].lower()  # ric / vnb / vvlh
        name = f"gv_{axes_word}_{_safe_name(origin_entity)}_{_safe_name(ref_entity)}_{_safe_name(frame_def.reference_body)}"
        if g.Exists(name):
            cs = g.CoordinateSystem.SetClass(g.GetObject(name))
        else:
            # `Primary`/`Secondary`/`XAxis`/`YAxis`/`ZAxis` are NOT plain GmatBase
            # parameters on a CoordinateSystem built through the raw Construct/SetField
            # API (only the script Interpreter forwards those field names to the
            # underlying AxisSystem sub-object). The typed accessors below
            # (`CoordinateSystem.SetPrimaryObject` etc, found via the actual object,
            # requiring the `g.CoordinateSystem.SetClass` downcast, matching the
            # `gmat.GroundStation.SetClass(...)` pattern in GMAT's own
            # `api/Ex_R2020a_RangeMeasurement.py`) are the API that works; this was
            # confirmed empirically against this GMAT build before writing this code.
            raw = g.Construct("CoordinateSystem", name, origin_entity, "ObjectReferenced")
            cs = g.CoordinateSystem.SetClass(raw)
            cs.SetPrimaryObject(self._space_point(frame_def.reference_body))
            cs.SetSecondaryObject(g.GetObject(ref_entity))
            xyz = _OBJECT_REFERENCED_FIELDS[frame_def.axes]
            if "XAxis" in xyz:
                cs.SetXAxis(xyz["XAxis"])
            if "YAxis" in xyz:
                cs.SetYAxis(xyz["YAxis"])
            if "ZAxis" in xyz:
                cs.SetZAxis(xyz["ZAxis"])
            g.Initialize()

        self._native_cs[frame_def.id] = cs
        self._to_native[frame_def.id] = None
        return name

    def _space_point(self, body: str):
        """A celestial body as a GMAT SpacePoint, for CoordinateSystem.SetPrimaryObject."""
        try:
            return self.g.GetSolarSystem().GetBody(body)
        except Exception as exc:  # GMAT's own exception type varies by build
            raise FrameError(f"reference_body {body!r} is not a known celestial body: {exc}") from exc

    def _require_spacecraft(self, frame_id: str, entity_id: str) -> None:
        """Raise :class:`UnknownEntityError` unless ``entity_id`` is a Spacecraft in
        GMAT's current configuration. Shared by RIC/VNB/VVLH and PLATFORM_BODY."""
        g = self.g
        if not g.Exists(entity_id) or g.GetObject(entity_id).GetTypeName() != "Spacecraft":
            raise UnknownEntityError(frame_id, entity_id)

    # --------------------------------------------------------------- AXES_KIND_LOCAL_CARTESIAN
    def _register_local_cartesian(self, frame_def: core_pb2.FrameDefinition) -> str:
        # Frameless by design -- see the module docstring's realization table entry.
        self._native_cs[frame_def.id] = None
        self._to_native[frame_def.id] = None
        return _LOCAL_CARTESIAN_SENTINEL

    # ------------------------------------------------------------------ AXES_KIND_PLATFORM_BODY
    def _register_platform_body(self, frame_def: core_pb2.FrameDefinition) -> str:
        """Validate and declare a ``PLATFORM_BODY`` frame (question 72).

        Unconditionally raises :class:`FrameNotRealizableError` when ``attitude_source``
        is absent -- unchanged from before this frame carried one. When it is present,
        validates the referenced entity exists and (for ``gmat_attitude_model``) that
        GMAT can construct that attitude type, then registers the frame as
        declared-but-not-convertible: no native GMAT CoordinateSystem backs it, so
        :meth:`convert` refuses with :class:`AttitudeServiceUnavailableError`, not this
        error -- see the module docstring's realization table.
        """
        if not frame_def.HasField("attitude_source"):
            raise FrameNotRealizableError(
                frame_def.id, "PLATFORM_BODY",
                "needs a configured attitude model on the named platform/entity; "
                "FrameDefinition.attitude_source is unset, so there is nothing to "
                "validate against. Set attitude_source (question 72) to declare this "
                "frame, or realize it through an attitude service (P2).")

        src = frame_def.attitude_source
        which_source = src.WhichOneof("source")
        if which_source is None:
            raise MissingFieldError(
                frame_def.id, "attitude_source.source",
                "PLATFORM_BODY's attitude_source must set exactly one of "
                "entity_attitude_stream or gmat_attitude_model")
        if not frame_def.reference_entity_id:
            raise MissingFieldError(
                frame_def.id, "reference_entity_id",
                "PLATFORM_BODY requires reference_entity_id: the entity whose attitude "
                "defines the axes")
        if not src.reference_frame_id:
            raise MissingFieldError(
                frame_def.id, "attitude_source.reference_frame_id",
                "PLATFORM_BODY requires attitude_source.reference_frame_id: the frame the "
                "attitude is expressed relative to, which also doubles as this frame's "
                "parent_frame_id (question 76's fill rule) since altavista does not invent a "
                "different one")

        # GMAT-level checks next, after every Python-level field check above has passed
        # (module docstring: field checks first, GMAT checks only once those hold).
        self._require_spacecraft(frame_def.id, frame_def.reference_entity_id)
        if which_source == "gmat_attitude_model":
            self._validate_gmat_attitude_model(frame_def.id, src.gmat_attitude_model)
        # else entity_attitude_stream: a CDM/state-space concept, not a GMAT one -- this
        # module validates against GMAT's configuration only (see FRAMES.md), so there is
        # nothing further to check here without pattern-matching against a Python-side
        # notion of the entity catalog this module does not own.

        self._native_cs[frame_def.id] = None
        self._to_native[frame_def.id] = None
        return f"gv_platform_body_{_safe_name(frame_def.id)}"

    def _validate_gmat_attitude_model(self, frame_id: str, model_name: str) -> None:
        """Verify ``model_name`` against real GMAT: attempt to set it as the ``Attitude``
        field of a scratch GMAT Spacecraft (never the entity the definition actually
        names -- see ``_ATTITUDE_PROBE_SAT_NAME``) and let GMAT's own exception decide.

        This proves GMAT recognizes ``model_name`` as a constructible attitude *type* --
        confirmed empirically (see the M3.3 report) to raise a GMAT ``APIException`` at
        exactly this call for an unrecognized name, e.g. "Cannot create Attitude object of
        unknown attitude type ...", and to succeed and construct the Attitude sub-object
        immediately for a recognized one (GMAT does not defer that construction to
        ``Initialize()``). It does **not** prove: that ``model_name``'s additional fields
        (e.g. an AEM file for ``CCSDS-AEM``) are configured or exist; that this attitude
        type is compatible with how ``reference_entity_id`` is actually propagated; or
        anything about ``reference_frame_id``. Those remain the attitude service's job.
        """
        if not model_name:
            raise MissingFieldError(
                frame_id, "attitude_source.gmat_attitude_model",
                "the gmat_attitude_model source requires a non-empty model name")
        g = self.g
        if g.Exists(_ATTITUDE_PROBE_SAT_NAME):
            probe = g.GetObject(_ATTITUDE_PROBE_SAT_NAME)
        else:
            probe = g.Construct("Spacecraft", _ATTITUDE_PROBE_SAT_NAME)
            g.Initialize()
        try:
            probe.SetField("Attitude", model_name)
        except Exception as exc:  # GMAT's own exception type varies by build
            raise UnknownGmatAttitudeModelError(frame_id, model_name, str(exc)) from exc

    # --------------------------------------------------------------- parent_frame_id (question 76)
    def _fill_parent_frame_id(self, frame_def: core_pb2.FrameDefinition) -> None:
        """Fill ``frame_def.parent_frame_id`` deterministically when empty, or check a
        supplied one against the same rule and reject it if it disagrees.

        proto3 gives ``parent_frame_id`` no explicit field presence (it is a plain
        ``string``, not ``optional``), so an empty string and "the field was never set"
        are the same wire value -- there is no way to tell them apart, and no need to:
        every axes kind whose rule computes ``""`` (root) treats "supplied empty" and
        "omitted" identically anyway, and for every other kind an explicitly-supplied
        empty string is simply wrong and gets rejected like any other inconsistent value.
        """
        computed = self._computed_parent(frame_def)
        supplied = frame_def.parent_frame_id
        if supplied and supplied != computed:
            raise InconsistentParentFrameError(frame_def.id, supplied, computed)
        frame_def.parent_frame_id = computed

    def _computed_parent(self, frame_def: core_pb2.FrameDefinition) -> str:
        """The deterministic parent_frame_id for ``frame_def`` per question 76's rule --
        see the module docstring's "``parent_frame_id`` fill" section for the full table.
        """
        axes = frame_def.axes
        if axes in _BODY_AXES_GMAT_NAME or axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN:
            return ""
        if axes in (core_pb2.AXES_KIND_ENU, core_pb2.AXES_KIND_NED):
            return self._ensure_body_axes_parent(core_pb2.AXES_KIND_BODY_FIXED, frame_def.origin_geodetic.body)
        if axes in _OBJECT_REFERENCED_FIELDS:  # RIC / VNB / VVLH
            return self._ensure_body_axes_parent(core_pb2.AXES_KIND_MJ2000_EQ, frame_def.reference_body)
        if axes == core_pb2.AXES_KIND_PLATFORM_BODY:
            return frame_def.attitude_source.reference_frame_id
        raise UnknownAxesKindError(frame_def.id, axes)  # pragma: no cover - unreachable, register() already checked

    def _find_registered_body_axes(self, axes_kind: int, body: str) -> Optional[str]:
        """The id of a registered frame with ``axes_kind`` (MJ2000_EQ or BODY_FIXED) whose
        origin is ``body``, or ``None``. Picks the lexicographically smallest id when more
        than one matches, so this never depends on dict insertion order.
        """
        matches = sorted(
            fid for fid, d in self._defs.items()
            if d.axes == axes_kind and d.WhichOneof("origin") == "body" and d.body == body)
        return matches[0] if matches else None

    def _ensure_body_axes_parent(self, axes_kind: int, body: str) -> str:
        """The id of the registered ``axes_kind`` frame about ``body`` used as an
        auto-filled parent (ENU/NED -> BODY_FIXED, RIC/VNB/VVLH -> MJ2000_EQ), registering
        one under a canonical id if none is registered yet.
        """
        existing = self._find_registered_body_axes(axes_kind, body)
        if existing is not None:
            return existing
        canonical_id = f"gv_auto_parent_{_safe_name(body)}_{_BODY_AXES_GMAT_NAME[axes_kind]}"
        prior = self._defs.get(canonical_id)
        if prior is not None:
            # _find_registered_body_axes above would have found this already if it
            # matched axes_kind/body -- reaching here means a different frame occupies
            # the canonical id, an id collision this module refuses to paper over.
            raise FrameError(
                f"cannot auto-register parent frame {canonical_id!r} for body {body!r} "
                f"({core_pb2.AxesKind.Name(axes_kind)}): a different frame is already "
                "registered under that id")
        self.register(core_pb2.FrameDefinition(id=canonical_id, axes=axes_kind, body=body))
        return canonical_id

    # ------------------------------------------------------------------------------ tree (question 76)
    def children(self, frame_id: str = "") -> List[str]:
        """Registered frame ids whose ``parent_frame_id`` is ``frame_id``, sorted
        lexicographically for a deterministic order. ``frame_id=""`` (the default) lists
        the registry's top-level (root) frames. Raises :class:`FrameError` if a non-root
        ``frame_id`` is not itself registered.
        """
        if frame_id and frame_id not in self._defs:
            raise FrameError(f"frame id {frame_id!r} is not registered; call register() first")
        return sorted(fid for fid, d in self._defs.items() if d.parent_frame_id == frame_id)

    def path_to_root(self, frame_id: str) -> List[str]:
        """``[frame_id, parent, grandparent, ...]`` up to (not including) the root
        sentinel ``""``. Raises :class:`FrameCycleError` on a cycle in the
        ``parent_frame_id`` chain, or :class:`FrameParentMissingError` if a parent names
        an unregistered frame -- never recurses forever on a malformed chain.
        """
        self._require(frame_id)
        path: List[str] = []
        seen = set()
        current = frame_id
        while current != "":
            if current in seen:
                raise FrameCycleError(frame_id, path + [current])
            seen.add(current)
            path.append(current)
            d = self._defs.get(current)
            if d is None:
                raise FrameParentMissingError(frame_id, current)
            current = d.parent_frame_id
        return path

    # ------------------------------------------------------------------------------ conversion
    def _converter(self):
        if self._cc is None:
            self._cc = self.g.CoordinateConverter()
        return self._cc

    def convert(self, state: Sequence[float], epoch_a1mjd: float, from_id: str, to_id: str) -> List[float]:
        """Convert ``[x, y, z, vx, vy, vz]`` (metres, m/s) from frame ``from_id`` to
        ``to_id`` at ``epoch_a1mjd`` (A1MJD float -- see the module docstring for why).

        Both ids must already be registered via :meth:`register`.
        """
        src_def, dst_def = self._require(from_id), self._require(to_id)
        if src_def.axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN or dst_def.axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN:
            bad = from_id if src_def.axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN else to_id
            raise FrameError(
                f"cannot convert {'from' if bad == from_id else 'to'} frame {bad!r}: "
                "LOCAL_CARTESIAN is frameless by design (no GMAT axes back it), so there is "
                "no defined mapping into or out of it")
        if src_def.axes == core_pb2.AXES_KIND_PLATFORM_BODY:
            raise AttitudeServiceUnavailableError(from_id)
        if dst_def.axes == core_pb2.AXES_KIND_PLATFORM_BODY:
            raise AttitudeServiceUnavailableError(to_id)

        pos = np.array(state[0:3], dtype=float) / M_PER_KM
        vel = np.array(state[3:6], dtype=float) / M_PER_KM

        m_src = self._to_native.get(from_id)
        if m_src is not None:
            pos, vel = m_src @ pos, m_src @ vel

        g = self.g
        s_in = g.Rvector6(float(pos[0]), float(pos[1]), float(pos[2]), float(vel[0]), float(vel[1]), float(vel[2]))
        s_out = g.Rvector6()
        self._converter().Convert(
            g.A1Mjd(float(epoch_a1mjd)), s_in, self._native_cs[from_id], s_out, self._native_cs[to_id])
        pos = np.array([s_out[0], s_out[1], s_out[2]])
        vel = np.array([s_out[3], s_out[4], s_out[5]])

        m_dst = self._to_native.get(to_id)
        if m_dst is not None:
            # native -> this frame is the inverse of to-native, i.e. its transpose (m_dst
            # is an orthogonal fixed rotation -- see _SEZ_TO_ENU / _SEZ_TO_NED above).
            pos, vel = m_dst.T @ pos, m_dst.T @ vel

        return [float(x) * M_PER_KM for x in pos] + [float(x) * M_PER_KM for x in vel]

    def rotation_matrix(self, from_id: str, to_id: str, epoch_a1mjd: float) -> np.ndarray:
        """3x3 rotation GMAT's CoordinateConverter used between the two frames' *native*
        GMAT CoordinateSystems at ``epoch_a1mjd`` -- i.e. GMAT's own rotation, bypassing
        the ENU/NED fixed matrix. Meant for verifying GMAT's output directly (obliquity,
        RIC orthonormality/radial-alignment); use :meth:`convert` for CDM-facing state
        conversion.
        """
        src_def, dst_def = self._require(from_id), self._require(to_id)
        if src_def.axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN or dst_def.axes == core_pb2.AXES_KIND_LOCAL_CARTESIAN:
            raise FrameError("LOCAL_CARTESIAN has no GMAT rotation to report")
        if src_def.axes == core_pb2.AXES_KIND_PLATFORM_BODY:
            raise AttitudeServiceUnavailableError(from_id)
        if dst_def.axes == core_pb2.AXES_KIND_PLATFORM_BODY:
            raise AttitudeServiceUnavailableError(to_id)
        g = self.g
        dummy_in = g.Rvector6(1.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        dummy_out = g.Rvector6()
        cc = self._converter()
        cc.Convert(g.A1Mjd(float(epoch_a1mjd)), dummy_in, self._native_cs[from_id], dummy_out, self._native_cs[to_id])
        R = cc.GetLastRotationMatrix()
        return np.array([[R.GetElement(i, j) for j in range(3)] for i in range(3)])
