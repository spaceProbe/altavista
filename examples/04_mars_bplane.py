"""GMAT's MAVEN-style Mars B-plane targeting sample, viewed in a Sun-centred ecliptic frame.

Long interplanetary runs produce many report rows; ``max_points`` decimates them for the
browser while GMAT's Hermite-friendly velocities keep the drawn curve smooth.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import altavista as gv

script = gv.gmat_root() / "samples" / "Ex_MarsBPlane.script"
sc = gv.Scenario.from_script(script, name="Mars B-plane", frame="SunMJ2000Ec",
                             bodies=["Sun", "Earth", "Mars", "Venus"], max_points=6000)
print(sc, sc.events)
print(sc.publish())
