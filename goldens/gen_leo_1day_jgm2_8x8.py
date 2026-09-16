"""Generate the golden arc `leo_1day_jgm2_8x8.json` with GMAT's own propagator
(docs/native-dynamics-plan.md milestone N1, ADR-002).

Run explicitly, never from a test:

  .venv/bin/python goldens/gen_leo_1day_jgm2_8x8.py --reason "..."

**Why this golden exists.** `goldens/leo_1day_jgm2_8x8_sunmoon.json` (the P0 golden) includes
Sun and Moon point masses -- N2 (third bodies), not in N1's scope. This golden is the SAME
arc (same epoch, same initial Keplerian elements, same ballistic set: SMA 6878.0, ECC 0.001,
INC 51.6, RAAN 30.0, AOP 0.0, TA 0.0; DryMass 500, Cd 2.2, Cr 1.8, DragArea 5.0, SRPArea 5.0;
epoch "01 Jan 2026 00:00:00.000") with the third bodies removed and no drag, SRP,
relativity, or tides added -- JGM2 8x8 central-body Earth gravity alone, one day
(86,400 s). The two goldens differ in exactly one thing: the third bodies. This is what N1's
`EarthGravityModel` (point-mass/spherical-harmonic gravity only, no third-body/drag/SRP/
relativity/tide terms -- those are N2/N3's scope) is pinned against.

Every propagator and force-model field below is READ BACK off the live GMAT objects after
`PrepareInternals()` rather than echoed from what this script requested (this task's own
rule) -- see the `*_readback` keys. One measured wrinkle worth recording: the `Propagator`
front-end object's OWN fields (the `gator`/`ProbeGator` integrator object constructed
separately and referenced via `SetReference`) do NOT reflect what was set on the
`PropSetup`/`Propagator` object even after `Initialize()` -- they stay at that integrator
class's own defaults (Accuracy ~1e-11, MinStep 0.001, MaxStep 2700 for PrinceDormand78).
The object that actually steps and DOES reflect the requested settings is
`prop.GetPropagator()`'s return value, read only after `PrepareInternals()` -- confirmed by
probing all three objects side by side (see this task's report, `n1c-probe-readback*.txt`).
`propagator_readback` below is read from that object, not from the front-end `Propagator` or
the bare integrator object.
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
GRAVITY_FILE = "JGM2.cof"
DEGREE = 8
ORDER = 8
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 86400.0
GOLDEN_NAME = "leo_1day_jgm2_8x8"
# Ballistic fields every seed carries (question 81, ADR-002 third amendment): "a seed is a
# vehicle, not a state vector."
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
        # Step(dt) returns False after MaxStepAttempts substep attempts, leaving the state
        # partially advanced (same rule gen_leo_1day.py follows).
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
        "note": "read from prop.GetPropagator() after PrepareInternals(), not the front-end Propagator or the bare integrator object -- see this script's module doc",
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
