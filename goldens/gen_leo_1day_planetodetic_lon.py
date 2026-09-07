"""Generate `leo_1day_jgm2_8x8_sunmoon_planetodetic_lon.json`: GMAT's own Earth-fixed
Longitude/Latitude, read through a genuine GMAT script + `ReportFile`, for the identical arc
`leo_1day_jgm2_8x8_sunmoon.json` pins (question 105, M12.2).

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_leo_1day_planetodetic_lon.py --reason "..."

**Why this golden exists.** Question 99/M11.1 closed the gap that let `GmatModel::step` read
GMAT's own `"RMAG"`/`"Cd"` real parameters after a step, but neither of those two outputs depends
on epoch at all: `RMAG = sqrt(x^2+y^2+z^2)` and `Cd` are both pure functions of the Cartesian
state (or, for `Cd`, not even of the state), so a test built only around them could never catch
the epoch never being written back to the spacecraft (question 105) -- that is the exact trap
`docs/open-questions.md` question 105 warns against. `Golden.Earth.Longitude` (GMAT's
`PlanetData`-based body-fixed longitude, `third_party/gmat-src/src/base/parameter/
PlanetData.cpp::GetPlanetReal`) genuinely depends on the spacecraft's epoch: it converts the
spacecraft's inertial Cartesian state into Earth's body-fixed frame using
`mSpacecraft->GetEpoch()` to look up Earth's rotation at that instant
(`CelestialBody::GetHourAngle`/`CoordinateConverter::Convert`), so the *same* inertial position at
two different epochs reports two different longitudes -- Earth rotates about 360.9856 degrees per
day. `Golden.Earth.Latitude` is reported alongside it as a second, independent cross-check (its
dependence on epoch is smaller -- driven by precession/nutation of the body-fixed frame relative
to the mean-J2000 equator, not by Earth's fast diurnal spin -- so it is not the primary parameter,
but agreement on both numbers is stronger evidence than agreement on one).

**How `crates/gmat-sys/tests/epoch_writeback.rs` reaches the same physical quantity without a
`ReportFile` or a Parameter object.** `Golden.Earth.Longitude` is a `Parameter` object
(`PlanetData`), reachable only through the script engine's own `Moderator::CreateParameter`
wiring (owner Spacecraft + origin CelestialBody ref objects) -- not through
`GmatBase::GetRealParameter(name)` on a bare `Spacecraft` handle, which is all `gmat-sys`'s C shim
exposes. The Rust test instead reads `"PlanetodeticLON"`/`"PlanetodeticLAT"` -- fields Spacecraft
itself understands natively (`Spacecraft::MULT_REP_STRINGS`) -- off a second, otherwise-inert
"mirror" spacecraft configured with `CoordinateSystem = EarthFixed` (built via the shim's already
generic `Construct`/`SetField`/`SetReference` calls: `Construct("BodyFixed", ...)` for the axis
system, `Construct("CoordinateSystem", "EarthFixed")` with `.Origin = Earth` and that axis system
as its reference). `Spacecraft::GetStateInRepresentation` only rotates into the body-fixed frame
(using the spacecraft's own epoch) when its display `CoordinateSystem` differs from GMAT's
internal one, which is why a mirror is needed at all -- reading `"PlanetodeticLON"` off the
*production* spacecraft (`CoordinateSystem = EarthMJ2000Eq`, matching internal) would silently
skip the rotation and give the exact epoch-independent trap this golden is built to avoid; see
that test's own doc comment for the full reasoning, and this repository's memory notes for the
underlying `Spacecraft.cpp` reading. `PlanetodeticLON`/`LAT` and `Earth.Longitude`/`Latitude` were
verified numerically identical (agreement ~4e-9 degrees, floating-point noise) for a fixed
state+epoch in an exploratory check before this generator was written -- they are the same
physical quantity computed two different ways (`CartesianToPlanetodetic`,
`third_party/gmat-src/src/gmatutil/util/StateConversionUtil.cpp`, vs. `PlanetData::GetPlanetReal`
+ `GmatCalcUtil::CalculatePlanetData`), which is exactly the point of this golden: it lets the
Rust-side mirror-spacecraft reading be checked against a genuinely different GMAT code path
(the script engine's own Parameter evaluation), the same pattern `gen_leo_1day_rmag.py` uses for
RMAG.

The GMAT script reproduces `leo_1day_jgm2_8x8_sunmoon.json`'s own epoch/spacecraft/force-model/
propagator parameters exactly (copied from `gen_leo_1day.py`/`gen_leo_1day_rmag.py`), propagates
the identical 86400 s arc via `Propagate GoldenProp(Golden) {Golden.ElapsedSecs = 86400}`, and
reports `{A1ModJulian, EarthMJ2000Eq.{X,Y,Z,VX,VY,VZ}, Earth.Longitude, Earth.Latitude}` at every
internal step -- this generator reads the report's *last* row, the arc's own final state.
"""
import argparse, datetime, csv, json, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Copied verbatim from goldens/gen_leo_1day.py / gen_leo_1day_rmag.py -- the plain golden's own
# epoch/spacecraft/force-model/propagator, so this script reproduces the identical arc.
EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {"SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
FORCE_MODEL_GRAVITY_FILE = "JGM2.cof"
FORCE_MODEL_GRAVITY_DEGREE = 8
FORCE_MODEL_GRAVITY_ORDER = 8
FORCE_MODEL_POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 86400.0

SCRIPT_TEMPLATE = """\
%----------------------------------------------------------------------------
% M12.2 ground truth: GMAT's own Earth-fixed Longitude/Latitude for goldens/leo_1day_jgm2_8x8_sunmoon.json
%----------------------------------------------------------------------------

Create Spacecraft Golden;
Golden.DateFormat = UTCGregorian;
Golden.Epoch = '{epoch}';
Golden.CoordinateSystem = EarthMJ2000Eq;
Golden.DisplayStateType = Keplerian;
Golden.SMA = {sma};
Golden.ECC = {ecc};
Golden.INC = {inc};
Golden.RAAN = {raan};
Golden.AOP = {aop};
Golden.TA = {ta};
Golden.DryMass = {dry_mass};
Golden.Cd = {cd};
Golden.Cr = {cr};
Golden.DragArea = {drag_area};
Golden.SRPArea = {srp_area};

Create ForceModel GoldenFM;
GoldenFM.CentralBody = Earth;
GoldenFM.PrimaryBodies = {{Earth}};
GoldenFM.PointMasses = {{{point_masses}}};
GoldenFM.Drag = None;
GoldenFM.SRP = Off;
GoldenFM.GravityField.Earth.PotentialFile = '{gravity_file}';
GoldenFM.GravityField.Earth.Degree = {gravity_degree};
GoldenFM.GravityField.Earth.Order = {gravity_order};

Create Propagator GoldenProp;
GoldenProp.FM = GoldenFM;
GoldenProp.Type = {integrator};
GoldenProp.InitialStepSize = {initial_step_s};
GoldenProp.Accuracy = {accuracy};
GoldenProp.MinStep = {min_step_s};
GoldenProp.MaxStep = {max_step_s};

Create ReportFile GoldenReport;
GoldenReport.Filename = '{report_path}';
GoldenReport.Precision = 16;
GoldenReport.WriteHeaders = false;
GoldenReport.LeftJustify = On;
GoldenReport.ZeroFill = Off;
GoldenReport.FixedWidth = false;
GoldenReport.Delimiter = ',';
GoldenReport.WriteReport = true;
GoldenReport.SolverIterations = Current;
GoldenReport.Add = {{Golden.A1ModJulian, Golden.EarthMJ2000Eq.X, Golden.EarthMJ2000Eq.Y, Golden.EarthMJ2000Eq.Z, Golden.EarthMJ2000Eq.VX, Golden.EarthMJ2000Eq.VY, Golden.EarthMJ2000Eq.VZ, Golden.Earth.Longitude, Golden.Earth.Latitude}};

BeginMissionSequence;

Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {duration_s}}};
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    args = ap.parse_args()

    out_dir = Path(__file__).parent
    script_path = out_dir / "_gen_leo_1day_planetodetic_lon.script"
    report_path = out_dir / "_gen_leo_1day_planetodetic_lon.rpt"

    script_text = SCRIPT_TEMPLATE.format(
        epoch=EPOCH, sma=SAT["SMA"], ecc=SAT["ECC"], inc=SAT["INC"], raan=SAT["RAAN"], aop=SAT["AOP"], ta=SAT["TA"],
        dry_mass=SAT["DryMass"], cd=SAT["Cd"], cr=SAT["Cr"], drag_area=SAT["DragArea"], srp_area=SAT["SRPArea"],
        point_masses=", ".join(FORCE_MODEL_POINT_MASSES), gravity_file=FORCE_MODEL_GRAVITY_FILE,
        gravity_degree=FORCE_MODEL_GRAVITY_DEGREE, gravity_order=FORCE_MODEL_GRAVITY_ORDER,
        integrator=PROP["integrator"], initial_step_s=PROP["initial_step_s"], accuracy=PROP["accuracy"],
        min_step_s=PROP["min_step_s"], max_step_s=PROP["max_step_s"], report_path=str(report_path),
        duration_s=DURATION_S,
    )
    script_path.write_text(script_text)

    cwd = os.getcwd()
    try:
        os.chdir(GMAT_BIN)
        if not gmat.LoadScript(str(script_path)):
            raise RuntimeError(f"GMAT failed to load {script_path}")
        if not gmat.RunScript():
            raise RuntimeError(f"GMAT failed to run {script_path}")
    finally:
        os.chdir(cwd)

    if not report_path.exists():
        raise RuntimeError(f"GMAT did not write a ReportFile at {report_path}")
    with open(report_path, newline="") as fh:
        rows = list(csv.reader(fh))
    if not rows:
        raise RuntimeError(f"ReportFile {report_path} is empty")
    last = [float(v) for v in rows[-1]]
    if len(last) != 9:
        raise RuntimeError(f"expected 9 columns (A1ModJulian, X, Y, Z, VX, VY, VZ, Longitude, Latitude), got {len(last)}: {last}")
    a1mjd, x_km, y_km, z_km, vx_kmps, vy_kmps, vz_kmps, longitude_deg, latitude_deg = last

    golden = {
        "name": "leo_1day_jgm2_8x8_sunmoon_planetodetic_lon",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "source_golden": "leo_1day_jgm2_8x8_sunmoon",
        "note": (
            "GMAT's own Earth-fixed Longitude/Latitude (PlanetData::GetPlanetReal, a genuinely "
            "epoch-dependent real parameter -- Longitude moves ~360.9856 deg/day at fixed "
            "inertial position, unlike RMAG/Cd which do not depend on epoch at all), read from a "
            "genuine GMAT ReportFile (script run, SolverIterations=Current -- never Execute() "
            "through the object API, per this repository's documented ReportFile quirk) for the "
            "identical arc leo_1day_jgm2_8x8_sunmoon.json pins. Question 105/M12.2: this golden "
            "exists to verify gmat_sys::model::GmatModel::step/step_with_stm's epoch write-back "
            "(gmat_sys::DerivativeModel::sync_spacecraft_epoch_a1mjd) against a genuinely "
            "different GMAT code path -- the script engine's own Longitude/Latitude Parameter "
            "evaluation (this file), versus the Rust test's own mirror-spacecraft read of "
            "PlanetodeticLON/PlanetodeticLAT through gmat_sys::DerivativeModel::real_parameter / "
            "crate::Object::real_parameter (crates/gmat-sys/tests/epoch_writeback.rs)."
        ),
        "frame": "EarthMJ2000Eq",
        "units": {"position": "km", "velocity": "km/s", "longitude": "deg", "latitude": "deg"},
        "report_last_row": {
            "epoch_a1mjd": a1mjd,
            "x_km": x_km, "y_km": y_km, "z_km": z_km,
            "vx_kmps": vx_kmps, "vy_kmps": vy_kmps, "vz_kmps": vz_kmps,
            "longitude_deg": longitude_deg,
            "latitude_deg": latitude_deg,
        },
        "epoch_a1mjd": a1mjd,
        "longitude_deg": longitude_deg,
        "latitude_deg": latitude_deg,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = out_dir / (golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("epoch_a1mjd =", a1mjd)
    print("longitude_deg =", longitude_deg, "latitude_deg =", latitude_deg)

    script_path.unlink(missing_ok=True)
    report_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
