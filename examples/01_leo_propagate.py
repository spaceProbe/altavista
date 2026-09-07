"""Step-propagate two Earth orbiters with the GMAT object API and publish them.

Run the viewer first:   python -m altavista serve
Then:                   python examples/01_leo_propagate.py

ISS carries a GMAT ``NadirPointing`` attitude model (M6.3): the viewer's per-entity
body frame uses the resulting real attitude stream instead of falling back to a
derived nadir-pointing VVLH orientation (``web/js/scene.js``'s fallback label
disappears for ISS specifically). A 10-degree-half-angle sensor footprint -- a cone
about the body's +X axis (GMAT's own default ``NadirPointing`` body-alignment axis;
see ``altavista/FRAMES.md``) intersected with Earth's WGS84-class ellipsoid -- is
declared on ISS and drawn on the globe.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import altavista as gv

sc = gv.Scenario("LEO demo", frame="EarthMJ2000Eq")

iss = sc.spacecraft(
    "ISS", epoch="01 Jan 2026 00:00:00.000",
    keplerian=dict(SMA=6778.0, ECC=0.0005, INC=51.64, RAAN=120.0, AOP=30.0, TA=0.0),
    DryMass=420000.0, DragArea=1500.0, Cd=2.2, color="#ff9f43",
    Attitude="NadirPointing",
)
sso = sc.spacecraft(
    "SunSync", epoch="01 Jan 2026 00:00:00.000",
    keplerian=dict(SMA=7078.0, ECC=0.001, INC=98.2, RAAN=200.0, AOP=90.0, TA=45.0),
    color="#1dd1a1",
)

# 8x8 Earth gravity + Sun/Moon point masses (default). Sample every 60 s for 3 hours.
sc.propagate([iss, sso], hours=3, step=60)

# Raise the ISS orbit with a 20 m/s prograde burn, then keep going for another orbit.
sc.maneuver(iss, dv=[0.020, 0, 0], frame="VNB", name="Reboost")
sc.propagate([iss, sso], hours=1.6, step=60)

# The first sensor footprint (M6.3): a 10-degree half-angle cone about ISS's +X body
# axis (the same axis GMAT's NadirPointing aligns with nadir by default), intersected
# with Earth's ellipsoid at every recorded attitude sample.
sc.footprint(iss, half_angle_deg=10.0, axis=(1.0, 0.0, 0.0), n_points=32, color="#00e5ff")

print(sc)
print("ISS Keplerian after run:", [round(x, 3) for x in iss.keplerian()])
print("ISS attitude samples:", len(iss.trajectory.attitude), "/", len(iss.trajectory.t))
print(sc.publish())
