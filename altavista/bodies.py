"""Celestial-body sampling: positions and orientations expressed in the scenario frame.

Positions come from GMAT's solar system (DE ephemeris) via ``GetMJ2000Position``
(Earth-centred J2000 equatorial), then are converted to the requested frame with
GMAT's ``CoordinateConverter`` so origin shifts and axis rotations match GMAT exactly.

Orientation of each body is sampled as the rotation from its ``BodyFixed`` frame to
the scenario frame (unit quaternion). Between samples the browser spins the body
about its pole at ``spin_rate`` deg/day, so coarse sampling stays accurate.
"""
from __future__ import annotations

import math
from typing import Dict, Iterable, List, Optional, Sequence

from .gmat_env import gmat
from .model import BodyTrack, Frame

# Textures shipped in GMAT's data/graphics/texture folder.
TEXTURES: Dict[str, str] = {
    "Sun": "Sun.jpg",
    "Mercury": "Mercury_JPLCaltech.jpg",
    "Venus": "Venus_BjornJonsson.jpg",
    "Earth": "ModifiedBlueMarble.jpg",
    "Luna": "Moon_HermesCelestiaMotherlode.jpg",
    "Mars": "Mars_JPLCaltechUSGS.jpg",
    "Jupiter": "Jupiter_HermesCelestiaMotherlode.jpg",
    "Saturn": "Saturn_gradiusCelestiaMotherlode.jpg",
    "Uranus": "Uranus_JPLCaltech.jpg",
    "Neptune": "Neptune_BjornJonsson.jpg",
    "Pluto": "Pluto_JPLCaltech.jpg",
}

COLORS: Dict[str, str] = {
    "Sun": "#ffd166", "Mercury": "#9e9e9e", "Venus": "#e6c27a", "Earth": "#3d7bd9",
    "Luna": "#bdbdbd", "Mars": "#d1603d", "Jupiter": "#d9b58c", "Saturn": "#e8d8a8",
    "Uranus": "#8fd3e8", "Neptune": "#4f74e3", "Pluto": "#c9b7a6",
}

ALL_BODIES = list(TEXTURES.keys())

_cs_cache: Dict[str, object] = {}


def coordinate_system(frame: Frame):
    """Return (constructing if needed) a GMAT CoordinateSystem object for ``frame``."""
    g = gmat()
    key = frame.name
    if key in _cs_cache:
        return _cs_cache[key]
    if g.Exists(frame.name):
        cs = g.GetObject(frame.name)
    else:
        cs = g.Construct("CoordinateSystem", frame.name, frame.origin, frame.axes)
        g.Initialize()  # a freshly constructed CS must be wired to the solar system before use
    _cs_cache[key] = cs
    return cs


def reset_cache() -> None:
    """Forget cached CoordinateSystem handles (call after GMAT's configuration is cleared)."""
    _cs_cache.clear()


def body_fixed_system(body: str):
    return coordinate_system(Frame(f"altavista_{body}Fixed", body, "BodyFixed"))


