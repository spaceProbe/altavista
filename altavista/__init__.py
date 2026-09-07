"""altavista - build GMAT scenarios in Python, view them in a shared Three.js browser viewer.

Quick start::

    # terminal 1
    python -m altavista serve

    # python session
    import altavista as gv
    sc = gv.Scenario("LEO demo")
    sat = sc.spacecraft("Sat", epoch="01 Jan 2026 00:00:00.000",
                        keplerian=dict(SMA=7000, ECC=0.01, INC=51.6, RAAN=0, AOP=0, TA=0))
    sc.propagate(sat, days=1)
    sc.publish()          # every open browser updates

Or run an existing GMAT script::

    sc = gv.Scenario.from_script("Ex_HohmannTransfer.script")
    sc.publish()
"""
from .frames import FrameRegistry
from .gmat_env import gmat, gmat_root, load_gmat
from .model import BodyTrack, Event, Frame, ScenarioData, Trajectory
from .scenario import Scenario, Spacecraft
from .client import ViewerError, is_running, publish, remove, set_clock
from .server import create_app, serve

__all__ = [
    "Scenario", "Spacecraft", "ScenarioData", "Trajectory", "BodyTrack", "Event", "Frame",
    "FrameRegistry",
    "gmat", "gmat_root", "load_gmat",
    "publish", "remove", "set_clock", "is_running", "ViewerError",
    "create_app", "serve",
]
__version__ = "0.1.0"
