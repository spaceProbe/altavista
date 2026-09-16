"""Generate the golden arc `leo_6h_egm96_70x70.json` with GMAT's own propagator
(docs/native-dynamics-plan.md milestone N1, ADR-002).

Run explicitly, never from a test:

  .venv/bin/python goldens/gen_leo_6h_egm96_70x70.py --reason "..."

**Why this golden exists.** N1's plan names "EGM96 70x70 six hours as a new golden with its
generator and reason" -- a second, higher-degree gravity golden beyond `leo_1day_jgm2_8x8`'s
JGM2 8x8, to pin the native `spherical_harmonic_gravity`/Cunningham-Gottlieb recursion (which
this crate's own doc claims is "stable to degree 70") at the degree it claims stability to,
not only at 8x8. Same central body (Earth), same spacecraft ballistic set and epoch as
`leo_1day_jgm2_8x8.json`/`leo_1day_jgm2_8x8_sunmoon.json` (SMA 6878.0, ECC 0.001, INC 51.6,
RAAN 30.0, AOP 0.0, TA 0.0; DryMass 500, Cd 2.2, Cr 1.8, DragArea 5.0, SRPArea 5.0; epoch
"01 Jan 2026 00:00:00.000"), but EGM96 (`EGM96.cof`, the 360x360 file, truncated to degree
and order 70 by both this generator's `GravityField.Degree`/`.Order` fields and, on the
native side, `cof::read_earth_gravity`'s own `max_degree`/`max_order` truncation) rather than
JGM2, no third bodies and no other forces, and six hours (21,600 s) rather than one day --
EGM96 70x70 at ~5,000 terms per evaluation over tens of thousands of evaluations is
expensive (this task's own cost note), so the arc is shortened rather than the degree
reduced or the force model touched.

Every propagator and force-model field below is read back off the live GMAT objects after
`PrepareInternals()`, not echoed from what this script requested -- see
`gen_leo_1day_jgm2_8x8.py`'s module doc for the measured reason `propagator_readback` reads
from `prop.GetPropagator()`'s return value specifically, not the front-end `Propagator` or
the bare integrator object (identical wrinkle, identical fix, here).
"""
import argparse
import datetime
import hashlib
import json
import sys
from pathlib import Path

GMAT_ROOT = Path("/Users/probe/code/AltaVista/GMAT R2026a")
sys.path.insert(1, str(GMAT_ROOT / "bin"))
import gmatpy as gmat  # noqa: E402

gmat.Setup(str(GMAT_ROOT / "bin" / "api_startup_file.txt"))

EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {
    "SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
    "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0,
}
GRAVITY_FILE = "EGM96.cof"
DEGREE = 70
ORDER = 70
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 21600.0
GOLDEN_NAME = "leo_6h_egm96_70x70"
BALLISTIC_FIELDS = ("DryMass", "Cd", "Cr", "DragArea", "SRPArea", "TotalMass")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument("--tolerance-m", type=float, default=0.05)
    args = ap.parse_args()

    gravity_path = GMAT_ROOT / "data" / "gravity" / "earth" / GRAVITY_FILE
    gravity_sha256 = hashlib.sha256(gravity_path.read_bytes()).hexdigest()

    sat = gmat.Construct("Spacecraft", "Golden")
    sat.SetField("DateFormat", "UTCGregorian")
    sat.SetField("Epoch", EPOCH)
    sat.SetField("CoordinateSystem", "EarthMJ2000Eq")
    sat.SetField("DisplayStateType", "Keplerian")
    for k, v in SAT.items():
        sat.SetField(k, v)

    fm = gmat.Construct("ForceModel", "GoldenFM")
    fm.SetField("CentralBody", "Earth")
    grav = gmat.Construct("GravityField", "GoldenGrav")
    grav.SetField("BodyName", "Earth")
    grav.SetField("PotentialFile", GRAVITY_FILE)
    grav.SetField("Degree", DEGREE)
    grav.SetField("Order", ORDER)
    fm.AddForce(grav)

    prop = gmat.Construct("Propagator", "GoldenProp")
    gator = gmat.Construct(PROP["integrator"], "GoldenGator")
    prop.SetReference(gator)
    prop.SetReference(fm)
    prop.SetField("InitialStepSize", PROP["initial_step_s"])
    prop.SetField("Accuracy", PROP["accuracy"])
    prop.SetField("MinStep", PROP["min_step_s"])
    prop.SetField("MaxStep", PROP["max_step_s"])

    gmat.Initialize()
    prop.AddPropObject(sat)
    prop.PrepareInternals()
    internal_prop = prop.GetPropagator()

    x0 = [float(v) for v in list(internal_prop.GetState())[:6]]
    a1_epoch = sat.GetEpoch()

    elapsed = 0.0
    while elapsed < DURATION_S - 1e-9:
        dt = min(PROP["max_step_s"], DURATION_S - elapsed)
        if not internal_prop.Step(dt):
            raise RuntimeError(f"Propagator.Step({dt}) returned False at elapsed={elapsed}: raise MaxStepAttempts or use smaller chunks")
        elapsed += dt
    x1 = [float(v) for v in list(internal_prop.GetState())[:6]]

    propagator_readback = {
        "integrator": internal_prop.GetTypeName(),
        "accuracy": internal_prop.GetRealParameter("Accuracy"),
        "initial_step_s": internal_prop.GetRealParameter("InitialStepSize"),
        "min_step_s": internal_prop.GetRealParameter("MinStep"),
        "max_step_s": internal_prop.GetRealParameter("MaxStep"),
        "note": "read from prop.GetPropagator() after PrepareInternals(), not the front-end Propagator or the bare integrator object -- see gen_leo_1day_jgm2_8x8.py's module doc",
    }
    gravity_readback = {
        "body_name": grav.GetField("BodyName"),
        "potential_file": grav.GetField("PotentialFile"),
        "degree": grav.GetIntegerParameter("Degree"),
        "order": grav.GetIntegerParameter("Order"),
        "mu_km3_per_s2": grav.GetRealParameter("Mu"),
    }
    earth = gmat.GetObject("Earth")
    central_body_readback = {
        "mu_km3_per_s2": earth.GetRealParameter("Mu"),
        "equatorial_radius_km": earth.GetRealParameter("EquatorialRadius"),
        "flattening": earth.GetRealParameter("Flattening"),
    }
    ballistics_readback = {k: sat.GetRealParameter(k) for k in BALLISTIC_FIELDS}

    golden = {
        "name": GOLDEN_NAME,
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "frame": "EarthMJ2000Eq", "units": {"position": "km", "velocity": "km/s"},
        "epoch_utc": EPOCH, "epoch_a1mjd": a1_epoch,
        "spacecraft": SAT,
        "spacecraft_ballistics_readback": ballistics_readback,
        "force_model": {
            "central_body": "Earth",
            "gravity": {"file": GRAVITY_FILE, "degree": DEGREE, "order": ORDER},
            "point_masses": [],
            "srp": False, "drag": None, "relativity": False, "tides": False,
        },
        "gravity_file_name": GRAVITY_FILE,
        "gravity_file_sha256": gravity_sha256,
        "gravity_file_readback": gravity_readback,
        "central_body_readback": central_body_readback,
        "solver_iterations": None,
        "propagator": PROP,
        "propagator_readback": propagator_readback,
        "duration_s": DURATION_S,
        "initial_state": x0, "final_state": x1,
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_m * 1e-3,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    golden["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(json.dumps(golden, indent=2, sort_keys=True) + "\n")
    print("wrote", out, "final state", x1)
    print("gravity_file_sha256", gravity_sha256)
    print("propagator_readback", propagator_readback)


if __name__ == "__main__":
    main()