def earth_mj2000eq():
    return coordinate_system(Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"))


def _mat_to_quat(m: Sequence[Sequence[float]]) -> List[float]:
    """3x3 rotation matrix (row-major) -> quaternion [x, y, z, w]."""
    tr = m[0][0] + m[1][1] + m[2][2]
    if tr > 0:
        s = math.sqrt(tr + 1.0) * 2
        w = 0.25 * s
        x = (m[2][1] - m[1][2]) / s
        y = (m[0][2] - m[2][0]) / s
        z = (m[1][0] - m[0][1]) / s
    elif m[0][0] > m[1][1] and m[0][0] > m[2][2]:
        s = math.sqrt(1.0 + m[0][0] - m[1][1] - m[2][2]) * 2
        w = (m[2][1] - m[1][2]) / s
        x = 0.25 * s
        y = (m[0][1] + m[1][0]) / s
        z = (m[0][2] + m[2][0]) / s
    elif m[1][1] > m[2][2]:
        s = math.sqrt(1.0 + m[1][1] - m[0][0] - m[2][2]) * 2
        w = (m[0][2] - m[2][0]) / s
        x = (m[0][1] + m[1][0]) / s
        y = 0.25 * s
        z = (m[1][2] + m[2][1]) / s
    else:
        s = math.sqrt(1.0 + m[2][2] - m[0][0] - m[1][1]) * 2
        w = (m[1][0] - m[0][1]) / s
        x = (m[0][2] + m[2][0]) / s
        y = (m[1][2] + m[2][1]) / s
        z = 0.25 * s
    n = math.sqrt(x * x + y * y + z * z + w * w)
    return [x / n, y / n, z / n, w / n]


class BodySampler:
    """Samples body positions/orientations in ``frame`` at a set of A1MJD epochs."""

    def __init__(self, frame: Frame):
        self.g = gmat()
        self.frame = frame
        self.frame_cs = coordinate_system(frame)
        self.eq_cs = earth_mj2000eq()
        self.g.Initialize()
        self.ss = self.g.GetSolarSystem()
        self.cc = self.g.CoordinateConverter()

    def position(self, body: str, t: float) -> List[float]:
        """Position (km) of ``body`` in the scenario frame at A1MJD ``t``."""
        b = self.ss.GetBody(body)
        p = b.GetMJ2000Position(t)
        state_in = self.g.Rvector6(p[0], p[1], p[2], 0.0, 0.0, 0.0)
        state_out = self.g.Rvector6()
        self.cc.Convert(self.g.A1Mjd(t), state_in, self.eq_cs, state_out, self.frame_cs)
        return [state_out[0], state_out[1], state_out[2]]

    def orientation(self, body: str, t: float) -> List[float]:
        """Quaternion rotating body-fixed axes into the scenario frame at ``t``."""
        fixed = body_fixed_system(body)
        state_in = self.g.Rvector6(1.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        state_out = self.g.Rvector6()
        self.cc.Convert(self.g.A1Mjd(t), state_in, fixed, state_out, self.frame_cs)
        R = self.cc.GetLastRotationMatrix()
        m = [[R.GetElement(i, j) for j in range(3)] for i in range(3)]
        return _mat_to_quat(m)

    def spin_rate(self, body: str, t: float, dt_days: float = 1.0 / 24.0) -> float:
        """Mean rotation rate about the pole in deg/day, from the body's hour angle."""
        b = self.ss.GetBody(body)
        try:
            h0 = b.GetHourAngle(self.g.A1Mjd(t))
            h1 = b.GetHourAngle(self.g.A1Mjd(t + dt_days))
        except Exception:
            return 0.0
        d = (h1 - h0) % 360.0
        if d > 180.0:
            d -= 360.0
        return d / dt_days

    def track(self, body: str, times: Iterable[float], central: bool = False,
              texture_url: Optional[str] = None) -> BodyTrack:
        times = list(times)
        b = self.ss.GetBody(body)
        tr = BodyTrack(
            name=body,
            radius=float(b.GetEquatorialRadius()),
            flattening=float(b.GetFlattening()),
            texture=texture_url if texture_url is not None else (f"/textures/{TEXTURES[body]}" if body in TEXTURES else None),
            color=COLORS.get(body, "#888888"),
            central=central,
        )
        if not times:
            return tr
        for t in times:
            tr.t.append(float(t))
            tr.pos.append(self.position(body, t))
            tr.quat.append(self.orientation(body, t))
        tr.spin_rate = self.spin_rate(body, times[0])
        return tr


def sample_times(t0: float, t1: float, max_samples: int = 2000, min_samples: int = 2,
                 max_step_days: float = 0.25) -> List[float]:
    """Evenly spaced A1MJD samples across [t0, t1] (inclusive)."""
    span = max(t1 - t0, 0.0)
    n = int(math.ceil(span / max_step_days)) + 1
    n = max(min_samples, min(max_samples, n))
    if span == 0.0:
        return [t0]
    return [t0 + span * i / (n - 1) for i in range(n)]
