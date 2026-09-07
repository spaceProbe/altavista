"""Minimal reproduction: GMAT R2026a converts OrbitErrorCovariance into a rotating frame
(EarthFixed) with the block-diagonal transform [[R,0],[0,R]] and omits the rotation-rate term.

Demonstration: a covariance with position uncertainty only (velocity variance exactly 0) is
reported in EarthFixed with velocity variance exactly 0. Physically, v_fixed = R v + Rdot r,
so position uncertainty sigma_r in a frame rotating at omega must produce velocity
uncertainty of order omega * sigma_r (Earth: 7.29e-5 rad/s * 1 km = 0.0729 m/s).

Usage: python run_repro.py [GMAT_BIN]   (default: the GMAT R2026a bin directory beside this repo)
Requires only GMAT's own Python API (gmatpy). No other dependency.
"""
import os, sys
from pathlib import Path

GMAT_BIN = sys.argv[1] if len(sys.argv) > 1 else "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat  # noqa: E402
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

here = Path(__file__).parent.resolve()
script = here / "repro.script"
state_rpt = here / "repro_state.rpt"
cov_rpt = here / "repro_cov.rpt"

SCRIPT = f"""\
% Minimal reproduction of the OrbitErrorCovariance rotating-frame conversion issue.
Create Spacecraft Sat;
Sat.DateFormat = UTCGregorian;
Sat.Epoch = '01 Jan 2026 00:00:00.000';
Sat.CoordinateSystem = EarthMJ2000Eq;
Sat.DisplayStateType = Cartesian;
Sat.X = 6878.137;
Sat.Y = 0;
Sat.Z = 0;
Sat.VX = 0;
Sat.VY = 5.38;
Sat.VZ = 5.38;

Create ForceModel FM;
FM.CentralBody = Earth;
FM.PrimaryBodies = {{Earth}};
FM.GravityField.Earth.Degree = 0;
FM.GravityField.Earth.Order = 0;
FM.Drag = None;
FM.SRP = Off;
Create Propagator Prop;
Prop.FM = FM;
Prop.Type = RungeKutta89;

Create ReportFile StateReport;
StateReport.Filename = '{state_rpt}';
StateReport.Precision = 16;
StateReport.WriteHeaders = false;
StateReport.Delimiter = ',';
StateReport.SolverIterations = Current;
StateReport.Add = {{Sat.EarthMJ2000Eq.X, Sat.EarthMJ2000Eq.Y, Sat.EarthMJ2000Eq.Z, Sat.EarthMJ2000Eq.VX, Sat.EarthMJ2000Eq.VY, Sat.EarthMJ2000Eq.VZ, Sat.EarthFixed.X, Sat.EarthFixed.Y, Sat.EarthFixed.Z, Sat.EarthFixed.VX, Sat.EarthFixed.VY, Sat.EarthFixed.VZ}};

Create ReportFile CovReport;
CovReport.Filename = '{cov_rpt}';
CovReport.Precision = 16;
CovReport.WriteHeaders = false;
CovReport.Delimiter = ',';
CovReport.SolverIterations = Current;
CovReport.Add = {{Sat.EarthMJ2000Eq.OrbitErrorCovariance, Sat.EarthFixed.OrbitErrorCovariance}};

BeginMissionSequence;
% Position uncertainty of 1 km (1 sigma) per axis, velocity uncertainty exactly zero.
Sat.OrbitErrorCovariance = diag([1 1 1 0 0 0]);
Propagate Prop(Sat) {{Sat.ElapsedSecs = 1}};
"""
script.write_text(SCRIPT)
for p in (state_rpt, cov_rpt):
    if p.exists():
        p.unlink()

cwd = os.getcwd()
try:
    os.chdir(GMAT_BIN)
    if not gmat.LoadScript(str(script)):
        msg = gmat.GetLastMessage() if hasattr(gmat, "GetLastMessage") else ""
        raise SystemExit(f"GMAT failed to load {script}: {msg}")
    if not gmat.RunScript():
        raise SystemExit(f"GMAT failed to run {script}")
finally:
    os.chdir(cwd)

def matrix_rows(path):
    rows = [ln.strip() for ln in path.read_text().splitlines() if ln.strip()]
    last6 = rows[-6:]
    mj, ef = [], []
    for ln in last6:
        vals = [float(x) for x in ln.replace(",", " ").split()]
        if len(vals) != 12:
            raise SystemExit(f"expected 12 values per covariance row (two 6x6 matrices), got {len(vals)}: {ln}")
        mj.append(vals[0:6])
        ef.append(vals[6:12])
    return mj, ef

state = [float(x) for x in state_rpt.read_text().strip().splitlines()[-1].replace(",", " ").split()]
r_in, v_in, r_ef, v_ef = state[0:3], state[3:6], state[6:9], state[9:12]
mj, ef = matrix_rows(cov_rpt)

norm = lambda v: sum(c * c for c in v) ** 0.5
omega_e = 7.2921159e-5  # rad/s

print("GMAT R2026a, epoch 2026-01-01T00:00:01 UTC, spacecraft in LEO")
print(f"|v| inertial (EarthMJ2000Eq) = {norm(v_in):.6f} km/s")
print(f"|v| EarthFixed               = {norm(v_ef):.6f} km/s  (differs by ~|omega x r| = {omega_e*norm(r_in):.6f} km/s: the STATE conversion includes the rotation rate)")
print()
print("Declared covariance (EarthMJ2000Eq), diagonal: ", [mj[i][i] for i in range(6)])
print("GMAT-reported covariance in EarthFixed, diagonal:", [ef[i][i] for i in range(6)])
vel_var = [ef[i][i] for i in range(3, 6)]
cross = max(abs(ef[i][j]) for i in range(3) for j in range(3, 6))
print()
print(f"Reported EarthFixed velocity variances: {vel_var}  (all exactly 0)")
print(f"Reported EarthFixed position-velocity cross terms, max |.|: {cross:.3e}  (exactly 0)")
sigma_v_expected = omega_e * 1.0  # km/s, for sigma_r = 1 km
print(f"Physically required velocity 1-sigma from 1 km position uncertainty in a frame rotating at omega_E: ~{sigma_v_expected:.3e} km/s = {sigma_v_expected*1e3:.4f} m/s per in-plane axis")
print(f"Expected EarthFixed velocity variance: ~{sigma_v_expected**2:.3e} km^2/s^2 ; GMAT reports 0.0")
print()
ok = all(v == 0.0 for v in vel_var) and cross == 0.0
print("REPRODUCED: covariance conversion used [[R,0],[0,R]] (no Rdot term)" if ok else "NOT reproduced: velocity block is non-zero")
