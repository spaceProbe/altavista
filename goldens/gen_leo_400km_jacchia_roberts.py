"""Generate the golden arc `leo_400km_jacchia_roberts.json` with GMAT's own propagator
(docs/native-dynamics-plan.md milestone N3, task 3b: "the drag force, the Jacchia-Roberts
atmosphere").

Run explicitly, never from a test:

  .venv/bin/python goldens/gen_leo_400km_jacchia_roberts.py --reason "..." --tolerance-m <measured>

**Why this golden exists, and why this exact arc.** N3's existing goldens
(`leo_1day_jgm2_8x8_sunmoon_srp.json`, `leo_1day_jgm2_8x8_sunmoon_drag_srp.json`) either have
no drag at all, or mix drag with SRP and third bodies on a 250 km orbit -- this golden isolates
DRAG ALONE (JGM2 8x8 central-body Earth gravity, `DragForce` with the `JacchiaRoberts`
atmosphere, and NOTHING else: no third bodies, no SRP, no relativity, no tides) on a 400 km
CIRCULAR orbit (`SMA = 6378.1363 + 400 = 6778.1363 km`, `ECC = 0.0` exactly), so the native
model's drag term alone is what this golden pins, the same isolation
`gen_leo_1day_jgm2_8x8.py` (N1's gravity-only golden) established for gravity relative to the
P0 arc.

**Weather source: GMAT's own CONSTANT defaults, explicitly set (not relied on implicitly).**
Read from GMAT's own source (`DragForce.cpp`'s constructor: `historicWSource =
predictedWSource = "ConstantFluxAndGeoMag"`, `fluxF107 = fluxF107A = 150.0`, `kp = 3.0`),
confirmed live -- see `crates/av-orbital/src/weather.rs`'s own module doc for the full account
and the live-probe evidence. This script sets these fields EXPLICITLY (rather than leaving
them at their implicit defaults) so the golden is self-documenting even if GMAT's own
defaults ever change, and reads every one of them back off the live, initialized `DragForce`
object below (`weather_readback`). **The CSSI space-weather file's own SHA-256 is recorded
regardless** (`weather_file_sha256`) -- N3's own explicit requirement -- because
`DragForce::Initialize()` unconditionally validates the file exists on disk even in constant
mode (`FileManager::DoesFileExist`, confirmed by reading `DragForce.cpp`), so every arc this
generator produces is tied to that exact file's bytes whether or not its VALUES were read.

**Duration: measured, not assumed.** At 400 km circular the atmosphere is dense enough that
drag is a real, measurable perturbation over a day (see `shadow`-free -- there is no SRP here
-- `drag_summary` below, this script's own measurement of the along-track drift attributable
to drag, by differencing against a SEPARATE gravity-only propagation of the identical initial
state). If `Propagator.Step` cannot complete the full 86,400 s without exceeding
`MaxStepAttempts`, this script falls back to the largest whole number of 600 s steps that DID
complete and records the SHORTER duration actually flown, rather than silently padding or
raising past this task's own step-return-value rule (question 77).

Every propagator and force-model field below is READ BACK off the live GMAT objects after
`PrepareInternals()`, matching every existing generator's own convention
(`gen_leo_1day_jgm2_8x8.py`'s module doc: `propagator_readback` must come from
`prop.GetPropagator()`'s return value).

**The tolerance is measured, not assumed** (`--tolerance-m` has no default -- ADR-002's
goldens rule). Procedure: (1) generate this golden with a placeholder tolerance far above
anything plausible; (2) run `crates/av-orbital/tests/drag_goldens.rs`'s trajectory-residual
test and read its printed measured residual; (3) re-run this script with `--tolerance-m` set
to that measured residual plus a stated margin -- see this task's own report for the exact
value and the regeneration command used.
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
EARTH_EQUATORIAL_RADIUS_KM = 6378.1363
ALTITUDE_KM = 400.0
SAT = {
    "SMA": EARTH_EQUATORIAL_RADIUS_KM + ALTITUDE_KM, "ECC": 0.0, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
    "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0,
}
GRAVITY_FILE = "JGM2.cof"
DEGREE = 8
ORDER = 8
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 86400.0
GOLDEN_NAME = "leo_400km_jacchia_roberts"
BALLISTIC_FIELDS = ("DryMass", "Cd", "Cr", "DragArea", "SRPArea", "TotalMass")
WEATHER_FILE_NAME = "SpaceWeather-All-v1.2.txt"
WEATHER_FIELDS = ("HistoricWeatherSource", "PredictedWeatherSource", "CSSISpaceWeatherFile", "SchattenFile")
WEATHER_REAL_FIELDS = ("F107", "F107A", "MagneticIndex")
CONSTANT_F107 = 150.0
CONSTANT_F107A = 150.0
CONSTANT_KP = 3.0


def _build(prefix, drag: bool):
    """Builds a Spacecraft/ForceModel/Propagator: gravity always, DragForce+JacchiaRoberts
    only when `drag` is True (used twice -- the golden's own drag arc, and a SEPARATE
    gravity-only propagation of the identical initial state, to measure `drag_summary`'s own
    along-track drift attributable to drag alone)."""
    sat = gmat.Construct("Spacecraft", f"{prefix}Sat")
    sat.SetField("DateFormat", "UTCGregorian")
    sat.SetField("Epoch", EPOCH)
    sat.SetField("CoordinateSystem", "EarthMJ2000Eq")
    sat.SetField("DisplayStateType", "Keplerian")
    for k, v in SAT.items():
        sat.SetField(k, v)

    fm = gmat.Construct("ForceModel", f"{prefix}FM")
    fm.SetField("CentralBody", "Earth")
    grav = gmat.Construct("GravityField", f"{prefix}Grav")
    grav.SetField("BodyName", "Earth")
    grav.SetField("PotentialFile", GRAVITY_FILE)
    grav.SetField("Degree", DEGREE)
    grav.SetField("Order", ORDER)
    fm.AddForce(grav)

    df = None
    if drag:
        df = gmat.Construct("DragForce", f"{prefix}Drag")
        df.SetField("AtmosphereModel", "JacchiaRoberts")
        df.SetField("HistoricWeatherSource", "ConstantFluxAndGeoMag")
        df.SetField("PredictedWeatherSource", "ConstantFluxAndGeoMag")
        df.SetField("F107", CONSTANT_F107)
        df.SetField("F107A", CONSTANT_F107A)
        df.SetField("MagneticIndex", CONSTANT_KP)
        atmos = gmat.Construct("JacchiaRoberts", f"{prefix}Atmos")
        df.SetReference(atmos)
        fm.AddForce(df)

    prop = gmat.Construct("Propagator", f"{prefix}Prop")
    gator = gmat.Construct(PROP["integrator"], f"{prefix}Gator")
    prop.SetReference(gator)
    prop.SetReference(fm)
    prop.SetField("InitialStepSize", PROP["initial_step_s"])
    prop.SetField("Accuracy", PROP["accuracy"])
    prop.SetField("MinStep", PROP["min_step_s"])
    prop.SetField("MaxStep", PROP["max_step_s"])
    return sat, fm, grav, df, prop


def _step_as_far_as_possible(internal_prop, duration_s, max_step_s):
    """Steps up to `duration_s`, MaxStep at a time; if `Propagator.Step` returns False
    (MaxStepAttempts exceeded, question 77), stops there and returns the elapsed time actually
    flown rather than raising -- this golden's own stated fallback (see this file's own module
    doc, 'Duration: measured, not assumed')."""
    elapsed = 0.0
    while elapsed < duration_s - 1e-9:
        dt = min(max_step_s, duration_s - elapsed)
        if not internal_prop.Step(dt):
            return elapsed
        elapsed += dt
    return elapsed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument(
        "--tolerance-m",
        type=float,
        required=True,
        help=(
            "the position tolerance (m) crates/av-orbital/tests/drag_goldens.rs reads from "
            "this file -- NO DEFAULT. Measure the actual trajectory residual first, then pass "
            "that measured value here plus a stated margin."
        ),
    )
    args = ap.parse_args()

    gravity_path = GMAT_ROOT / "data" / "gravity" / "earth" / GRAVITY_FILE
    gravity_sha256 = hashlib.sha256(gravity_path.read_bytes()).hexdigest()
    weather_path = GMAT_ROOT / "data" / "atmosphere" / "earth" / WEATHER_FILE_NAME
    weather_sha256 = hashlib.sha256(weather_path.read_bytes()).hexdigest()

    sat, fm, grav, df, prop = _build("Golden", drag=True)
    gsat, gfm, ggrav, _gdf, gprop = _build("GravOnly", drag=False)

    gmat.Initialize()
    prop.AddPropObject(sat)
    prop.PrepareInternals()
    internal_prop = prop.GetPropagator()
    gprop.AddPropObject(gsat)
    gprop.PrepareInternals()
    ginternal_prop = gprop.GetPropagator()

    x0 = [float(v) for v in list(internal_prop.GetState())[:6]]
    gx0 = [float(v) for v in list(ginternal_prop.GetState())[:6]]
    a1_epoch = sat.GetEpoch()
    if any(abs(a - b) > 1e-9 for a, b in zip(x0, gx0)):
        raise RuntimeError(f"drag and gravity-only initial states disagree: {x0} vs {gx0}")

    weather_readback = {f: df.GetField(f) for f in WEATHER_FIELDS}
    weather_readback.update({f: df.GetRealParameter(f) for f in WEATHER_REAL_FIELDS})

    elapsed = _step_as_far_as_possible(internal_prop, DURATION_S, PROP["max_step_s"])
    if elapsed < DURATION_S - 1e-6:
        print(f"WARNING: drag arc only completed {elapsed} s of the requested {DURATION_S} s before MaxStepAttempts; recording the shorter duration actually flown")
    x1 = [float(v) for v in list(internal_prop.GetState())[:6]]

    gelapsed = _step_as_far_as_possible(ginternal_prop, elapsed, PROP["max_step_s"])
    if abs(gelapsed - elapsed) > 1e-6:
        raise RuntimeError(f"gravity-only comparison propagation did not complete the SAME elapsed time as the drag arc: {gelapsed} vs {elapsed}")
    gx1 = [float(v) for v in list(ginternal_prop.GetState())[:6]]

    # drag_summary: how far the drag arc's own end state has moved from the gravity-only arc's
    # end state, over the SAME elapsed time from the SAME initial state -- the measured drag
    # perturbation this golden's own module doc names, computed independently of x0/x1 above
    # (never alters them).
    dr = sum((x1[i] - gx1[i]) ** 2 for i in range(3)) ** 0.5 * 1000.0  # km -> m
    dv = sum((x1[i] - gx1[i]) ** 2 for i in range(3, 6)) ** 0.5 * 1000.0  # km/s -> m/s
    drag_summary = {
        "position_drift_from_gravity_only_m": dr,
        "velocity_drift_from_gravity_only_m_s": dv,
        "elapsed_s": elapsed,
        "note": "the L2 distance between this golden's own drag-arc end state and a SEPARATE gravity-only propagation of the identical initial state over the identical elapsed time -- the measured perturbation attributable to drag alone, never used to derive x0/x1 above",
    }

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
        "polar_radius_km": earth.GetRealParameter("PolarRadius"),
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
            "srp": False, "drag": "JacchiaRoberts", "relativity": False, "tides": False,
        },
        "gravity_file_name": GRAVITY_FILE,
        "gravity_file_sha256": gravity_sha256,
        "gravity_file_readback": gravity_readback,
        "central_body_readback": central_body_readback,
        "weather_file_name": WEATHER_FILE_NAME,
        "weather_file_sha256": weather_sha256,
        "weather_readback": weather_readback,
        "drag_summary": drag_summary,
        "solver_iterations": None,
        "propagator": PROP,
        "propagator_readback": propagator_readback,
        "duration_s": elapsed,
        "requested_duration_s": DURATION_S,
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
    print("weather_file_sha256", weather_sha256)
    print("weather_readback", weather_readback)
    print("propagator_readback", propagator_readback)
    print("drag_summary", drag_summary)


if __name__ == "__main__":
    main()
