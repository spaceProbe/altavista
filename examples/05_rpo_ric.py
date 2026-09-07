"""Two-spacecraft RPO (rendezvous/proximity operations) scene: a target and a chaser
a few tens of metres apart in LEO, with the target's RIC (radial/in-track/cross-track)
frame declared for the viewer's frame graph (M4.1, docs/open-questions.md questions 10
and 78).

Run the viewer first:   python -m altavista serve
Then:                   python examples/05_rpo_ric.py

In the browser: pick "Target RIC" in the sidebar's "Frames" -> "View frame" selector,
then "Chaser" in "Focus" -- that is "focus the chaser in the target's RIC frame":
the camera is re-parented into the target's declared RIC frame (a small, precise,
translating-with-the-target view) instead of the whole-orbit-scale entities frame, so
the ~30 m separation renders at the precision docs/open-questions.md Q46 requires for
RPO (centimetre bound; see tests/test_viewer_jitter.py's RPO checks).
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import altavista as gv

SEPARATION_KM = 0.030  # 30 m -- "a few tens of metres apart"

sc = gv.Scenario("RPO demo", frame="EarthMJ2000Eq")

target = sc.spacecraft(
    "Target", epoch="01 Jan 2026 00:00:00.000",
    keplerian=dict(SMA=6878.0, ECC=0.0005, INC=51.6, RAAN=45.0, AOP=0.0, TA=0.0),
    DryMass=500.0, color="#54a0ff",
)

# Seed the chaser a small in-track offset from the target's own cartesian state, with
# the target's velocity carried over unchanged (not the chaser's own natural orbital
# velocity at that offset point) -- a standard way to seed a small relative-motion
# demo: the two spacecraft then drift apart/together over the orbit under their own
# (very slightly different) two-body dynamics, which is exactly what an RIC/RPO view
# is for watching.
tx, ty, tz, tvx, tvy, tvz = target.cartesian()
speed = (tvx ** 2 + tvy ** 2 + tvz ** 2) ** 0.5
ux, uy, uz = tvx / speed, tvy / speed, tvz / speed  # unit in-track direction
chaser = sc.spacecraft(
    "Chaser", epoch="01 Jan 2026 00:00:00.000",
    cartesian=[tx + SEPARATION_KM * ux, ty + SEPARATION_KM * uy, tz + SEPARATION_KM * uz, tvx, tvy, tvz],
    DryMass=450.0, color="#ff6b6b",
)

# ~1.5 orbits (LEO period ~93 min), sampled finely enough to resolve the metre-scale
# relative motion. Scenario.propagate() takes days/hours/seconds only.
sc.propagate([target, chaser], hours=2.33, step=15)

# Declare the target's RIC frame for the viewer's frame graph (goes through
# altavista.frames.FrameRegistry at build()/publish() time -- never hand-built here).
ric_id = sc.frame_ric(target)

print(sc)
print(f"declared frame: {ric_id!r} (target's RIC)")
print(sc.publish())
