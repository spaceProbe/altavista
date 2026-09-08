"""Plain data containers describing a scenario, and their JSON form.

Everything the browser needs is in :meth:`Scenario.to_dict`. Positions are in km,
velocities in km/s, epochs are A1MJD floats, all expressed in ``Scenario.frame``.

``frames`` (M4.1, docs/open-questions.md question 78)
--------------------------------------------------------
``ScenarioData.to_dict()`` carries an **additive** ``frames`` key: a list of plain
dicts, each the ``google.protobuf.json_format.MessageToDict`` transcoding of one
``altavista.v1.FrameDefinition`` (camelCase field names, e.g. ``parentFrameId``,
``gmatName``; enum fields as their string name, e.g. ``"AXES_KIND_RIC"`` -- the
protobuf3 canonical JSON mapping, default ``MessageToDict`` settings, no
``preserving_proto_field_name``). Every entry has been through
:class:`altavista.frames.FrameRegistry` (``altavista/scenario.py`` builds the registry and
does the transcoding; this module only carries the resulting dicts), so
``parentFrameId`` is always filled per question 76's deterministic rule -- a root
frame simply omits the key (proto3 default-value elision), which the viewer treats as
"no parent" identically to an explicit empty string.

One extra, non-``FrameDefinition`` key is added per entry when applicable:
``originTrack`` -- ``{"t": [...], "pos": [...], "vel": [...]}`` (A1MJD / km / km-s,
flat ``pos``/``vel`` like :meth:`Trajectory.to_dict`) for a frame whose origin moves
relative to its parent (an entity-relative RIC/VNB/VVLH frame); absent for a
non-moving (body-centred, body-fixed) frame. This is an altavista wire-format extension
the CDM's ``FrameDefinition`` message itself does not carry (it has no origin-motion
field) -- documented here rather than silently smuggled in, since the frame graph
(``web/js/frames.js``'s ``FrameNode.setOriginTrack``) needs it to animate a moving
frame's own transform.

This key is purely additive: every field ``ScenarioData.to_dict()`` produced before
M4.1 keeps its exact meaning and shape, and ``frames`` defaults to an empty list, so
a caller (or test) that never populates it gets ``"frames": []`` and nothing else
changes -- see ``tests/test_script_prep.py::test_scenario_data_json_shape``.

``scores`` (M26.4b, docs/open-questions.md question 165)
----------------------------------------------------------
``ScenarioData.to_dict()`` carries an **additive** ``scores`` key: a plain dict keyed by
score name, each entry ``{"value": <float>, "unit": <str>, "passed": <bool|None>}`` --
the wire shape the lead approved for threading ``altavista.v1.RunProducts.scores``
(``ScoreResult``, ADR-005 sec 6) into the viewer payload. ``altavista/server.py``'s
``POST /api/cdm/run`` handler builds these dicts (never ``google.protobuf.json_format.
MessageToDict`` directly on a ``ScoreResult`` -- that helper *omits* an unset
``optional bool passed`` entirely rather than emitting an explicit ``null``, which
would silently misrepresent a measure of effectiveness as "field not sent" instead of
"no pass criterion exists"). ``passed`` is ``None`` (JSON ``null``) for a measure of
effectiveness, never coerced to ``False``. Purely additive: defaults to ``{}``, so
every other publish path (``POST /api/scenario``, ``POST /api/cdm/trajectory``) and
every scenario built before M26.4b still round-trips with ``"scores": {}`` and nothing
else changes.

``measurements`` (M25.3e, docs/open-questions.md question 174)
------------------------------------------------------------------
``ScenarioData.to_dict()`` carries an **additive** ``measurements`` key: a list of
plain dicts, each ``{"id": <str>, "epoch": <float A1MJD>, "sensorId": <str>,
"frameId": <str>, "z": [<float>...], "r": [<float>...]}`` -- the wire shape the lead
approved for threading ``altavista.v1.RunProducts.measurements`` (question 173's
``Measurement`` list, sorted by epoch and id by the executor) into the viewer payload.
``altavista/server.py``'s ``POST /api/cdm/run`` handler builds these dicts
(``_measurement_to_dict``, mirroring ``_score_result_to_dict``'s own hand-built-dict
convention). ``epoch`` is ``Measurement.epoch_ns`` converted through the same
``cdm_adapter.tai_ns_to_a1mjd`` every other epoch on the payload uses, so a
measurement lines up on the same timeline as everything else. ``z``/``r`` are plain
lists copied verbatim off the proto's own repeated ``double`` fields -- an empty ``r``
(a sensor that declares no covariance for a given measurement, e.g. a star tracker's
raw unit-quaternion component) is published as an empty list, never a fabricated
identity or zero matrix (nothing is ever synthesized). Purely additive: defaults to
``[]``, so every other publish path and every scenario built before M25.3e still
round-trips with ``"measurements": []`` and nothing else changes.

``referenceId``/``attributes`` on an event (M25.3e, docs/open-questions.md question 177)
--------------------------------------------------------------------------------------------
:class:`Event` carries two more **additive** fields on top of the pre-existing
``{name, t, type, spacecraft, detail}`` wire shape: ``referenceId`` (the real CDM
``Event.reference_id`` -- for a command transition, the ``Command.id``) and
``attributes`` (the real CDM ``Event.provenance.attributes`` as a plain string map --
where ``ack_level`` lives on a command's ACKED transition). Both default to falsy/empty
(``None`` / ``{}``) so a caller that builds an :class:`Event` without them, and every
scenario published before M25.3e, still round-trips unchanged. See
``altavista/cdm.py``'s ``cdm_event_to_viewer_event`` for where these are populated from
a real ``altavista.v1.Event``, and ``web/js/timeline_events.js`` for the viewer-side
consumer (command grouping keyed on ``referenceId`` instead of parsing ``detail``,
the acknowledgement panel reading the real ``ack_level`` out of ``attributes``).
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Dict, List, Optional

SCHEMA_VERSION = 1

# M7.1 (docs/open-questions.md question 88 / docs/adr/005-simulation-kernel.md sec 3): the
# two state-space ids this codebase declares components for. Defined here (not in
# altavista/cdm.py, which owns the protobuf-carrying `StateSpace` builder itself) so this
# protobuf-free module and cdm.py agree on the exact same id strings without either one
# hardcoding a second copy -- cdm.py imports these two names rather than re-declaring them
# (see that module's own `DEFAULT_STATE_SPACE_ID`/`DEFAULT_STATE_SPACE_ID_WITH_ATTITUDE`,
# which alias these verbatim for backward compatibility with existing callers).
STATE_SPACE_ID_CARTESIAN_POS_VEL_6 = "altavista.cartesian_pos_vel_6"
STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4 = "altavista.cartesian_pos_vel_6_attitude_quat_4"

DEFAULT_COLORS = [
    "#ff9f43", "#54a0ff", "#1dd1a1", "#ff6b6b", "#feca57",
    "#5f27cd", "#48dbfb", "#ff9ff3", "#c8d6e5", "#00d2d3",
]


@dataclass
class Frame:
    """A GMAT coordinate system: origin body + axes type, e.g. Earth / MJ2000Eq."""
    name: str = "EarthMJ2000Eq"
    origin: str = "Earth"
    axes: str = "MJ2000Eq"

    def to_dict(self) -> dict:
        return {"name": self.name, "origin": self.origin, "axes": self.axes}


@dataclass
class Trajectory:
    """Sampled state history of one spacecraft."""
    name: str
    t: List[float] = field(default_factory=list)          # A1MJD
    pos: List[List[float]] = field(default_factory=list)  # km, [[x,y,z], ...]
    vel: List[List[float]] = field(default_factory=list)  # km/s
    color: Optional[str] = None
    label: Optional[str] = None
    model: Optional[str] = None                            # reserved for 3D model refs
    # Additive, M5.2 (docs/open-questions.md question 10's sensor-footprint
    # groundwork): a real attitude quaternion stream, [[x, y, z, w], ...], one per
    # sample, parallel to `t` -- scalar-last, matching
    # proto/altavista/v1/core.proto's AttitudeSource doc comment convention. Expressed
    # relative to this trajectory's own frame (ScenarioData.frame), the same frame
    # `pos`/`vel` are in. Empty (the default) means "no attitude data recorded for
    # this spacecraft" -- web/js/scene.js's per-entity body-frame node then falls back
    # to a nadir-pointing VVLH orientation derived from `pos`/`vel` instead, and labels
    # that fallback in the UI (never silently substituting one for the other). No
    # producer populates this yet (GMAT attitude realization is explicitly P2 per that
    # proto field's own comment); this is schema groundwork only.
    attitude: List[List[float]] = field(default_factory=list)

    def append(self, t: float, state) -> None:
        s = [float(state[i]) for i in range(6)]
        self.t.append(float(t))
        self.pos.append(s[0:3])
        self.vel.append(s[3:6])

    @property
    def t0(self) -> Optional[float]:
        return self.t[0] if self.t else None

    @property
    def t1(self) -> Optional[float]:
        return self.t[-1] if self.t else None

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "label": self.label or self.name,
            "color": self.color,
            "t": self.t,
            "pos": [c for p in self.pos for c in p],
            "vel": [c for v in self.vel for c in v],
            "attitude": [c for q in self.attitude for c in q],
            # Additive, M7.1 (question 88): which declared StateSpace (see the module-level
            # STATE_SPACE_ID_* constants and ScenarioData.state_spaces below) this
            # trajectory's `pos`/`vel`/`attitude` shape corresponds to -- derived from
            # whether `attitude` is populated, the same rule altavista/cdm.py's
            # trajectory_to_cdm() upgrade uses, so the two never disagree about which shape
            # a given Trajectory is.
            "stateSpaceId": (STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4 if self.attitude
                             else STATE_SPACE_ID_CARTESIAN_POS_VEL_6),
        }


@dataclass
class BodyTrack:
    """A celestial body: size, texture and sampled position/orientation in the frame."""
    name: str
    radius: float                 # equatorial radius, km
    flattening: float = 0.0
    texture: Optional[str] = None  # URL path served by the viewer server
    color: str = "#888888"
    central: bool = False
    t: List[float] = field(default_factory=list)
    pos: List[List[float]] = field(default_factory=list)
    # Rotation from body-fixed to the scenario frame at each sample, as a unit quaternion [x,y,z,w]
    quat: List[List[float]] = field(default_factory=list)
    spin_rate: float = 0.0        # deg/day about the body's pole (used between samples)

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "radius": self.radius,
            "flattening": self.flattening,
            "texture": self.texture,
            "color": self.color,
            "central": self.central,
            "t": self.t,
            "pos": [c for p in self.pos for c in p],
            "quat": [c for q in self.quat for c in q],
            "spinRate": self.spin_rate,
        }


@dataclass
class Event:
    """A point-in-time annotation (maneuver, custom marker...)."""
    name: str
    t: float                      # A1MJD
    type: str = "marker"
    spacecraft: Optional[str] = None
    detail: Optional[str] = None
    # Additive, M25.3e (docs/open-questions.md question 177): the real CDM Event's own
    # `reference_id` (for a command transition, the Command.id) and
    # `provenance.attributes` (a plain string map -- `ack_level` lives here on a
    # command's ACKED transition). See this module's own docstring, "referenceId /
    # attributes on an event" section, for the full contract. Both default to
    # falsy/empty so an Event built without them (every caller before M25.3e) is
    # unaffected.
    reference_id: Optional[str] = None
    attributes: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> dict:
        return {"name": self.name, "t": self.t, "type": self.type,
                "spacecraft": self.spacecraft, "detail": self.detail,
                "referenceId": self.reference_id, "attributes": dict(self.attributes)}


@dataclass
class Footprint:
    """A sensor cone (half-angle about a declared body axis) intersected with its
    target body's ellipsoid, sampled alongside a spacecraft's attitude (M6.3, the
    first consumer of ``Trajectory.attitude``). Additive: a scenario with no
    footprints declared gets ``"footprints": []`` and nothing else changes.

    Every position here (``center``, ``ring``) is in *this scenario's own frame*
    (``ScenarioData.frame``, km, matching ``Trajectory.pos``) at the corresponding
    ``t`` -- already converted from the target body's rotating body-fixed frame, the
    same way ``Trajectory.pos``/``vel`` are already in the scenario frame rather than
    the raw propagation frame. The viewer draws these directly as scene geometry, no
    further frame conversion needed.

    Sampled at the *nearest recorded attitude sample* only (one ring per ``t`` entry,
    not a continuously-interpolated cone) -- unlike position (Hermite) or attitude
    (slerp), a ring of ellipsoid-intersection points has no natural interpolation
    contract to fit a curve to between samples, so this does not invent one; the
    viewer picks the nearest sample in time rather than blending two rings.
    """
    name: str
    spacecraft: str
    half_angle_deg: float
    axis: List[float]                                      # unit body-frame axis, [x, y, z]
    color: str = "#00e5ff"
    t: List[float] = field(default_factory=list)            # A1MJD, subset of the spacecraft's own t
    # Sub-boresight ("center") ellipsoid intersection point per sample, [x, y, z] km,
    # or None if the boresight itself misses the ellipsoid at that sample.
    center: List[Optional[List[float]]] = field(default_factory=list)
    # Cone-edge ring per sample: flat [x0,y0,z0, x1,y1,z1, ...] km, points in ray order;
    # a ray that misses the ellipsoid (cone partly over the horizon) is simply omitted,
    # so a ring may have fewer than the requested point count -- never a fabricated point.
    ring: List[List[float]] = field(default_factory=list)

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "spacecraft": self.spacecraft,
            "halfAngleDeg": self.half_angle_deg,
            "axis": list(self.axis),
            "color": self.color,
            "t": list(self.t),
            "center": [list(c) if c is not None else None for c in self.center],
            "ring": [list(r) for r in self.ring],
        }


@dataclass
class ScenarioData:
    name: str
    frame: Frame = field(default_factory=Frame)
    spacecraft: List[Trajectory] = field(default_factory=list)
    bodies: List[BodyTrack] = field(default_factory=list)
    events: List[Event] = field(default_factory=list)
    meta: Dict[str, object] = field(default_factory=dict)
    t0_iso: Optional[str] = None
    t1_iso: Optional[str] = None
    # Additive, M4.1: list of protobuf-JSON-transcoded FrameDefinition dicts (see the
    # module docstring's "frames" section). Plain dicts, not proto messages -- this
    # module has no protobuf dependency of its own; altavista/scenario.py builds them.
    frames: List[dict] = field(default_factory=list)
    # Additive, M6.3: declared sensor footprints (see Footprint's own docstring).
    footprints: List[Footprint] = field(default_factory=list)
    # Additive, M7.1 (docs/open-questions.md question 88 / docs/adr/005-simulation-kernel.md
    # sec 3): list of protobuf-JSON-transcoded altavista.v1.StateSpace dicts (same
    # MessageToDict convention as `frames` above), one per state-space id actually used by
    # this scenario's recorded spacecraft trajectories (altavista/scenario.py's
    # `_build_state_spaces` populates it via `altavista.cdm.state_space_for`). This is the
    # declared-message half of question 88's condition (a): a viewer or kernel consumer
    # reading `spacecraft[i].stateSpaceId` finds the labels/units it names here, never an
    # ad hoc id string with nothing behind it. Additive: defaults to `[]`, so a scenario
    # built before M7.1 round-trips unchanged (tests/test_script_prep.py::test_scenario_data_json_shape).
    state_spaces: List[dict] = field(default_factory=list)
    # Additive, M26.4b (docs/open-questions.md question 165): {name: {value, unit, passed}}
    # -- see the module docstring's "scores" section above for the exact wire shape and why
    # `passed` must be an explicit `None`, not an absent key, for a measure of effectiveness.
    scores: Dict[str, dict] = field(default_factory=dict)
    # Additive, M25.3e (docs/open-questions.md question 174): a list of
    # {id, epoch, sensorId, frameId, z, r} dicts -- see the module docstring's
    # "measurements" section above for the exact wire shape and why `r` is published
    # empty (never fabricated) when the producer declared no covariance.
    measurements: List[dict] = field(default_factory=list)

    def span(self):
        ts = [tr.t0 for tr in self.spacecraft if tr.t] + [tr.t1 for tr in self.spacecraft if tr.t]
        if not ts:
            return None
        return min(ts), max(ts)

    def to_dict(self) -> dict:
        span = self.span()
        return {
            "schema": SCHEMA_VERSION,
            "name": self.name,
            "frame": self.frame.to_dict(),
            "t0": span[0] if span else None,
            "t1": span[1] if span else None,
            "t0Iso": self.t0_iso,
            "t1Iso": self.t1_iso,
            "spacecraft": [s.to_dict() for s in self.spacecraft],
            "bodies": [b.to_dict() for b in self.bodies],
            "events": [e.to_dict() for e in self.events],
            "frames": list(self.frames),
            "footprints": [f.to_dict() for f in self.footprints],
            "stateSpaces": list(self.state_spaces),
            "scores": dict(self.scores),
            "measurements": list(self.measurements),
            "meta": {"published": datetime.now(timezone.utc).isoformat(timespec="seconds"), **self.meta},
        }

    def to_json(self, **kw) -> str:
        return json.dumps(self.to_dict(), **kw)
