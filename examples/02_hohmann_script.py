"""Run one of GMAT's sample scripts (a targeted Hohmann transfer to GEO) headless and publish it.

The script's OpenFrames view is stripped automatically; the converged trajectory and
the two maneuvers found by the differential corrector are captured.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import altavista as gv

script = gv.gmat_root() / "samples" / "Ex_HohmannTransfer.script"
sc = gv.Scenario.from_script(script, name="Hohmann transfer", frame="EarthMJ2000Eq")
for s in sc.spacecraft_list:
    print(s)
for ev in sc.events:
    print("event:", ev.name, ev.t, ev.detail)
print(sc.publish())
