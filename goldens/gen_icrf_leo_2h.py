"""Generate `icrf_leo_2h.json`: GMAT's own ICRF-expressed state, read through a genuine GMAT
script + `ReportFile`, for a spacecraft whose own `CoordinateSystem` is `EarthICRF` (M18.1,
`docs/open-questions.md` questions 10/124: "The trajectory expressed in ICRF matches GMAT's own
ICRF report for the same arc").

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_icrf_leo_2h.py --reason "..."

**Why a script + `ReportFile`, not the object API.** Same documented quirk `gen_leo_1day_rmag.py`
already pins: a `ReportFile` driven through the GMAT *object* API's `Execute()` does not
reliably write output, and `SaveScript` segfaults. The working pattern is a real GMAT *script*,
run through `gmat.LoadScript` + `gmat.RunScript`, with a `ReportFile` injected and
`SolverIterations = Current` set on it.

**Same vehicle/orbit/force-model/epoch as `drms/demo_two_instance.system.yaml`'s `leo_demo_sys`**
(itself field-for-field `goldens/gen_leo_1day.py`'s own SAT/force model: JGM2 8x8 Earth gravity,
Sun + Moon point masses, PrinceDormand78 at Accuracy 1e-13 / MaxStep 600) -- "the same arc" the
brief asks for is `demo_two_instance`'s own 7200 s window (`FAULT_S`/`MANEUVER_S`/`END_S` in
`goldens/gen_demo_two_instance.py`), propagated whole (no fault, no maneuver) since this golden
exists to pin the ICRF *frame*, not the fault/maneuver behaviour those two instances separately
cover. The one difference from `leo_demo_sys`: `Golden.CoordinateSystem = EarthICRF` instead of
`EarthMJ2000Eq` -- `EarthICRF` is one of GMAT's own built-in coordinate systems (needs no `Create
CoordinateSystem`, per `altavista/scenario.py`'s own `BUILTIN_CS` table) and, being inertial
(a fixed frame-bias rotation off MJ2000Eq, not a rotating body-fixed one), is a valid coordinate
system for a `DisplayStateType = Keplerian` spacecraft -- unlike `EarthFixed`, which GMAT refuses
for Keplerian elements because it is non-inertial.

`crates/av-kernel/tests/demo_two_instance.rs`'s own `icrf_products()` binds an ad hoc
`SystemDefinition` -- a clone of `leo_demo_sys` with `spacecraft.CoordinateSystem` overridden to
`"EarthICRF"`, nothing else changed -- and propagates the identical 7200 s arc through
`crate::drm::executor::execute`, then compares its own final trajectory sample (SI, converted
from `av_cdm::units::state_km_to_m`) against this golden's own final report row to the existing
golden tolerance (0.0001 m position / 7.837e-8 m/s velocity, `leo_1day_jgm2_8x8_sunmoon.json`'s
own class).
"""
import argparse, csv, datetime, json, math, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Copied verbatim from goldens/gen_leo_1day.py / drms/demo_two_instance.system.yaml's
# leo_demo_sys -- the same vehicle/orbit/force-model this golden's own Rust-side comparison
# (crates/av-kernel/tests/demo_two_instance.rs) binds through an ad hoc SystemDefinition.
EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {"SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
FORCE_MODEL_GRAVITY_FILE = "JGM2.cof"
FORCE_MODEL_GRAVITY_DEGREE = 8
FORCE_MODEL_GRAVITY_ORDER = 8
FORCE_MODEL_POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 7200.0  # demo_two_instance's own END_S -- "the same arc"

SCRIPT_TEMPLATE = """\
%----------------------------------------------------------------------------
% M18.1 ground truth: GMAT's own ICRF-expressed state for goldens/icrf_leo_2h.json
%----------------------------------------------------------------------------

Create Spacecraft Golden;
Golden.DateFormat = UTCGregorian;
Golden.Epoch = '{epoch}';
Golden.CoordinateSystem = EarthICRF;
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
GoldenReport.Add = {{Golden.A1ModJulian, Golden.EarthICRF.X, Golden.EarthICRF.Y, Golden.EarthICRF.Z, Golden.EarthICRF.VX, Golden.EarthICRF.VY, Golden.EarthICRF.VZ, Golden.EarthMJ2000Eq.X, Golden.EarthMJ2000Eq.Y, Golden.EarthMJ2000Eq.Z, Golden.EarthMJ2000Eq.VX, Golden.EarthMJ2000Eq.VY, Golden.EarthMJ2000Eq.VZ}};

BeginMissionSequence;

Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {duration_s}}};
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    args = ap.parse_args()

    out_dir = Path(__file__).parent
    script_path = out_dir / "_gen_icrf_leo_2h.script"
    report_path = out_dir / "_gen_icrf_leo_2h.rpt"

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
    if len(last) != 13:
        raise RuntimeError(f"expected 13 columns (A1ModJulian, ICRF x6, MJ2000Eq x6), got {len(last)}: {last}")
    a1mjd = last[0]
    icrf = last[1:7]
    mj2000eq = last[7:13]

    golden = {
        "name": "icrf_leo_2h",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "note": (
            "GMAT's own ICRF-expressed state (Golden.CoordinateSystem = EarthICRF, a built-in "
            "GMAT coordinate system), read from a genuine GMAT ReportFile (script run, "
            "SolverIterations=Current -- never Execute() through the object API, per this "
            "repository's documented ReportFile quirk). Same vehicle/orbit/force-model/epoch as "
            "drms/demo_two_instance.system.yaml's leo_demo_sys (itself goldens/gen_leo_1day.py's "
            "own SAT/force model), propagated over that fixture's own 7200 s arc with no fault "
            "or maneuver -- this golden pins the ICRF frame itself, not the fault/maneuver "
            "behaviour drms/demo_two_instance.drm.yaml separately covers. M19.1 (question 128) "
            "additionally reports the identical instant's EarthMJ2000Eq state in the same row -- "
            "both numbers come from the same GMAT script run/PrinceDormand78 propagation, so "
            "converting state_final_km_mj2000eq through gmat_sys::Gmat::convert and comparing "
            "against state_final_km (ICRF) isolates the CONVERSION itself from any Dopri5-vs-"
            "PrinceDormand78 integrator divergence (crates/gmat-sys/tests/convert.rs's own use)."
        ),
        "frame": "EarthICRF",
        "units": {"position": "km", "velocity": "km/s"},
        "epoch_utc": EPOCH,
        "keplerian": SAT,
        "force_model": {"central_body": "Earth", "gravity": {"file": FORCE_MODEL_GRAVITY_FILE, "degree": FORCE_MODEL_GRAVITY_DEGREE, "order": FORCE_MODEL_GRAVITY_ORDER}, "point_masses": FORCE_MODEL_POINT_MASSES},
        "propagator": PROP,
        "duration_s": DURATION_S,
        "report_last_row": {
            "epoch_a1mjd": a1mjd,
            "x_km": icrf[0], "y_km": icrf[1], "z_km": icrf[2],
            "vx_kmps": icrf[3], "vy_kmps": icrf[4], "vz_kmps": icrf[5],
        },
        "state_final_km": icrf,
        "state_final_km_mj2000eq": mj2000eq,
        "tolerance_m": 0.0001,
        "tolerance_mps": 7.837e-8,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = out_dir / (golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("state_final_km (EarthICRF) =", icrf)
    print("state_final_km_mj2000eq (EarthMJ2000Eq) =", mj2000eq)

    script_path.unlink(missing_ok=True)
    report_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
