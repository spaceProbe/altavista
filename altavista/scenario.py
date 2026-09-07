"""Scenario builder: the Python-facing API.

Two ways to produce trajectories:

* **Step propagation** (:meth:`Scenario.propagate`): spacecraft, force models and
  propagators are built through the GMAT object API and advanced step by step,
  sampling the state at every step. Impulsive maneuvers can be applied between
  propagate calls with :meth:`Scenario.maneuver`. Good for interactive work.

* **Script runs** (:meth:`Scenario.from_script` / :meth:`Scenario.run_script`): any
  GMAT script (targeting, optimisation, finite burns...) is executed by GMAT's own
  mission control sequence. A ``ReportFile`` is injected for each spacecraft so the
  complete, converged ephemeris is captured; GUI-only subscribers (OrbitView,
  OpenFramesInterface, plots) are stripped so the script runs headless.

Either way the result is a :class:`~altavista.model.ScenarioData` that
:meth:`Scenario.publish` sends to the viewer server.
"""
from __future__ import annotations

import math
import os
import re
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence, Tuple, Union

import numpy as np

from . import client
from .bodies import (ALL_BODIES, BodySampler, _mat_to_quat, coordinate_system,
                     reset_cache as reset_cs_cache, sample_times)
from .gmat_env import load_gmat
from .model import DEFAULT_COLORS, Event, Footprint, Frame, ScenarioData, Trajectory
from .timeutil import a1_to_iso, tai_to_a1_mjd

SEC_PER_DAY = 86400.0

BUILTIN_CS = {
    "EarthMJ2000Eq": ("Earth", "MJ2000Eq"),
    "EarthMJ2000Ec": ("Earth", "MJ2000Ec"),
    "EarthFixed": ("Earth", "BodyFixed"),
    "EarthICRF": ("Earth", "ICRF"),
}
AXES_SUFFIXES = {
    "MJ2000Eq": "MJ2000Eq", "MJ2000Ec": "MJ2000Ec", "Fixed": "BodyFixed",
    "ICRF": "ICRF", "Inertial": "BodyInertial",
}
GUI_SUBSCRIBERS = ("OpenFramesInterface", "OpenFramesView", "OrbitView", "GroundTrackPlot",
                   "XYPlot", "DynamicDataDisplay")
GRAVITY_FILES = {"Earth": "JGM2.cof", "Luna": "LP165P.cof", "Mars": "Mars50c.cof", "Venus": "MGNP180U.cof"}


# --------------------------------------------------------------------------- sensor footprint geometry (M6.3)
def _quat_apply(q: Sequence[float], v: Sequence[float]) -> np.ndarray:
    """Rotate vector ``v`` by unit quaternion ``q = [x, y, z, w]`` (scalar-last): the
    standard optimized quaternion-vector-rotation formula (``q * v * q_conj`` expanded
    without building the conjugate), double precision. Verified against
    :func:`altavista.bodies._mat_to_quat`'s own convention: for a random rotation matrix
    ``M``, ``_quat_apply(_mat_to_quat(M), v) == M @ v`` to float64 machine precision
    (checked interactively; not re-asserted at import time since it is a property of
    the two functions' shared convention, not of any particular ``M``/``v``)."""
    x, y, z, w = q
    qv = np.array([x, y, z], dtype=float)
    vv = np.asarray(v, dtype=float)
    t = 2.0 * np.cross(qv, vv)
    return vv + w * t + np.cross(qv, t)


def _cone_ray_directions(boresight: Sequence[float], half_angle_rad: float, n: int) -> np.ndarray:
    """``n`` unit vectors (double precision) evenly spaced around the surface of a
    cone of half-angle ``half_angle_rad`` centred on unit vector ``boresight``, all in
    whatever frame ``boresight`` is given in."""
    b = np.asarray(boresight, dtype=float)
    b = b / np.linalg.norm(b)
    arb = np.array([1.0, 0.0, 0.0]) if abs(b[0]) < 0.9 else np.array([0.0, 1.0, 0.0])
    u = np.cross(b, arb)
    u = u / np.linalg.norm(u)
    v = np.cross(b, u)
    ca, sa = math.cos(half_angle_rad), math.sin(half_angle_rad)
    thetas = 2.0 * math.pi * np.arange(n) / n
    return np.array([ca * b + sa * (math.cos(th) * u + math.sin(th) * v) for th in thetas])


def _ray_ellipsoid_intersect(origin: np.ndarray, direction: np.ndarray,
                             a_km: float, b_km: float) -> Optional[np.ndarray]:
    """Nearest point (double precision) where the ray ``origin + t*direction`` (``t >
    0``, ``direction`` unit) first crosses the oblate ellipsoid of revolution ``x^2/a^2
    + y^2/a^2 + z^2/b^2 = 1`` (``a`` = equatorial radius, ``b`` = polar radius, both
    km, ``origin`` in the same body-fixed axes the ellipsoid is defined in). ``None``
    if the ray misses the ellipsoid entirely (beyond the horizon) or only crosses it
    behind the ray's origin."""
    dx, dy, dz = direction
    px, py, pz = origin
    inv_a2 = 1.0 / (a_km * a_km)
    inv_b2 = 1.0 / (b_km * b_km)
    A = dx * dx * inv_a2 + dy * dy * inv_a2 + dz * dz * inv_b2
    B = 2.0 * (px * dx * inv_a2 + py * dy * inv_a2 + pz * dz * inv_b2)
    C = px * px * inv_a2 + py * py * inv_a2 + pz * pz * inv_b2 - 1.0
    disc = B * B - 4.0 * A * C
    if disc < 0.0:
        return None
    sq = math.sqrt(disc)
    ts = [t for t in ((-B - sq) / (2.0 * A), (-B + sq) / (2.0 * A)) if t > 0.0]
    if not ts:
        return None
    t = min(ts)
    return origin + t * direction


def nadir_footprint_half_angle_spherical(sensor_half_angle_rad: float, altitude_km: float, radius_km: float) -> float:
    """Closed-form ground half-angle (Earth-central angle, radians) of a nadir-pointing
    sensor cone of half-angle ``sensor_half_angle_rad`` at ``altitude_km`` above a
    *sphere* of radius ``radius_km`` (law of sines in the spacecraft/Earth-centre/
    footprint-edge triangle: ``sin(elevation + 90 deg) / (R+h) = sin(sensor_half_angle)
    / R``, then ``ground_half_angle = 90 deg - sensor_half_angle - elevation`` -- the
    standard SMAD/Wertz Earth-coverage-geometry relation). Returns ``0.0`` (not
    ``nan``) when the cone edge is beyond the horizon (``(R+h)/R * sin(a) > 1``) --
    same "no intersection" case :func:`_ray_ellipsoid_intersect` reports as ``None``.

    **This is a spherical closed form, not exact for the WGS84 ellipsoid** (flattening
    ~1/298.257): see ``altavista/FRAMES.md``'s footprint section for the measured
    sphere-vs-ellipsoid discrepancy for this round's example and a bound on it
    (``|R_polar - R_equatorial| = f * R_equatorial ~= 21.3 km`` for Earth).
    """
    rho_over_r = (radius_km + altitude_km) / radius_km
    x = rho_over_r * math.sin(sensor_half_angle_rad)
    if x > 1.0:
        return 0.0
    elevation = math.acos(x)
    return max(0.0, math.pi / 2.0 - sensor_half_angle_rad - elevation)


def parse_frame(spec: Union[str, Frame, Sequence[str]]) -> Frame:
    """Accepts ``"EarthMJ2000Eq"``, ``"MarsInertial"``, ``("Mars", "BodyInertial")`` or a Frame."""
    if isinstance(spec, Frame):
        return spec
    if isinstance(spec, (tuple, list)) and len(spec) == 2:
        origin, axes = spec
        return Frame(f"{origin}{axes}", origin, axes)
    name = str(spec)
    if name in BUILTIN_CS:
        o, a = BUILTIN_CS[name]
        return Frame(name, o, a)
    for body in ALL_BODIES:
        if name.startswith(body) and name[len(body):] in AXES_SUFFIXES:
            return Frame(name, body, AXES_SUFFIXES[name[len(body):]])
    raise ValueError(
        f"unknown frame {name!r}; use <Body>MJ2000Eq / <Body>MJ2000Ec / <Body>Fixed / <Body>ICRF / "
        f"<Body>Inertial, a (origin, axes) tuple, or a Frame")


def frame_from_script(text: str, name: str) -> Optional[Frame]:
    """Find ``Create CoordinateSystem name`` in a script and read its Origin/Axes."""
    if not re.search(r"^\s*Create\s+CoordinateSystem\s+.*\b%s\b" % re.escape(name), text, re.M):
        return None
    o = re.search(r"^\s*%s\.Origin\s*=\s*(\w+)" % re.escape(name), text, re.M)
    a = re.search(r"^\s*%s\.Axes\s*=\s*(\w+)" % re.escape(name), text, re.M)
    return Frame(name, o.group(1) if o else "Earth", a.group(1) if a else "MJ2000Eq")


