"""Build a GMAT script from a Python template (a targeted lunar transfer) and publish it.

Shows the "script from Python" workflow: parameters live in Python, GMAT's differential
corrector does the work, and the viewer shows Earth, Moon and the converged transfer.
Initial conditions follow GMAT's Ex_LunarTransfer sample.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import altavista as gv

EPOCH = "22 Jul 2014 11:29:10.811"
STATE = dict(X=-137380.1984338506, Y=75679.87867537055, Z=21487.63875187856,
             VX=-0.2324532014235503, VY=-0.4462753967758019, VZ=0.08561205662877103)
BDOTT, BDOTR = 15000.4401777, 4000.59308992
LOI_DV = -0.652          # km/s, lunar orbit insertion (retrograde in VNB)
MOON_DAYS = 2.0          # coast in lunar orbit after LOI

state_lines = "\n".join(f"Sat.{k} = {v};" for k, v in STATE.items())

SCRIPT = f"""
Create Spacecraft Sat;
Sat.DateFormat = UTCGregorian;
Sat.Epoch = '{EPOCH}';
Sat.CoordinateSystem = EarthMJ2000Eq;
Sat.DisplayStateType = Cartesian;
{state_lines}
Sat.DryMass = 1000;

Create ForceModel EarthForces;
EarthForces.CentralBody = Earth;
EarthForces.PrimaryBodies = {{Earth}};
EarthForces.PointMasses = {{Sun, Luna, Jupiter}};
EarthForces.GravityField.Earth.Degree = 8;
EarthForces.GravityField.Earth.Order = 8;
EarthForces.GravityField.Earth.PotentialFile = 'JGM2.cof';

Create ForceModel MoonForces;
MoonForces.CentralBody = Luna;
MoonForces.PrimaryBodies = {{Luna}};
MoonForces.PointMasses = {{Sun, Earth}};
MoonForces.GravityField.Luna.Degree = 8;
MoonForces.GravityField.Luna.Order = 8;
MoonForces.GravityField.Luna.PotentialFile = 'LP165P.cof';

Create Propagator EarthProp;
EarthProp.FM = EarthForces;
EarthProp.Type = RungeKutta89;
EarthProp.InitialStepSize = 60;
EarthProp.Accuracy = 1e-11;
EarthProp.MinStep = 0.001;
EarthProp.MaxStep = 3600;

Create Propagator MoonProp;
MoonProp.FM = MoonForces;
MoonProp.Type = RungeKutta89;
MoonProp.InitialStepSize = 60;
MoonProp.Accuracy = 1e-11;
MoonProp.MinStep = 0.001;
MoonProp.MaxStep = 600;

Create ImpulsiveBurn TOI;
TOI.CoordinateSystem = Local;
TOI.Origin = Earth;
TOI.Axes = VNB;

Create ImpulsiveBurn LOI;
LOI.CoordinateSystem = Local;
LOI.Origin = Luna;
LOI.Axes = VNB;
LOI.Element1 = {LOI_DV};

Create CoordinateSystem MoonMJ2000Eq;
MoonMJ2000Eq.Origin = Luna;
MoonMJ2000Eq.Axes = MJ2000Eq;

Create DifferentialCorrector DC;
DC.MaximumIterations = 50;

BeginMissionSequence;
Propagate 'Prop to Perigee' EarthProp(Sat) {{Sat.Periapsis}};
Target 'Target B-plane' DC {{SolveMode = Solve, ExitMode = DiscardAndContinue}};
   Vary DC(TOI.Element1 = 0.14, {{Perturbation = 1e-5, Lower = 0.13, Upper = 0.5, MaxStep = 0.01}});
   Vary DC(TOI.Element3 = 0.1, {{Perturbation = 1e-5, Lower = -0.5, Upper = 0.5, MaxStep = 0.01}});
   Maneuver 'Apply TOI' TOI(Sat);
   Propagate 'Prop to Moon SOI' EarthProp(Sat) {{Sat.Earth.RMAG = 325000, StopTolerance = 1e-5}};
   Propagate 'Prop to Periselene' MoonProp(Sat) {{Sat.Luna.Periapsis, StopTolerance = 1e-5}};
   Achieve DC(Sat.MoonMJ2000Eq.BdotT = {BDOTT}, {{Tolerance = 3}});
   Achieve DC(Sat.MoonMJ2000Eq.BdotR = {BDOTR}, {{Tolerance = 3}});
EndTarget;
Maneuver 'Apply LOI' LOI(Sat);
Propagate 'Lunar orbit' MoonProp(Sat) {{Sat.ElapsedDays = {MOON_DAYS}}};
"""

sc = gv.Scenario.from_script_text(SCRIPT, name="Lunar transfer", frame="EarthMJ2000Eq",
                                  bodies=["Earth", "Luna", "Sun"])
print(sc)
for ev in sc.events:
    print("event:", ev.name, ev.detail)
print(sc.publish())
