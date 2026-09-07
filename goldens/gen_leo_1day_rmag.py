"""Generate `leo_1day_jgm2_8x8_sunmoon_rmag.json`: GMAT's own RMAG real parameter, read through
a genuine GMAT script + `ReportFile`, for the identical arc `leo_1day_jgm2_8x8_sunmoon.json`
pins (M10.2, `docs/open-questions.md` question 95's second half; question 99/M11.1 updated the
rationale below to match `gmat_sys::model::GmatModel::step` no longer computing `rmag` itself).

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_leo_1day_rmag.py --reason "..."

**Why a script + `ReportFile`, not the object API.** This repository has already documented
(`altavista/scenario.py`'s own module doc comment, and the AltaVista team's own memory notes) that
a `ReportFile` created and driven through the GMAT *object* API's `Execute()` does not reliably
write output. The working pattern -- used here, and by `altavista.scenario.Scenario.run_script`/
`prepare_script`/`report_block` -- is a real GMAT *script*, run through `gmat.LoadScript` +
`gmat.RunScript` (the script-engine's own mission-sequence execution), with a `ReportFile`
object injected into the script and `SolverIterations = Current` set on it.

**Why RMAG at all (updated at M11.1, question 99).** Through M10.2, `gmat_sys::model
::GmatModel::step` computed `OUTPUT_RMAG` ("rmag") directly from the propagated state in Rust,
because `GetRealParameter`-style reads were not reachable through `gmat-sys`'s FFI at all. As of
M11.1, that gap is closed: `GmatModel::step` writes the propagated state back into the bound
spacecraft and reads `"RMAG"` through `gmat_sys::DerivativeModel::real_parameter`
(`gmatffi_get_real_parameter`) -- GMAT's own real-parameter subsystem, not Rust arithmetic. This
script's `ReportFile` reading remains the right independent check even so: it is a *genuinely
different* GMAT code path from the one `GmatModel::step` now also uses -- the script engine's
own `Propagate` statement and parameter evaluation, driven through `gmat.LoadScript`/
`gmat.RunScript`, versus `gmat-sys`'s `GetDerivatives`-driven integration with the propagated
state written back into a *different* `Spacecraft` object and read via `GetRealParameter`
directly (not a `ReportFile`) -- landing on the same golden arc from two independent directions.

The GMAT script reproduces `leo_1day_jgm2_8x8_sunmoon.json`'s own epoch/spacecraft/force-model/
propagator parameters exactly (see `EPOCH`/`SAT`/`FORCE_MODEL`/`PROP` below, copied from
`gen_leo_1day.py`), propagates the identical 86400 s arc via `Propagate GoldenProp(Golden)
{Golden.ElapsedSecs = 86400}`, and reports `{A1ModJulian, EarthMJ2000Eq.{X,Y,Z,VX,VY,VZ},
Earth.RMAG}` at every internal step (the script engine's own substep cadence, not just start/
end) -- this generator reads the report's *last* row, the arc's own final state.
"""
import argparse, csv, datetime, json, math, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Copied verbatim from goldens/gen_leo_1day.py -- the plain golden's own epoch/spacecraft/
# force-model/propagator, so this script reproduces the identical arc.
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
% M10.2 ground truth: GMAT's own RMAG real parameter for goldens/leo_1day_jgm2_8x8_sunmoon.json
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
GoldenReport.Add = {{Golden.A1ModJulian, Golden.EarthMJ2000Eq.X, Golden.EarthMJ2000Eq.Y, Golden.EarthMJ2000Eq.Z, Golden.EarthMJ2000Eq.VX, Golden.EarthMJ2000Eq.VY, Golden.EarthMJ2000Eq.VZ, Golden.Earth.RMAG}};

BeginMissionSequence;

Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {duration_s}}};
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    args = ap.parse_args()

    out_dir = Path(__file__).parent
    script_path = out_dir / "_gen_leo_1day_rmag.script"
    report_path = out_dir / "_gen_leo_1day_rmag.rpt"

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
    if len(last) != 8:
        raise RuntimeError(f"expected 8 columns (A1ModJulian, X, Y, Z, VX, VY, VZ, RMAG), got {len(last)}: {last}")
    a1mjd, x_km, y_km, z_km, vx_kmps, vy_kmps, vz_kmps, rmag_km = last

    # Sanity check the ReportFile mechanism itself: GMAT's own RMAG must equal sqrt(x^2+y^2+z^2)
    # computed from the same row, to floating-point precision -- confirms RMAG really is being
    # reported as the position-vector magnitude, not something else entirely.
    rmag_from_xyz_km = math.sqrt(x_km ** 2 + y_km ** 2 + z_km ** 2)
    rmag_self_check_km = rmag_km - rmag_from_xyz_km

    golden = {
        "name": "leo_1day_jgm2_8x8_sunmoon_rmag",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "source_golden": "leo_1day_jgm2_8x8_sunmoon",
        "note": (
            "GMAT's own RMAG real parameter, read from a genuine GMAT ReportFile (script run, "
            "SolverIterations=Current -- never Execute() through the object API, per this "
            "repository's documented ReportFile quirk) for the identical arc "
            "leo_1day_jgm2_8x8_sunmoon.json pins. As of M11.1 (question 99), "
            "gmat_sys::model::GmatModel::step reads its own OUTPUT_RMAG through "
            "gmat_sys::DerivativeModel::real_parameter (gmatffi_get_real_parameter) -- GMAT's "
            "own GetRealParameter, not Rust arithmetic -- so this golden confirms that value "
            "against a second, genuinely different GMAT code path: the script engine's own "
            "Propagate statement and parameter evaluation (this file), versus gmat-sys's own "
            "GetDerivatives-driven integration with the propagated state written back into a "
            "different Spacecraft object and read via GetRealParameter directly, not a "
            "ReportFile (crates/gmat-sys/src/model.rs)."
        ),
        "frame": "EarthMJ2000Eq",
        "units": {"position": "km", "velocity": "km/s", "rmag": "km"},
        "report_last_row": {
            "epoch_a1mjd": a1mjd,
            "x_km": x_km, "y_km": y_km, "z_km": z_km,
            "vx_kmps": vx_kmps, "vy_kmps": vy_kmps, "vz_kmps": vz_kmps,
            "rmag_km": rmag_km,
        },
        "rmag_km": rmag_km,
        "rmag_m": rmag_km * 1000.0,
        "rmag_self_check_km": rmag_self_check_km,
        "rmag_self_check_note": "rmag_km - sqrt(x_km^2 + y_km^2 + z_km^2) from the same report row; should be floating-point noise, confirming RMAG is defined as the position-vector magnitude.",
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = out_dir / (golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("rmag_km =", rmag_km, "rmag_m =", rmag_km * 1000.0)
    print("self-check (should be ~0):", rmag_self_check_km)

    script_path.unlink(missing_ok=True)
    report_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