# --------------------------------------------------------------------------- script prep
def strip_gui_subscribers(text: str) -> str:
    """Comment out GUI-only subscribers (and every line configuring them)."""
    names: List[str] = []
    out = []
    for line in text.splitlines():
        m = re.match(r"\s*Create\s+(\w+)\s+(.+?);?\s*$", line)
        if m and m.group(1) in GUI_SUBSCRIBERS:
            names += [n for n in re.split(r"[\s,]+", m.group(2)) if n]
            out.append("% [altavista stripped] " + line)
            continue
        if names and re.match(r"\s*(%s)\.\w+" % "|".join(map(re.escape, names)), line):
            out.append("% [altavista stripped] " + line)
            continue
        # Toggle / PenUp / PenDown commands referencing stripped subscribers
        if names and re.match(r"\s*(Toggle|PenUp|PenDown|MarkPoint|ClearPlot)\b.*\b(%s)\b" %
                              "|".join(map(re.escape, names)), line):
            out.append("% [altavista stripped] " + line)
            continue
        out.append(line)
    return "\n".join(out) + "\n"


def script_spacecraft(text: str) -> List[str]:
    names = []
    for grp in re.findall(r"^\s*Create\s+Spacecraft\s+(.+?);?\s*$", text, re.M):
        names += [n for n in re.split(r"[\s,]+", grp) if n]
    return names


def report_block(sat: str, cs: str, path: str) -> str:
    rf = f"altavista_{sat}"
    return (
        f"\nCreate ReportFile {rf};\n"
        f"{rf}.Filename = '{path}';\n"
        f"{rf}.Precision = 16;\n"
        f"{rf}.WriteHeaders = false;\n"
        f"{rf}.LeftJustify = On;\n"
        f"{rf}.ZeroFill = Off;\n"
        f"{rf}.FixedWidth = false;\n"
        f"{rf}.Delimiter = ',';\n"
        f"{rf}.WriteReport = true;\n"
        # 'Current' keeps only the latest solver pass, so converged Target/Optimize loops
        # appear once. ('None' drops the propagation inside solver loops entirely.)
        f"{rf}.SolverIterations = Current;\n"
        f"{rf}.Add = {{{sat}.A1ModJulian, {sat}.{cs}.X, {sat}.{cs}.Y, {sat}.{cs}.Z, "
        f"{sat}.{cs}.VX, {sat}.{cs}.VY, {sat}.{cs}.VZ}};\n"
    )


def prepare_script(text: str, frame: Frame, report_dir: str,
                   spacecraft: Optional[Sequence[str]] = None,
                   keep_gui: bool = False) -> Tuple[str, Dict[str, str]]:
    """Return (script text ready to run headless, {spacecraft name: report path})."""
    if not keep_gui:
        text = strip_gui_subscribers(text)
    sats = list(spacecraft) if spacecraft else script_spacecraft(text)
    if not sats:
        raise ValueError("no 'Create Spacecraft' found in script")
    inject = ""
    if frame.name not in BUILTIN_CS and frame_from_script(text, frame.name) is None:
        inject += (f"\nCreate CoordinateSystem {frame.name};\n{frame.name}.Origin = {frame.origin};\n"
                   f"{frame.name}.Axes = {frame.axes};\n")
    files = {}
    for s in sats:
        path = os.path.join(report_dir, f"{s}.rpt")
        files[s] = path
        inject += report_block(s, frame.name, path)
    if re.search(r"^\s*BeginMissionSequence", text, re.M):
        text = re.sub(r"^(\s*BeginMissionSequence)", inject.replace("\\", "\\\\") + r"\n\1", text, count=1, flags=re.M)
    else:
        text = text + inject
    return text, files


def _log_tail(lines: int = 15) -> str:
    """Last lines of GMAT's log file (errors from script parsing/running end up there)."""
    try:
        from .gmat_env import log_file_path
        text = log_file_path().read_text(errors="replace").splitlines()
        interesting = [l for l in text if "error" in l.lower() or "exception" in l.lower() or "**" in l]
        return "\n".join((interesting or text)[-lines:])
    except Exception as e:  # pragma: no cover
        return f"(could not read GmatLog.txt: {e})"


def parse_report(path: str) -> Trajectory:
    """Read an injected report (t, x, y, z, vx, vy, vz per row) into a Trajectory.

    Rows that step backwards in time (leftover solver iterations) are dropped so the
    result is monotonic; equal epochs are kept (impulsive burns).
    """
    tr = Trajectory(name=Path(path).stem)
    t_max = -math.inf
    with open(path) as fh:
        for line in fh:
            parts = line.strip().split(",")
            if len(parts) < 7:
                continue
            try:
                vals = [float(p) for p in parts[:7]]
            except ValueError:
                continue
            if vals[0] < t_max:
                continue
            t_max = vals[0]
            tr.append(vals[0], vals[1:7])
    return tr


def decimate(tr: Trajectory, max_points: int) -> Trajectory:
    n = len(tr.t)
    if max_points and n > max_points:
        stride = int(math.ceil(n / max_points))
        keep = list(range(0, n, stride))
        if keep[-1] != n - 1:
            keep.append(n - 1)
        tr.t = [tr.t[i] for i in keep]
        tr.pos = [tr.pos[i] for i in keep]
        tr.vel = [tr.vel[i] for i in keep]
    return tr


# --------------------------------------------------------------------------- API objects
def _vec6(v) -> List[float]:
    """Copy a GMAT Rvector6 / list-like into a plain list of 6 floats."""
    return [float(v[i]) for i in range(6)]


@dataclass
class PropagatorSpec:
    """Settings used to build a fresh GMAT PropSetup for each propagate() call."""
    force_model: object
    integrator: str = "PrinceDormand78"
    fields: Dict[str, float] = field(default_factory=dict)

    @property
    def central_body(self) -> str:
        return self.force_model.GetField("CentralBody")


class Spacecraft:
    """Wraps a GMAT Spacecraft plus the trajectory recorded for it."""

    def __init__(self, scenario: "Scenario", obj, color: Optional[str] = None):
        self.scenario = scenario
        self.obj = obj
        self.name = obj.GetName()
        self.trajectory = Trajectory(self.name, color=color)
        # last known state, in <central_body>MJ2000Eq
        self.epoch: Optional[float] = None
        self.state: Optional[List[float]] = None
        self.central_body: str = "Earth"
        # M6.3 attitude sampling cache (see Scenario._attitude_reference_frame):
        # GMAT's attitude model name and the Frame its DCM is expressed against, both
        # cheap GMAT field reads but invariant for the run once an attitude model is
        # configured, so cached after the first lookup rather than re-queried every
        # sample.
        self._attitude_model: Optional[str] = None
        self._attitude_ref_frame: Optional[Frame] = None
        # M6.3: whether *this caller* (not GMAT's own default) asked for an attitude
        # model. GMAT gives every Spacecraft a default Attitude ("CoordinateSystemFixed",
        # verified empirically: HasAttitude() is True even when a script/caller never
        # sets the Attitude field at all) -- so HasAttitude() alone cannot tell "the
        # caller wants an attitude stream sampled" apart from "GMAT's silent default
        # nobody asked for". Tracked here instead, set only by set_field("Attitude",
        # ...) (below) and Scenario.spacecraft(Attitude=...), so every example/script
        # that never mentions Attitude keeps exactly its pre-M6.3 behaviour (no
        # Trajectory.attitude populated, viewer fallback unchanged) even though GMAT's
        # own HasAttitude() would say True.
        self._attitude_explicit: bool = False

    def __repr__(self) -> str:
        return f"<Spacecraft {self.name}: {len(self.trajectory.t)} samples>"

    @property
    def color(self):
        return self.trajectory.color

    def set_field(self, name: str, value):
        if name == "Attitude":
            self._attitude_explicit = True
        return self.obj.SetField(name, value)

    def keplerian(self) -> List[float]:
        """[SMA, ECC, INC, RAAN, AOP, TA] of the current state in the spacecraft's coordinate system."""
        self.scenario.gmat.Initialize()
        return _vec6(self.obj.GetKeplerianState())

    def cartesian(self) -> List[float]:
        """[x, y, z, vx, vy, vz] of the current state in the spacecraft's coordinate system."""
        self.scenario.gmat.Initialize()
        return _vec6(self.obj.GetCartesianState())

    def _write_back(self):
        """Push our cached state/epoch into the GMAT object (so later propagations continue)."""
        if self.state is None:
            return
        self.obj.SetField("DateFormat", "A1ModJulian")
        self.obj.SetField("Epoch", repr(self.epoch))
        self.obj.SetField("CoordinateSystem", f"{self.central_body}MJ2000Eq" if self.central_body != "Earth"
                          else "EarthMJ2000Eq")
        self.obj.SetField("DisplayStateType", "Cartesian")
        for k, v in zip(("X", "Y", "Z", "VX", "VY", "VZ"), self.state):
            self.obj.SetField(k, float(v))


class Scenario:
    """Collects spacecraft trajectories, bodies and events, then publishes them."""

    def __init__(self, name: str = "scenario", frame: Union[str, Frame, Sequence[str]] = "EarthMJ2000Eq",
                 bodies: Optional[Sequence[str]] = None, url: Optional[str] = None, log: bool = False):
        self.gmat = load_gmat(log=log)
        self.name = name
        self.frame = parse_frame(frame)
        self.bodies_override = list(bodies) if bodies else None
        self.url = url
        self.spacecraft_list: List[Spacecraft] = []
        self.events: List[Event] = []
        self.meta: Dict[str, object] = {}
        self._default_prop: Optional[PropagatorSpec] = None
        self._prop_counter = 0
        self._maneuver_counter = 0
        self._cc = None
        # Entity-relative frames declared via frame_ric()/frame_vnb()/frame_vvlh()
        # (M4.1): {"id", "kind" ("ric"/"vnb"/"vvlh"), "entity_id", "reference_entity_id",
        # "reference_body"}. Registered through altavista.frames.FrameRegistry lazily, at
        # build() time (see _build_frames()) -- not here, since the entity may not be
        # propagated yet when this is called.
        self._entity_frame_decls: List[Dict[str, str]] = []
        # Sensor footprints declared via footprint() (M6.3): computed lazily at
        # build() time, from each declared spacecraft's already-recorded
        # trajectory/attitude, the same "declare now, compute at build" pattern as
        # _entity_frame_decls above.
        self._footprint_decls: List[Dict[str, object]] = []

    # ---------------------------------------------------------------- object creation
    def _next_color(self) -> str:
        return DEFAULT_COLORS[len(self.spacecraft_list) % len(DEFAULT_COLORS)]

    def spacecraft(self, name: str, epoch: Optional[str] = None, keplerian: Optional[Dict[str, float]] = None,
                   cartesian: Optional[Sequence[float]] = None, coordinate_system: str = "EarthMJ2000Eq",
                   date_format: str = "UTCGregorian", color: Optional[str] = None, **fields) -> Spacecraft:
        """Create (or adopt) a GMAT Spacecraft.

        ``keplerian`` keys: SMA, ECC, INC, RAAN, AOP, TA (km / deg). ``cartesian``: [x,y,z,vx,vy,vz].
        Extra ``fields`` are passed to ``SetField`` (DryMass, Cd, DragArea, ...).
        """
        g = self.gmat
        obj = g.GetObject(name) if g.Exists(name) else g.Construct("Spacecraft", name)
        if epoch:
            obj.SetField("DateFormat", date_format)
            obj.SetField("Epoch", epoch)
        obj.SetField("CoordinateSystem", coordinate_system)
        if keplerian:
            obj.SetField("DisplayStateType", "Keplerian")
            for k in ("SMA", "ECC", "INC", "RAAN", "AOP", "TA"):
                if k in keplerian:
                    obj.SetField(k, float(keplerian[k]))
        elif cartesian is not None:
            obj.SetField("DisplayStateType", "Cartesian")
            for k, v in zip(("X", "Y", "Z", "VX", "VY", "VZ"), cartesian):
                obj.SetField(k, float(v))
        for k, v in fields.items():
            obj.SetField(k, v)
        sc = self.adopt(obj, color=color)
        if "Attitude" in fields:
            sc._attitude_explicit = True  # see Spacecraft.set_field's docstring comment
        return sc

    def adopt(self, obj, color: Optional[str] = None) -> Spacecraft:
        """Adopt an existing GMAT Spacecraft object built with the raw API."""
        for s in self.spacecraft_list:
            if s.obj.GetName() == obj.GetName():
                return s
        sc = Spacecraft(self, obj, color=color or self._next_color())
        self.spacecraft_list.append(sc)
        return sc

    def force_model(self, central_body: str = "Earth", degree: int = 8, order: int = 8,
                    point_masses: Sequence[str] = ("Luna", "Sun"), drag: Optional[str] = None,
                    srp: bool = False, potential_file: Optional[str] = None, name: Optional[str] = None):
        """Build a GMAT ForceModel. ``drag`` may be e.g. ``"JacchiaRoberts"`` or ``"MSISE90"``."""
        g = self.gmat
        # id(self): GMAT's object namespace is process-wide (a singleton -- module
        # docstrings across this package), but self.spacecraft_list/_prop_counter are
        # per-Scenario-instance counters that restart at 0 for every new Scenario(). Two
        # Scenario objects built in the same process (e.g. two tests in one pytest run)
        # could otherwise generate the identical default name and collide on GMAT's
        # "already a GravityField force in place" error the second time a ForceModel
        # got auto-built. id(self) is unique for the lifetime of this Python object, so
        # folding it in makes the auto-generated name unique per Scenario even when
        # every other counter coincides; a caller-supplied `name` is untouched.
        name = name or f"gv_FM_{central_body}_{id(self)}_{len(self.spacecraft_list)}_{self._prop_counter}"
        fm = g.Construct("ForceModel", name)
        fm.SetField("CentralBody", central_body)
        pot = potential_file or GRAVITY_FILES.get(central_body)
        if degree > 0 and pot:
            grav = g.Construct("GravityField")
            grav.SetField("BodyName", central_body)
            grav.SetField("PotentialFile", pot)
            grav.SetField("Degree", int(degree))
            grav.SetField("Order", int(order))
            fm.AddForce(grav)
        else:
            pm = g.Construct("PointMassForce")
            pm.SetField("BodyName", central_body)
            fm.AddForce(pm)
        for b in point_masses:
            if b == central_body:
                continue
            pm = g.Construct("PointMassForce")
            pm.SetField("BodyName", b)
            fm.AddForce(pm)
        if drag:
            df = g.Construct("DragForce")
            df.SetField("AtmosphereModel", drag)
            atmos = g.Construct(drag)
            df.SetReference(atmos)
            fm.AddForce(df)
        if srp:
            fm.AddForce(g.Construct("SolarRadiationPressure"))
        return fm

    def propagator(self, force_model=None, integrator: str = "PrinceDormand78", max_step: float = 300.0,
                   min_step: float = 0.0, initial_step: float = 60.0, accuracy: float = 1e-12) -> PropagatorSpec:
        fm = force_model if force_model is not None else self.force_model()
        return PropagatorSpec(fm, integrator, {"InitialStepSize": initial_step, "Accuracy": accuracy,
                                               "MinStep": min_step, "MaxStep": max_step})

    def _default_propagator(self) -> PropagatorSpec:
        if self._default_prop is None:
            self._default_prop = self.propagator()
        return self._default_prop

    # ---------------------------------------------------------------- frame conversion
    def _converter(self):
        if self._cc is None:
            self._cc = self.gmat.CoordinateConverter()
        return self._cc

    def _to_frame(self, t: float, state: Sequence[float], central_body: str) -> List[float]:
        """Convert a <central_body>MJ2000Eq state into the scenario frame."""
        if central_body == self.frame.origin and self.frame.axes == "MJ2000Eq":
            return [float(x) for x in state]
        g = self.gmat
        src = coordinate_system(Frame(f"{central_body}MJ2000Eq", central_body, "MJ2000Eq"))
        dst = coordinate_system(self.frame)
        s_in = g.Rvector6(*[float(x) for x in state])
        s_out = g.Rvector6()
        self._converter().Convert(g.A1Mjd(t), s_in, src, s_out, dst)
        return [s_out[i] for i in range(6)]

    # ---------------------------------------------------------------- attitude (M6.3)
    def _rotation_matrix_between(self, src_frame: Frame, dst_frame: Frame, t: float) -> List[List[float]]:
        """3x3 rotation ``R`` (``v_dst = R @ v_src``) GMAT's ``CoordinateConverter`` uses
        between two coordinate systems at A1MJD ``t``, via a dummy-vector ``Convert()``
        call (the same technique ``altavista.bodies.BodySampler.orientation`` and
        ``altavista.frames.FrameRegistry.rotation_matrix`` use to expose GMAT's own
        rotation). Duplicated here rather than calling into ``altavista.frames`` because
        that module's version needs a live ``FrameRegistry`` with registered
        ``FrameDefinition`` ids, which attitude sampling during :meth:`propagate` does
        not have -- this one only needs two native GMAT ``CoordinateSystem`` objects.
        """
        if src_frame.name == dst_frame.name:
            return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        g = self.gmat
        src_cs = coordinate_system(src_frame)
        dst_cs = coordinate_system(dst_frame)
        dummy_in = g.Rvector6(1.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        dummy_out = g.Rvector6()
        self._converter().Convert(g.A1Mjd(float(t)), dummy_in, src_cs, dummy_out, dst_cs)
        R = self._converter().GetLastRotationMatrix()
        return [[R.GetElement(i, j) for j in range(3)] for i in range(3)]

    def _attitude_reference_frame(self, s: "Spacecraft", central_body: str) -> Frame:
        """The GMAT ``CoordinateSystem`` (as a :class:`~altavista.model.Frame`) that
        ``s.obj.GetAttitude(t)``'s cosine matrix is expressed against. Cached on ``s``
        after the first lookup (both GMAT calls this makes are cheap field reads, but
        the attitude model configured on a spacecraft is not expected to change mid-run).

        ``NadirPointing`` is special-cased: verified against GMAT's own source
        (``third_party/gmat-src/src/base/attitude/NadirPointing.cpp``,
        ``ComputeCosineMatrixAndAngularVelocity``) to compute its DCM directly from
        ``owningSC->GetMJ2000State(theTime)`` -- the spacecraft's own default MJ2000
        state -- and to never read the ``AttitudeCoordinateSystem`` field at all.  That
        default MJ2000 frame is ``<central_body>MJ2000Eq`` (the same frame
        :meth:`_to_frame` converts *from* for position/velocity), independent of
        whatever ``AttitudeCoordinateSystem`` happens to report (GMAT leaves that field
        at its class default, ``"EarthMJ2000Eq"``, whether or not NadirPointing reads
        it -- for a non-Earth ``central_body`` those two would disagree, which is why
        this is special-cased rather than trusted generically).

        Every other attitude model this round supports (``CoordinateSystemFixed``,
        ``Spinner``, a CCSDS-AEM file) reads GMAT's ``AttitudeCoordinateSystem`` field
        (verified empirically: ``Spacecraft.GetStringParameter("AttitudeCoordinateSystem")``
        delegates to the owned ``Attitude`` object and returns e.g. ``"EarthMJ2000Eq"``)
        -- used here as-is. If that name is not yet a constructed GMAT
        ``CoordinateSystem`` (an edge case outside this round's examples, which all use
        the default), :func:`altavista.bodies.coordinate_system` would construct a new one
        assuming ``(central_body, MJ2000Eq)``, which may not match; not hit by any
        scenario this worker ships, flagged here rather than silently risking it.
        """
        if s._attitude_ref_frame is not None:
            return s._attitude_ref_frame
        model = s.obj.GetStringParameter("Attitude")
        s._attitude_model = model
        if model == "NadirPointing":
            frame = Frame(f"{central_body}MJ2000Eq", central_body, "MJ2000Eq")
        else:
            cs_name = s.obj.GetStringParameter("AttitudeCoordinateSystem")
            frame = Frame(cs_name, central_body, "MJ2000Eq")
        s._attitude_ref_frame = frame
        return frame

    def _attitude_quat_in_frame(self, s: "Spacecraft", t: float, central_body: str) -> Optional[List[float]]:
        """Sample ``s``'s GMAT-configured attitude at A1MJD ``t``, if any, as a unit
        quaternion ``[x, y, z, w]`` (scalar-last) such that applying it to a vector
        given in the spacecraft's body frame yields that vector's coordinates in
        ``self.frame`` (the scenario frame) -- the same "axis direction expressed in
        the parent frame" convention already used by
        ``altavista.bodies.BodySampler.orientation`` (body-fixed -> scenario, for
        ``BodyTrack.quat``) and by ``web/js/frames.js``'s RIC/VNB/VVLH
        ``axesForKind``/``quaternionFromAxes`` fallback (both feed directly into
        Three.js's ``Object3D.quaternion``, which is defined to transform a
        *local-frame* vector into its *parent-frame* representation -- see
        ``altavista/FRAMES.md``'s attitude-convention section for the full derivation).
        Returns ``None`` unless the caller explicitly configured an attitude model on
        ``s`` (``s._attitude_explicit``, set by ``Spacecraft.set_field("Attitude", ...)``
        or ``Scenario.spacecraft(..., Attitude=...)``) -- deliberately *not* GMAT's own
        ``Spacecraft.HasAttitude()``, which is ``True`` for every Spacecraft regardless
        (verified empirically: GMAT defaults every Spacecraft's Attitude to
        ``"CoordinateSystemFixed"`` even when nothing ever sets the ``Attitude`` field),
        so trusting it here would silently start attaching an unrequested
        "CoordinateSystemFixed" attitude stream to every existing altavista scenario/example
        that never asked for one -- exactly the "old scenarios stay unchanged" rule this
        module's own docstrings and ``ScenarioData.to_dict()``'s additive-fields
        discipline exist to protect.

        GMAT's own ``Spacecraft.GetAttitude(t)`` (``attitude->GetCosineMatrix(t)``,
        verified against ``third_party/gmat-src/src/base/attitude/Attitude.hpp``'s "the
        current rotation matrix (from inertial to body)" comment and confirmed
        numerically against a known nadir vector) is the *opposite* direction: ``v_body
        = dcm @ v_attitudeCS`` (GMAT/CCSDS "reference-from-body" convention, scalar-last
        per ``AttitudeConversionUtility.cpp``'s "CCSDS definition of quaternions where
        qc = q4"). This method transposes ``dcm`` (equivalent to conjugating GMAT's own
        quaternion) before composing with the attitude-CS -> scenario-frame rotation, so
        the quaternion this module emits is consistently "axis direction in the parent
        frame" everywhere in altavista, not GMAT's raw convention.

        ``GetAttitude`` returns a reference to the ``Attitude`` object's own mutable
        ``dcm`` member (confirmed empirically: two calls at different epochs, read
        lazily, alias the same underlying matrix) -- its elements are copied out
        immediately, before any other GMAT call that might recompute it, exactly like
        ``BodySampler.orientation`` already does for body-fixed orientations.
        """
        if not s._attitude_explicit:
            return None
        dcm_native = s.obj.GetAttitude(float(t))
        dcm = np.array([[dcm_native.GetElement(i, j) for j in range(3)] for i in range(3)])
        att_frame = self._attitude_reference_frame(s, central_body)
        R = np.array(self._rotation_matrix_between(att_frame, self.frame, t))
        M = R @ dcm.T
        return _mat_to_quat(M.tolist())

    # ---------------------------------------------------------------- propagation
    def propagate(self, spacecraft: Union[Spacecraft, Sequence[Spacecraft]], days: float = 0.0,
                  hours: float = 0.0, seconds: float = 0.0, step: float = 60.0,
                  propagator: Optional[PropagatorSpec] = None) -> None:
        """Advance one or more spacecraft, recording their state every ``step`` seconds."""
        sats = [spacecraft] if isinstance(spacecraft, Spacecraft) else list(spacecraft)
        total = days * SEC_PER_DAY + hours * 3600.0 + seconds
        if total <= 0:
            raise ValueError("propagate() needs a positive duration (days/hours/seconds)")
        g = self.gmat
        spec = propagator or self._default_propagator()
        central = spec.central_body
        attitude_sats = any(s._attitude_explicit for s in sats)

        for s in sats:
            s._write_back()
        g.Initialize()

        self._prop_counter += 1
        # id(self): same reasoning as force_model()'s default name above -- GMAT's
        # object namespace is process-wide, but self._prop_counter restarts at 0 for
        # every new Scenario(), so two Scenario objects in one process could otherwise
        # both build their first Propagator as "gv_prop_1" and silently share/corrupt
        # each other's PropSetup (verified: this caused a real cross-test failure,
        # tests/test_frames.py's M6.3 attitude tests, before this fix).
        pname = f"gv_prop_{id(self)}_{self._prop_counter}"
        prop = g.Construct("Propagator", pname)
        prop.SetReference(g.Construct(spec.integrator, pname + "_int"))
        prop.SetReference(spec.force_model)
        for k, v in spec.fields.items():
            prop.SetField(k, float(v))
        for s in sats:
            prop.AddPropObject(s.obj)
        prop.PrepareInternals()
        gator = prop.GetPropagator()

        t0 = sats[0].obj.GetEpoch()
        for s in sats:
            if abs(s.obj.GetEpoch() - t0) > 1e-9:
                raise ValueError(f"{s.name} epoch differs from {sats[0].name}; GMAT propagates a group at one epoch")

        def record(elapsed: float, state):
            t = t0 + elapsed / SEC_PER_DAY
            for i, s in enumerate(sats):
                raw = [state[6 * i + j] for j in range(6)]
                s.epoch, s.state, s.central_body = t, raw, central
                s.trajectory.append(t, self._to_frame(t, raw, central))
                q = self._attitude_quat_in_frame(s, t, central)
                if q is not None:
                    s.trajectory.attitude.append(q)

        # Skip the initial sample when it would duplicate the previous call's last one
        # (a maneuver already appended the post-burn state at this epoch).
        if any(not s.trajectory.t or abs(s.trajectory.t[-1] - t0) > 1e-9 for s in sats):
            record(0.0, gator.GetState())
        elapsed = 0.0
        while elapsed < total - 1e-9:
            dt = min(step, total - elapsed)
            # Step(dt) sub-steps internally but returns False once MaxStepAttempts (default 50,
            # rejected attempts included) is exhausted, leaving the state partly advanced. A
            # caller that ignores the boolean gets a plausible wrong trajectory
            # (tests/test_gmat_step_return.py). Keep `step` to a few MaxStep, or raise
            # MaxStepAttempts on the PropagatorSpec.
            if not gator.Step(dt):
                raise RuntimeError(
                    f"GMAT Propagator.Step({dt}) failed at elapsed={elapsed:.1f} s: MaxStepAttempts exhausted. "
                    f"Use a smaller `step` (a few times the propagator's MaxStep) or raise MaxStepAttempts.")
            elapsed += dt
            # GMAT's low-level Step() advances only the integrator's own internal state
            # buffer (what gator.GetState() below reads) -- it does NOT, by itself, push
            # that state back into the PropObject's (Spacecraft's) own fields. That is
            # harmless for pos/vel (record() reads gator.GetState() directly, never the
            # Spacecraft object's own X/Y/Z), but GMAT's attitude models read the
            # spacecraft's state through the *object* (e.g. NadirPointing's
            # ComputeCosineMatrixAndAngularVelocity calls owningSC->GetMJ2000State(...)) --
            # verified empirically: without this call, Spacecraft.GetAttitude(t) returns
            # the SAME (stale, pre-loop) matrix for every t during a propagate() run,
            # growing from ~0 to ~2.0 in nadir-alignment residual over one loop. Explicit
            # sync via the low-level Propagator API's UpdateSpaceObject() before sampling
            # attitude.
            if attitude_sats:
                gator.UpdateSpaceObject()
            record(elapsed, gator.GetState())
        for s in sats:
            s._write_back()

    def maneuver(self, spacecraft: Spacecraft, dv: Sequence[float], frame: str = "VNB",
                 name: Optional[str] = None) -> None:
        """Apply an impulsive delta-v (km/s) to a spacecraft's current state.

        ``frame`` (case-insensitive):

        * ``"VNB"`` (default, unchanged by M11.3) -- velocity / normal / binormal, built
          in Python from the spacecraft's own r/v. This is the reference path
          ``goldens/gen_leo_1day_maneuver_vnb.py`` pins; its formula must not change.
        * ``"inertial"`` (or any other unrecognized string, unchanged) -- ``dv`` applied
          unrotated.
        * ``"RIC"`` -- radial / in-track / cross-track (``XAxis = R``, ``ZAxis = N``,
          question 73), realized through a real GMAT ``ImpulsiveBurn`` whose
          ``CoordinateSystem`` is an ObjectReferenced RIC system built via
          :class:`altavista.frames.FrameRegistry` (question 102, M11.3) -- never a second
          hand-rolled rotation. See :meth:`_fire_impulsive_burn`.
        * ``"VVLH"`` (M12.4, question 106; the current name of what M11.3 called
          ``"LVLH"`` below) -- this platform's own ratified ``AXES_KIND_VVLH``
          (``YAxis = -N``, ``ZAxis = -R``, GMAT derives ``XAxis = N x R``, question 73),
          realized the same way as ``"RIC"`` above: a real GMAT ``ImpulsiveBurn`` whose
          ``CoordinateSystem`` is an ObjectReferenced system built via
          :class:`altavista.frames.FrameRegistry`, never a second hand-rolled rotation.
          See :meth:`_fire_impulsive_burn`.
        * ``"LVLH"`` -- fired through GMAT's own native local burn axes
          (``ImpulsiveBurn.CoordinateSystem = Local``, ``Axes = LVLH``), the literal GMAT
          field -- kept as its own option *only* because it emits that literal GMAT axes
          setting; it is **not** altavista's ratified ``AXES_KIND_VVLH`` above. **Verified
          empirically (M11.3) that GMAT's ``Axes = LVLH`` is X = R, Y = N x R (in-track),
          Z = N -- numerically identical to RIC's X=R,Z=N convention (and to the
          ``"RIC"`` option above), and different from this platform's ratified
          ``AXES_KIND_VVLH`` (Z = -R, Y = -N, X = N x R, question 73; renamed from
          ``AXES_KIND_LVLH`` by question 106 for exactly this reason -- a caller who
          wants GMAT's own literal LVLH burn axes should ask for ``"RIC"`` or ``"LVLH"``,
          never assume ``"VVLH"``/``AXES_KIND_VVLH`` matches it).** ``altavista/FRAMES.md``
          documents the mapping; this method does not reconcile the two -- it reports
          whatever GMAT's ``ImpulsiveBurn`` actually computes (:meth:`_fire_impulsive_burn`'s
          own doc comment has the detail).
        """
        s = spacecraft
        if s.state is None:
            self.gmat.Initialize()
            s.epoch = s.obj.GetEpoch()
            s.state = _vec6(s.obj.GetState().GetState())
            s.central_body = "Earth"
        r = s.state[0:3]
        v = s.state[3:6]
        frame_upper = frame.upper()
        if frame_upper == "VNB":
            vn = math.sqrt(sum(x * x for x in v))
            V = [x / vn for x in v]
            h = [r[1] * v[2] - r[2] * v[1], r[2] * v[0] - r[0] * v[2], r[0] * v[1] - r[1] * v[0]]
            hn = math.sqrt(sum(x * x for x in h))
            N = [x / hn for x in h]
            B = [V[1] * N[2] - V[2] * N[1], V[2] * N[0] - V[0] * N[2], V[0] * N[1] - V[1] * N[0]]
            dvi = [dv[0] * V[i] + dv[1] * N[i] + dv[2] * B[i] for i in range(3)]
        elif frame_upper in ("RIC", "VVLH", "LVLH"):
            dvi = self._fire_impulsive_burn(s, dv, frame_upper)
        else:
            dvi = [float(x) for x in dv]
        s.state = r + [v[i] + dvi[i] for i in range(3)]
        s._write_back()
        s.trajectory.append(s.epoch, self._to_frame(s.epoch, s.state, s.central_body))
        q = self._attitude_quat_in_frame(s, s.epoch, s.central_body)
        if q is not None:
            s.trajectory.attitude.append(q)
        mag = math.sqrt(sum(x * x for x in dvi))
        self.event(name or f"{s.name} burn", s.epoch, type="maneuver", spacecraft=s.name,
                   detail=f"dv = {mag * 1000:.2f} m/s ({frame})")

    def _fire_impulsive_burn(self, s: Spacecraft, dv: Sequence[float], frame_upper: str) -> List[float]:
        """Apply ``dv`` (km/s, in ``frame_upper``'s own basis) to ``s`` through a real GMAT
        ``ImpulsiveBurn`` object (``Construct``/``SetField``/``Fire``, never a hand-rolled
        rotation matrix) and return the delta-v GMAT actually applied, expressed in ``s``'s
        own inertial frame (``<central_body>MJ2000Eq``, km/s) via
        ``ImpulsiveBurn.GetDeltaVInertial()``. Called by :meth:`maneuver` for
        ``frame="RIC"``/``"VVLH"``/``"LVLH"`` (question 102, M11.3; ``"VVLH"`` added by
        M12.4, question 106).

        ``frame_upper == "RIC"``: the burn's ``CoordinateSystem`` field is set to an
        ObjectReferenced RIC system's GMAT name, built by registering a fresh
        ``core_pb2.FrameDefinition(axes=AXES_KIND_RIC, entity_id=s.name,
        reference_entity_id=s.name, reference_body=s.central_body)`` against a fresh
        :class:`altavista.frames.FrameRegistry` -- the exact same registry/mechanism
        :meth:`_build_frames` uses for ``frame_ric()`` declarations (``XAxis = R``,
        ``ZAxis = N``, question 73), not a second implementation of ObjectReferenced
        construction. A fresh registry is built on every call (never cached on ``self``)
        for the same reason :meth:`_build_frames` gives: GMAT is a process-wide singleton
        and a script run elsewhere in the process (``LoadScript``) can wipe the
        configuration a cached registry's ``CoordinateSystem`` handles depend on.

        ``frame_upper == "VVLH"`` (M12.4, question 106): the same ObjectReferenced
        mechanism as ``"RIC"`` above, but with ``core_pb2.AXES_KIND_VVLH``
        (``YAxis = -N``, ``ZAxis = -R``, GMAT derives ``XAxis = N x R``) -- this
        platform's own ratified convention (question 73), the same one
        :meth:`frame_vvlh` declares for the viewer's frame graph. This realizes our
        convention as a *real* GMAT burn, unlike the pre-M12.4 state where only a
        Python-side unit test exercised this triad.

        ``frame_upper == "LVLH"``: the burn's ``CoordinateSystem`` field is left at GMAT's
        own default, ``Local``, with ``Origin = s.central_body`` and ``Axes = LVLH`` --
        GMAT's own native local burn axes, not altavista's ratified ``AXES_KIND_VVLH``.
        **Measured empirically against this GMAT build (scratch spacecraft, dv=(1,1,1) at
        r=(7000,0,0) km / v=(0,7.5,0) km/s so R=+X, N=+Z, in-track=N×R=+Y): GMAT's
        ``Axes = LVLH`` returned an inertial dv of exactly (1,1,1), i.e. X_lvlh = R,
        Y_lvlh = N×R (in-track), Z_lvlh = N** -- matching the GMAT help text for
        ``ImpulsiveBurn`` ("the X-axis points from the center of the [body] to the
        spacecraft ... the Z-axis is along the instantaneous orbit normal ... the Y-axis
        completes the right-handed set") and numerically **identical to the RIC branch
        above** (X=R, Z=N -- GMAT derives Y=Z×X=N×R the same way). This is a genuinely
        different triad from this platform's ratified ``AXES_KIND_VVLH`` (Z = -R
        (nadir), Y = -N, X = N×R (in-track); question 73's VVLH convention, renamed from
        ``AXES_KIND_LVLH`` by question 106 for exactly this reason -- which would map
        the same dv=(1,1,1) to (-1,1,-1) instead -- see
        ``crates/av-kernel/src/drm/maneuver.rs::dv_to_inertial``'s ``AxesKind::Vvlh``
        arm, and its own unit test using this exact r/v/dv). ``altavista/FRAMES.md``
        records this mapping; nothing in this method (or in ``goldens/
        gen_leo_1day_maneuver_gmat_lvlh.py``, which calls this same path) reorients the
        result to force it to agree with ``AXES_KIND_VVLH`` -- it reports GMAT's own
        number.

        All branches then: ``SetField`` the three ``Element`` components, ``Initialize``
        GMAT (the burn -- and, for RIC/VVLH, the freshly-built ``CoordinateSystem`` --
        are new objects), bind the burn to ``s.obj`` via ``SetSolarSystem``/
        ``SetSpacecraftToManeuver``, ``Initialize`` the burn itself, then ``Fire()``
        (raising if it returns ``False``, the same "check the boolean" discipline
        :meth:`propagate` uses for ``Propagator.Step``) and read back
        ``GetDeltaVInertial()``. The burn object is named deterministically
        (``gv_burn_<ric|vvlh|lvlh>_<id(self)>_<self._maneuver_counter>``, ``id(self)``
        disambiguating concurrent ``Scenario`` instances the same way
        :meth:`propagate`'s propagator names do) and constructed fresh every call rather
        than reused, since a stale ``Fire()`` binding to a previous spacecraft/dv would be
        exactly the kind of silent cross-call state this module avoids elsewhere.
        """
        from . import frames as frames_mod
        from .pb import core_pb2

        g = self.gmat
        axes_word = frame_upper.lower()  # "ric" / "vvlh" / "lvlh"
        cs_name: Optional[str] = None
        if axes_word == "ric":
            registry = frames_mod.FrameRegistry()
            fd = registry.register(core_pb2.FrameDefinition(
                id=f"gv_maneuver_ric_frame_{id(self)}_{self._maneuver_counter}",
                axes=core_pb2.AXES_KIND_RIC, entity_id=s.name,
                reference_entity_id=s.name, reference_body=s.central_body))
            cs_name = fd.gmat_name
        elif axes_word == "vvlh":
            registry = frames_mod.FrameRegistry()
            fd = registry.register(core_pb2.FrameDefinition(
                id=f"gv_maneuver_vvlh_frame_{id(self)}_{self._maneuver_counter}",
                axes=core_pb2.AXES_KIND_VVLH, entity_id=s.name,
                reference_entity_id=s.name, reference_body=s.central_body))
            cs_name = fd.gmat_name

        self._maneuver_counter += 1
        burn_name = f"gv_burn_{axes_word}_{id(self)}_{self._maneuver_counter}"
        burn = g.Construct("ImpulsiveBurn", burn_name)
        if cs_name is not None:
            burn.SetField("CoordinateSystem", cs_name)
        else:
            burn.SetField("CoordinateSystem", "Local")
            burn.SetField("Origin", s.central_body)
            burn.SetField("Axes", "LVLH")
        burn.SetField("Element1", float(dv[0]))
        burn.SetField("Element2", float(dv[1]))
        burn.SetField("Element3", float(dv[2]))
        g.Initialize()

        burn.SetSolarSystem(g.GetSolarSystem())
        burn.SetSpacecraftToManeuver(s.obj)
        burn.Initialize()
        if not burn.Fire():
            cs_desc = cs_name if cs_name is not None else f"Local/{s.central_body}/LVLH"
            raise RuntimeError(
                f"GMAT ImpulsiveBurn.Fire() failed for {s.name} (frame={frame_upper}); "
                f"burn={burn_name!r} coordinate_system={cs_desc!r}")
        dv_inertial = burn.GetDeltaVInertial()
        return [float(dv_inertial[0]), float(dv_inertial[1]), float(dv_inertial[2])]

    # ---------------------------------------------------------------- entity-relative frames (M4.1)
    def frame_ric(self, entity: Union[Spacecraft, str], reference_body: Optional[str] = None,
                 frame_id: Optional[str] = None) -> str:
        """Declare ``entity``'s RIC (radial / in-track / cross-track) frame for the
        viewer's frame graph (docs/open-questions.md question 10: RPO/OSAM relative
        frames). Does not touch GMAT immediately -- the declaration is realized through
        :class:`altavista.frames.FrameRegistry` at :meth:`build` time (see
        :meth:`_build_frames`), the same registry the frame service uses, never
        hand-built here. ``reference_body`` defaults to this scenario's own central
        body (``self.frame.origin``). Returns the frame id (``frame_id`` if given, else
        ``f"{entity_name}_ric"``), which also appears as the ``id`` of the matching
        entry in ``ScenarioData.to_dict()["frames"]``.
        """
        return self._declare_object_referenced_frame("ric", entity, reference_body, frame_id)

    def frame_vnb(self, entity: Union[Spacecraft, str], reference_body: Optional[str] = None,
                 frame_id: Optional[str] = None) -> str:
        """Declare ``entity``'s VNB (velocity / normal / binormal) frame -- see
        :meth:`frame_ric` for the mechanism; identical except for axes kind."""
        return self._declare_object_referenced_frame("vnb", entity, reference_body, frame_id)

    def frame_vvlh(self, entity: Union[Spacecraft, str], reference_body: Optional[str] = None,
                  frame_id: Optional[str] = None) -> str:
        """Declare ``entity``'s VVLH (vehicle-velocity-local-horizontal) frame -- see
        :meth:`frame_ric`; identical except for axes kind. Renamed from ``frame_lvlh``
        by M12.4 (question 106) alongside ``AXES_KIND_LVLH`` -> ``AXES_KIND_VVLH``; there
        is no ``frame_lvlh`` alias -- the old name is gone, not merely deprecated."""
        return self._declare_object_referenced_frame("vvlh", entity, reference_body, frame_id)

    def _declare_object_referenced_frame(self, kind: str, entity: Union[Spacecraft, str],
                                         reference_body: Optional[str], frame_id: Optional[str]) -> str:
        name = entity.name if isinstance(entity, Spacecraft) else str(entity)
        ref_body = reference_body or self.frame.origin
        fid = frame_id or f"{name}_{kind}"
        self._entity_frame_decls.append({
            "id": fid, "kind": kind, "entity_id": name,
            "reference_entity_id": name, "reference_body": ref_body,
        })
        return fid

    def event(self, name: str, t: float, type: str = "marker", spacecraft: Optional[str] = None,
              detail: Optional[str] = None) -> Event:
        ev = Event(name=name, t=float(t), type=type, spacecraft=spacecraft, detail=detail)
        self.events.append(ev)
        return ev

    # ---------------------------------------------------------------- scripts
    @classmethod
    def from_script(cls, path: Union[str, os.PathLike], name: Optional[str] = None,
                    frame: Union[str, Frame, Sequence[str]] = "EarthMJ2000Eq", **kw) -> "Scenario":
        """Run a GMAT script file headless and capture every spacecraft's trajectory."""
        path = Path(path)
        text = path.read_text()
        sc = cls._from_text(text, name or path.stem, frame, source=str(path), **kw)
        return sc

    @classmethod
    def from_script_text(cls, text: str, name: str = "script",
                         frame: Union[str, Frame, Sequence[str]] = "EarthMJ2000Eq", **kw) -> "Scenario":
        """Run GMAT script text (e.g. built with a Python template) and capture trajectories."""
        return cls._from_text(text, name, frame, source="<text>", **kw)

    @classmethod
    def _from_text(cls, text, name, frame, source, bodies=None, url=None, log=False, **kw):
        fr = None
        if isinstance(frame, str):
            try:
                fr = parse_frame(frame)
            except ValueError:
                fr = frame_from_script(text, frame)
                if fr is None:
                    raise
        sc = cls(name, frame=fr or frame, bodies=bodies, url=url, log=log)
        sc.run_script(text, **kw)
        sc.meta["source"] = source
        return sc

    def run_script(self, text_or_path: Union[str, os.PathLike], spacecraft: Optional[Sequence[str]] = None,
                   max_points: Optional[int] = None, keep_gui: bool = False,
                   colors: Optional[Dict[str, str]] = None, workdir: Optional[str] = None) -> None:
        """Execute a GMAT script (text or path) and add its spacecraft/events to this scenario."""
        g = self.gmat
        p = Path(str(text_or_path))
        if "\n" not in str(text_or_path) and p.exists():
            text = p.read_text()
        else:
            text = str(text_or_path)
        # GMAT resolves relative paths in scripts (e.g. '../samples/SupportFiles/x.txt')
        # against its bin folder, because the GUI runs from there. Do the same.
        from .gmat_env import gmat_root
        base = gmat_root() / "bin"
        workdir = workdir or tempfile.mkdtemp(prefix="altavista_")
        script, files = prepare_script(text, self.frame, workdir, spacecraft, keep_gui=keep_gui)
        script_path = os.path.join(workdir, "altavista_run.script")
        with open(script_path, "w") as fh:
            fh.write(script)
        cwd = os.getcwd()
        try:
            os.chdir(base)
            reset_cs_cache()  # LoadScript replaces GMAT's configuration; cached CS handles go stale
            self._cc = None
            if not g.LoadScript(script_path):
                raise RuntimeError(f"GMAT failed to load the script.\nPrepared script: {script_path}\n"
                                   f"GMAT log tail:\n{_log_tail()}")
            if not g.RunScript():
                raise RuntimeError(f"GMAT run failed.\nPrepared script: {script_path}\n"
                                   f"GMAT log tail:\n{_log_tail()}")
        finally:
            os.chdir(cwd)
        for sat, path in files.items():
            if not os.path.exists(path):
                raise RuntimeError(f"no report written for {sat}; is it propagated in the script?")
            tr = parse_report(path)
            tr.name = sat
            tr.color = (colors or {}).get(sat) or DEFAULT_COLORS[len(self.spacecraft_list) % len(DEFAULT_COLORS)]
            if max_points:
                decimate(tr, max_points)
            obj = g.GetObject(sat) if g.Exists(sat) else None
            sc = Spacecraft(self, obj, color=tr.color) if obj is not None else None
            if sc is None:
                sc = Spacecraft.__new__(Spacecraft)
                sc.scenario, sc.obj, sc.name = self, None, sat
                sc.epoch = sc.state = None
                sc.central_body = self.frame.origin
            sc.trajectory = tr
            if tr.t:
                sc.epoch = tr.t[-1]
            self.spacecraft_list.append(sc)
        self.events += self._maneuver_events()
        self.meta["script"] = script_path

    def _maneuver_events(self) -> List[Event]:
        """Read maneuver epochs from GMAT's command summaries after a script run."""
        g = self.gmat
        events: List[Event] = []
        try:
            node = g.Moderator.Instance().GetFirstCommand()
        except Exception:
            return events

        def visit(node, parent=None):
            while node is not None:
                tn = node.GetTypeName()
                if tn in ("Maneuver", "BeginFiniteBurn", "EndFiniteBurn"):
                    ev = self._event_from_summary(node, tn)
                    if ev:
                        events.append(ev)
                if node.IsOfType("BranchCommand"):
                    try:
                        visit(node.GetChildCommand(), node)
                    except Exception:
                        pass
                nxt = node.GetNext()
                if parent is not None and nxt is not None and nxt.GetTypeName() == parent.GetTypeName() \
                        and nxt.GetName() == parent.GetName():
                    return
                node = nxt

        try:
            visit(node)
        except Exception:
            pass
        return events

    @staticmethod
    def _event_from_summary(node, type_name: str) -> Optional[Event]:
        try:
            summary = node.GetField("Summary")
        except Exception:
            return None
        m = re.search(r"TAI Epoch:\s+.+?\s+([0-9]+\.[0-9]+)", summary)
        sat = re.search(r"Spacecraft\s*:\s*(\S+)", summary)
        if not m:
            return None
        t = tai_to_a1_mjd(float(m.group(1)))
        name = node.GetName() or type_name
        detail = None
        dv = re.search(r"Delta V Vector.*?\n(.*?)\n\n", summary, re.S)
        if dv:
            detail = " ".join(dv.group(1).split())
        return Event(name=name, t=t, type="maneuver", spacecraft=sat.group(1) if sat else None, detail=detail)

    # ---------------------------------------------------------------- frames (M4.1)
    def _origin_track_for(self, entity_name: str, parent_origin: str, parent_axes: str) -> Dict[str, list]:
        """``entity_name``'s recorded trajectory (sampled in ``self.frame`` by
        :meth:`propagate`/:meth:`run_script`), reexpressed in the ``(parent_origin,
        parent_axes)`` frame via GMAT's ``CoordinateConverter`` -- the same
        ``coordinate_system()``/``self._converter()`` machinery :meth:`_to_frame`
        already uses, not a second converter. Used to give an entity-relative
        ``FrameDefinition``'s viewer node its motion (``FrameNode.setOriginTrack``,
        ``web/js/frames.js``): per question 76's fill rule, an entity-relative frame's
        parent is exactly ``reference_body``'s MJ2000Eq frame, so this is what that
        frame's own ``Group.position`` needs to track over time.

        Returns ``{"t": [...], "pos": [flat xyz...], "vel": [flat xyz...]}`` (A1MJD /
        km / km-s, matching :meth:`Trajectory.to_dict`'s flat layout) -- empty lists if
        the entity has no recorded trajectory (unpropagated, or an unknown name; never
        raises, since a caller declaring a frame before propagating is legitimate and
        the viewer simply gets no motion for that frame's node until data exists).
        """
        try:
            traj = self.get(entity_name).trajectory
        except KeyError:
            return {"t": [], "pos": [], "vel": []}
        if not traj.t:
            return {"t": [], "pos": [], "vel": []}
        if self.frame.origin == parent_origin and self.frame.axes == parent_axes:
            # Already in the target frame -- traj.pos/vel were converted into
            # self.frame by _to_frame() when recorded; no further conversion needed.
            return {"t": list(traj.t), "pos": [c for p in traj.pos for c in p],
                    "vel": [c for v in traj.vel for c in v]}
        g = self.gmat
        src = coordinate_system(self.frame)
        dst = coordinate_system(Frame(f"{parent_origin}{parent_axes}", parent_origin, parent_axes))
        t_out: List[float] = []
        pos_out: List[float] = []
        vel_out: List[float] = []
        for t, p, v in zip(traj.t, traj.pos, traj.vel):
            s_in = g.Rvector6(*[float(x) for x in p], *[float(x) for x in v])
            s_out = g.Rvector6()
            self._converter().Convert(g.A1Mjd(t), s_in, src, s_out, dst)
            t_out.append(t)
            pos_out += [s_out[0], s_out[1], s_out[2]]
            vel_out += [s_out[3], s_out[4], s_out[5]]
        return {"t": t_out, "pos": pos_out, "vel": vel_out}

    def _build_frames(self, central_body: str) -> List[dict]:
        """Build the additive ``frames`` list for ``ScenarioData.to_dict()`` (M4.1,
        docs/open-questions.md question 78): the scenario's own frame, ``central_body``'s
        MJ2000Eq and BodyFixed frames, and every entity-relative RIC/VNB/VVLH frame
        declared via :meth:`frame_ric`/:meth:`frame_vnb`/:meth:`frame_vvlh` -- all
        produced through :class:`altavista.frames.FrameRegistry` (so ``parent_frame_id``
        is filled per question 76, never hand-built) and transcoded with
        ``google.protobuf.json_format.MessageToDict`` (the same protobuf-JSON
        transcoding ``altavista/server.py``'s CDM ingest path already uses).

        A fresh :class:`~altavista.frames.FrameRegistry` is built on every call, against
        the *current* GMAT configuration -- like :class:`altavista.bodies.BodySampler` in
        :meth:`build`, never cached on ``self``, since a script run (``LoadScript``)
        wipes GMAT's configuration and would leave a cached registry's
        ``CoordinateSystem`` handles stale (the environment note this whole module
        already works around, e.g. ``reset_cs_cache()`` in :meth:`run_script`).

        Local imports (``altavista.frames``, ``altavista.cdm``, ``altavista.pb.core_pb2``,
        ``google.protobuf.json_format``) match :meth:`build_cdm`'s existing pattern:
        keep these dependencies optional at *import* time for callers that never
        declare a frame or publish a CDM bundle.
        """
        from google.protobuf import json_format

        from . import frames as frames_mod
        from .cdm import UnmappedAxesError, frame_definition_for
        from .pb import core_pb2

        registry = frames_mod.FrameRegistry()
        emitted: List[core_pb2.FrameDefinition] = []
        by_key: Dict[Tuple[int, str], str] = {}  # (AxesKind, body) -> already-registered id

        def register_body_axes(frame_id: str, axes: int, body: str) -> str:
            key = (axes, body)
            if key in by_key:
                return by_key[key]
            fd = registry.register(core_pb2.FrameDefinition(id=frame_id, axes=axes, body=body))
            emitted.append(fd)
            by_key[key] = fd.id
            return fd.id

        # 1. The scenario's own frame. Not every axes altavista's parse_frame() accepts
        # has an altavista.v1.AxesKind counterpart (e.g. "BodyInertial", GMAT's
        # Topocentric) -- frame_definition_for() raises UnmappedAxesError for those.
        # This is a real, known gap, not papered over: the scenario frame is simply
        # omitted from `frames` in that case (web/js/scene.js documents and logs the
        # resulting fallback), while the central-body and declared entity-relative
        # frames below are still emitted -- they never depend on the scenario frame's
        # own axes being CDM-mappable.
        try:
            scenario_fd_in = frame_definition_for(self.frame, frame_id=self.frame.name)
        except UnmappedAxesError:
            scenario_fd_in = None
        if scenario_fd_in is not None:
            key = (scenario_fd_in.axes, scenario_fd_in.body)
            if key not in by_key:
                fd = registry.register(scenario_fd_in)
                emitted.append(fd)
                by_key[key] = fd.id

        # 2. central_body's MJ2000Eq and BodyFixed frames (question 78's explicit
        # requirement). Deduped against the scenario frame above when they coincide --
        # the common case (frame="EarthMJ2000Eq" scenarios) -- rather than emitting a
        # second FrameDefinition describing the exact same origin+axes.
        register_body_axes(f"{central_body}MJ2000Eq", core_pb2.AXES_KIND_MJ2000_EQ, central_body)
        register_body_axes(f"{central_body}BodyFixed", core_pb2.AXES_KIND_BODY_FIXED, central_body)

        # 3. Declared entity-relative frames. Each one's reference_body needs a
        # registered MJ2000Eq frame as its parent (question 76); register_body_axes()
        # reuses one if it already matches (e.g. central_body's, from step 2) or
        # registers a new one -- so a RIC frame about a body other than central_body
        # (a lunar-flyby scenario, say) still gets a correctly deduped parent.
        axes_by_kind = {"ric": core_pb2.AXES_KIND_RIC, "vnb": core_pb2.AXES_KIND_VNB,
                        "vvlh": core_pb2.AXES_KIND_VVLH}
        for decl in self._entity_frame_decls:
            register_body_axes(f"{decl['reference_body']}MJ2000Eq", core_pb2.AXES_KIND_MJ2000_EQ,
                               decl["reference_body"])
            fd = registry.register(core_pb2.FrameDefinition(
                id=decl["id"], axes=axes_by_kind[decl["kind"]], entity_id=decl["entity_id"],
                reference_entity_id=decl["reference_entity_id"], reference_body=decl["reference_body"]))
            emitted.append(fd)

        # Transcode, and attach the altavista-only `originTrack` wire extension (see
        # altavista/model.py's ScenarioData docstring) for every entity-relative frame:
        # its origin entity's own trajectory, reexpressed in the frame's parent.
        out: List[dict] = []
        for fd in emitted:
            d = json_format.MessageToDict(fd)
            if fd.axes in (core_pb2.AXES_KIND_RIC, core_pb2.AXES_KIND_VNB, core_pb2.AXES_KIND_VVLH):
                track = self._origin_track_for(fd.entity_id, fd.reference_body, "MJ2000Eq")
                if track["t"]:
                    d["originTrack"] = track
            out.append(d)
        return out

    # ---------------------------------------------------------------- state spaces (M7.1)
    def _build_state_spaces(self) -> List[dict]:
        """Build the additive ``stateSpaces`` list for ``ScenarioData.to_dict()`` (M7.1,
        docs/open-questions.md question 88 / docs/adr/005-simulation-kernel.md sec 3): a
        declared ``altavista.v1.StateSpace`` (component labels and units) for every
        state-space id actually used by this scenario's recorded spacecraft trajectories
        -- the id a :class:`~altavista.model.Trajectory`'s own ``to_dict()["stateSpaceId"]``
        names always resolves to an entry here, never a naked string with nothing behind
        it (ADR-001: "nothing exists only by convention").

        A trajectory with a populated, parallel ``attitude`` stream contributes the
        10-component attitude id; every other recorded trajectory contributes the plain
        6-component id -- the same rule :meth:`altavista.model.Trajectory.to_dict` and
        :func:`altavista.cdm.trajectory_to_cdm`'s upgrade both use, so this list, a
        trajectory's own declared id, and the CDM adapter's ``state_space_id`` never
        disagree about which shape a given trajectory is. Sorted by id (determinism).

        Local imports match :meth:`_build_frames`'s existing pattern: keep ``altavista.cdm``
        (and its protobuf dependency) optional at plain :meth:`build` import time.
        """
        from google.protobuf import json_format

        from .cdm import state_space_for
        from .model import STATE_SPACE_ID_CARTESIAN_POS_VEL_6, STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4

        ids = set()
        for sc in self.spacecraft_list:
            traj = sc.trajectory
            if not traj.t:
                continue
            has_attitude = bool(traj.attitude) and len(traj.attitude) == len(traj.t)
            ids.add(STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4 if has_attitude
                    else STATE_SPACE_ID_CARTESIAN_POS_VEL_6)
        return [json_format.MessageToDict(state_space_for(sid)) for sid in sorted(ids)]

    # ---------------------------------------------------------------- sensor footprint (M6.3)
    def footprint(self, spacecraft: Spacecraft, half_angle_deg: float, axis: Sequence[float] = (1.0, 0.0, 0.0),
                 body: Optional[str] = None, n_points: int = 24, name: Optional[str] = None,
                 color: Optional[str] = None) -> str:
        """Declare a sensor footprint: a cone of half-angle ``half_angle_deg`` about
        body-frame ``axis`` (unit vector; default ``+X``, matching GMAT
        ``NadirPointing``'s own default ``BodyAlignmentVector`` -- verified empirically,
        see ``altavista/FRAMES.md``), intersected with ``body``'s ellipsoid (default this
        scenario's own central body, ``self.frame.origin``; equatorial radius and
        flattening read live from GMAT's own body model, the same
        ``SolarSystem.GetBody`` source :class:`altavista.bodies.BodySampler` uses for
        ``BodyTrack.radius``/``flattening``).

        Computed lazily at :meth:`build` time (the same "declare now, compute once the
        run is final" pattern as :meth:`frame_ric`/:meth:`frame_vnb`/:meth:`frame_vvlh`),
        from ``spacecraft``'s already-recorded trajectory and attitude samples -- never
        by re-querying GMAT's (possibly stateful, e.g. ``Spinner``) attitude object for
        a past epoch a second time. Raises ``ValueError`` at :meth:`build` time if
        ``spacecraft`` has no recorded attitude stream (``Trajectory.attitude`` empty or
        not parallel to ``t``): a cone direction needs a real attitude, not an invented
        one. Double precision throughout (Python ``float``/``numpy`` ``float64``; no
        ``float32`` anywhere in this path).

        Returns the footprint's ``name`` (``f"{spacecraft.name}_footprint"`` if not
        given), the ``name`` field of the matching entry in
        ``ScenarioData.to_dict()["footprints"]``.
        """
        fid = name or f"{spacecraft.name}_footprint"
        self._footprint_decls.append({
            "spacecraft": spacecraft, "half_angle_deg": float(half_angle_deg), "axis": list(axis),
            "body": body, "n_points": int(n_points), "name": fid, "color": color or "#00e5ff",
        })
        return fid

    def _build_footprints(self) -> List[Footprint]:
        out: List[Footprint] = []
        if not self._footprint_decls:
            return out
        g = self.gmat
        ss = g.GetSolarSystem()
        for decl in self._footprint_decls:
            sc: Spacecraft = decl["spacecraft"]
            traj = sc.trajectory
            if not traj.attitude or len(traj.attitude) != len(traj.t):
                raise ValueError(
                    f"footprint({sc.name!r}, ...) needs an attitude stream recorded for "
                    f"{sc.name!r} (Trajectory.attitude parallel to t, {len(traj.attitude)} "
                    f"vs {len(traj.t)} samples); configure a GMAT attitude model "
                    f"(NadirPointing, CoordinateSystemFixed, Spinner, or a CCSDS AEM file) "
                    f"on this spacecraft with set_field('Attitude', ...) before propagate()")
            body = decl["body"] or self.frame.origin
            b_obj = ss.GetBody(body)
            a_km = float(b_obj.GetEquatorialRadius())
            flat = float(b_obj.GetFlattening())
            b_km = a_km * (1.0 - flat)
            body_fixed = Frame(f"{body}Fixed", body, "BodyFixed")
            axis = np.asarray(decl["axis"], dtype=float)
            axis = axis / np.linalg.norm(axis)
            half_angle = math.radians(decl["half_angle_deg"])
            rays_body_local = _cone_ray_directions(axis, half_angle, decl["n_points"])
            fp = Footprint(name=decl["name"], spacecraft=sc.name, half_angle_deg=decl["half_angle_deg"],
                          axis=axis.tolist(), color=decl["color"])
            for i, t in enumerate(traj.t):
                q = traj.attitude[i]
                pos_scenario = np.asarray(traj.pos[i], dtype=float)
                R_bf = np.asarray(self._rotation_matrix_between(self.frame, body_fixed, t))  # v_bf = R_bf @ v_scenario
                pos_bf = R_bf @ pos_scenario
                boresight_bf = R_bf @ _quat_apply(q, axis)
                boresight_bf = boresight_bf / np.linalg.norm(boresight_bf)
                center_bf = _ray_ellipsoid_intersect(pos_bf, boresight_bf, a_km, b_km)
                fp.t.append(t)
                fp.center.append((R_bf.T @ center_bf).tolist() if center_bf is not None else None)
                ring_flat: List[float] = []
                for ray_local in rays_body_local:
                    ray_bf = R_bf @ _quat_apply(q, ray_local)
                    ray_bf = ray_bf / np.linalg.norm(ray_bf)
                    hit_bf = _ray_ellipsoid_intersect(pos_bf, ray_bf, a_km, b_km)
                    if hit_bf is not None:
                        ring_flat.extend((R_bf.T @ hit_bf).tolist())
                fp.ring.append(ring_flat)
            out.append(fp)
        return out

    # ---------------------------------------------------------------- output
    def default_bodies(self) -> List[str]:
        names = [self.frame.origin]
        for s in self.spacecraft_list:
            if s.central_body not in names and s.central_body in ALL_BODIES:
                names.append(s.central_body)
        if "Sun" not in names:
            names.append("Sun")
        if "Earth" in names and "Luna" not in names:
            names.append("Luna")
        return names

    def build(self, body_samples: int = 2000) -> ScenarioData:
        data = ScenarioData(name=self.name, frame=self.frame, events=list(self.events), meta=dict(self.meta))
        data.spacecraft = [s.trajectory for s in self.spacecraft_list if s.trajectory.t]
        span = data.span()
        if span:
            data.t0_iso = a1_to_iso(span[0])
            data.t1_iso = a1_to_iso(span[1])
            times = sample_times(span[0], span[1], max_samples=body_samples)
        else:
            times = []
        sampler = BodySampler(self.frame)
        for b in (self.bodies_override or self.default_bodies()):
            data.bodies.append(sampler.track(b, times, central=(b == self.frame.origin)))
        data.frames = self._build_frames(central_body=self.frame.origin)
        data.footprints = self._build_footprints()
        data.state_spaces = self._build_state_spaces()
        return data

    def build_cdm(self, body_samples: int = 2000, **cdm_kwargs):
        """Opt-in CDM v1 bundle for this scenario (M1.3, ``altavista.cdm``).

        Builds the same :class:`~altavista.model.ScenarioData` :meth:`build` would (does not
        change its return value or signature) and converts it with
        :func:`altavista.cdm.scenario_to_cdm`. ``cdm_kwargs`` are forwarded to that function
        (``state_space_id``, ``tool``, ``principal``, ``created_tai_ns``, ``run_id``).
        """
        from .cdm import scenario_to_cdm  # local import: keeps `cdm` optional at import time
        return scenario_to_cdm(self.build(body_samples=body_samples), **cdm_kwargs)

    def to_dict(self) -> dict:
        return self.build().to_dict()

    def save(self, path: Union[str, os.PathLike]) -> None:
        Path(path).write_text(self.build().to_json())

    def publish(self, url: Optional[str] = None) -> dict:
        """Send the scenario to the viewer server; all connected browsers update."""
        return client.publish(self.build(), url=url or self.url)

    def get(self, name: str) -> Spacecraft:
        """Look up a spacecraft by name."""
        for s in self.spacecraft_list:
            if s.name == name:
                return s
        raise KeyError(name)

    def __repr__(self) -> str:
        return f"<Scenario {self.name!r} frame={self.frame.name} spacecraft={[s.name for s in self.spacecraft_list]}>"
