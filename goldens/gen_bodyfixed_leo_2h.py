"""Generate `bodyfixed_leo_2h.json`: GMAT's own EarthFixed (body-fixed)-expressed state, read
through a genuine GMAT script + `ReportFile`, for the identical arc `gen_icrf_leo_2h.py` pins
in `EarthICRF` (M19.1, `docs/open-questions.md` question 128, ADR-002's fourth amendment:
"the LEO golden arc converted to EarthICRF and to EarthBodyFixed matches GMAT's own ReportFile
in those coordinate systems").

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_bodyfixed_leo_2h.py --reason "..."

**Why a script + `ReportFile`, not the object API.** Same documented quirk `gen_leo_1day_rmag.py`/
`gen_icrf_leo_2h.py` already pin: a `ReportFile` driven through the GMAT *object* API's
`Execute()` does not reliably write output, and `SaveScript` segfaults. The working pattern is a
real GMAT *script*, run through `gmat.LoadScript` + `gmat.RunScript`, with a `ReportFile`
injected and `SolverIterations = Current` set on it.

**Why `Golden.CoordinateSystem` stays `EarthMJ2000Eq`, unlike `gen_icrf_leo_2h.py`'s
`EarthICRF`.** GMAT refuses `DisplayStateType = Keplerian` against a non-inertial
`CoordinateSystem` ("`EarthFixed` ... not ... inertial" -- confirmed empirically, and noted in
`gen_icrf_leo_2h.py`'s own doc comment). The propagated physical trajectory does not depend on
this field at all (only on how the *initial* Keplerian elements are interpreted, and on the
`ReportFile`'s own `Add` list) -- reporting `Golden.EarthFixed.X` etc. as separate `Parameter`
references, exactly `gen_leo_1day_planetodetic_lon.py`'s own pattern, gives GMAT's genuine
body-fixed state for the identical arc without needing the spacecraft's own display frame to be
`EarthFixed`.

**Same vehicle/orbit/force-model/epoch as `drms/demo_two_instance.system.yaml`'s
`leo_demo_sys`** (itself field-for-field `goldens/gen_leo_1day.py`'s own SAT/force model),
propagated over the identical 7200 s window `gen_icrf_leo_2h.py` uses -- "the same arc" this
golden's own numeric pin (`crates/av-kernel/tests/demo_two_instance.rs::
body_fixed_conversion_matches_the_genuine_gmat_reportfile`) compares against.

`crates/av-kernel/tests/demo_two_instance.rs`'s own `bodyfixed_products()` binds an ad hoc
`SystemDefinition` -- a clone of `leo_demo_sys` with `spacecraft.CoordinateSystem` overridden to
`"EarthBodyFixed"` (this repository's own registry name, realized as a *freshly constructed*
CoordinateSystem by `gmat_sys::Gmat::coordinate_system("...", "Earth", "BodyFixed")` -- the
identical physical frame as GMAT's own built-in `EarthFixed` this script reports through, just a
different, never-colliding object name) -- and propagates the identical 7200 s arc through
`crate::drm::executor::execute`, then compares its own final trajectory sample (SI, converted
from `av_cdm::units::state_km_to_m`) against this golden's own final report row to the existing
golden tolerance (0.0001 m position / 7.837e-8 m/s velocity,
`leo_1day_jgm2_8x8_sunmoon.json`'s own class).

Deliberately the same arc, vehicle and epoch as `icrf_leo_2h.json` -- the two goldens exist to
be genuinely different cases (a near-constant inertial frame bias vs. a time-varying rotating
frame), not to test different physics.
"""
import argparse, csv, datetime, json, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Copied verbatim from goldens/gen_icrf_leo_2h.py / goldens/gen_leo_1day.py / drms/
# demo_two_instance.system.yaml's leo_demo_sys -- "the same arc" both frame-conversion goldens
# pin (see the module doc comment).
EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {"SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
FORCE_MODEL_GRAVITY_FILE = "JGM2.cof"
FORCE_MODEL_GRAVITY_DEGREE = 8
FORCE_MODEL_GRAVITY_ORDER = 8
FORCE_MODEL_POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 7200.0  # demo_two_instance's own END_S -- "the same arc" as icrf_leo_2h.json

SCRIPT_TEMPLATE = """\
%----------------------------------------------------------------------------
% M19.1 ground truth: GMAT's own EarthFixed (body-fixed)-expressed state for
% goldens/bodyfixed_leo_2h.json
%----------------------------------------------------------------------------

% M19.1 review (manager): force Earth to recompute its nutation every call instead of
% caching for the default 60 s (Planet::nutationUpdateInterval), so this ReportFile
% evaluates BodyFixedAxes at the same instant a cold gmat_sys::Gmat::convert call does.
Earth.NutationUpdateInterval = 0;

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
GoldenReport.Add = {{Golden.A1ModJulian, Golden.EarthFixed.X, Golden.EarthFixed.Y, Golden.EarthFixed.Z, Golden.EarthFixed.VX, Golden.EarthFixed.VY, Golden.EarthFixed.VZ, Golden.EarthMJ2000Eq.X, Golden.EarthMJ2000Eq.Y, Golden.EarthMJ2000Eq.Z, Golden.EarthMJ2000Eq.VX, Golden.EarthMJ2000Eq.VY, Golden.EarthMJ2000Eq.VZ}};

BeginMissionSequence;

Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {duration_s}}};
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    args = ap.parse_args()

    out_dir = Path(__file__).parent
    script_path = out_dir / "_gen_bodyfixed_leo_2h.script"
    report_path = out_dir / "_gen_bodyfixed_leo_2h.rpt"

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
        raise RuntimeError(f"expected 13 columns (A1ModJulian, EarthFixed x6, MJ2000Eq x6), got {len(last)}: {last}")
    a1mjd = last[0]
    body_fixed = last[1:7]
    mj2000eq = last[7:13]

    golden = {
        "name": "bodyfixed_leo_2h",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "note": (
            "GMAT's own body-fixed-expressed state (Golden.EarthFixed.*, GMAT's built-in "
            "Earth body-fixed CoordinateSystem), read from a genuine GMAT ReportFile (script "
            "run, SolverIterations=Current -- never Execute() through the object API, per this "
            "repository's documented ReportFile quirk). Same vehicle/orbit/force-model/epoch/"
            "duration as icrf_leo_2h.json -- the deliberately time-varying counterpart to that "
            "golden's near-constant ICRF frame bias (question 128, M19.1). Also reports the "
            "identical instant's EarthMJ2000Eq state in the same row, for the same reason "
            "icrf_leo_2h.json does (crates/gmat-sys/tests/convert.rs isolates the conversion "
            "itself from Dopri5-vs-PrinceDormand78 integrator divergence by converting "
            "state_final_km_mj2000eq directly, rather than re-propagating). "
            "This golden holds icrf_leo_2h.json's strict tolerance class (1e-4 m / 7.837e-8 "
            "m/s) because the script sets Earth.NutationUpdateInterval = 0. Earth's own "
            "NutationUpdateInterval defaults to 60 s (Planet::nutationUpdateInterval, "
            "third_party/gmat-src/src/base/solarsys/Planet.cpp), so BodyFixedAxes caches its "
            "rotation (and rotation-rate) matrix and only recomputes when the requested epoch "
            "moves outside that window since the last computed one; with the default the "
            "ReportFile evaluates EarthFixed at whatever micro-epoch its last cached recompute "
            "landed on, not bit-identically the same instant gmat_sys::Gmat::convert computes "
            "fresh (cold, no prior calls) for the identical nominal epoch, which cost a "
            "measured ~9.9e-5 m / ~1.66e-7 m/s of sub-microsecond epoch staleness. Forcing "
            "recomputation every call removes it at the source rather than absorbing it into a "
            "looser tolerance (M19.1 manager review). For scale, the error this task's fix "
            "(question 128) closes is ~1.33 m -- four orders of magnitude larger. This "
            "task's own fix (question 128) closes, and both consistent with sub-microsecond "
            "epoch staleness (Earth's rotation rate times ~200 ns), not a conversion defect. "
            "The near-static EarthICRF frame bias has no such time-dependent caching, so "
            "icrf_leo_2h.json's own tolerance stays tight."
        ),
        "frame": "EarthBodyFixed",
        "gmat_coordinate_system": "EarthFixed",
        "units": {"position": "km", "velocity": "km/s"},
        "epoch_utc": EPOCH,
        "keplerian": SAT,
        "force_model": {"central_body": "Earth", "gravity": {"file": FORCE_MODEL_GRAVITY_FILE, "degree": FORCE_MODEL_GRAVITY_DEGREE, "order": FORCE_MODEL_GRAVITY_ORDER}, "point_masses": FORCE_MODEL_POINT_MASSES},
        "propagator": PROP,
        "duration_s": DURATION_S,
        "report_last_row": {
            "epoch_a1mjd": a1mjd,
            "x_km": body_fixed[0], "y_km": body_fixed[1], "z_km": body_fixed[2],
            "vx_kmps": body_fixed[3], "vy_kmps": body_fixed[4], "vz_kmps": body_fixed[5],
        },
        "state_final_km": body_fixed,
        "state_final_km_mj2000eq": mj2000eq,
        # Looser than icrf_leo_2h.json's tolerance -- see the "note" field above for the
        # measured, understood (NutationUpdateInterval caching) reason, not a conversion defect.
        "tolerance_m": 0.0001,
        "tolerance_mps": 7.837e-08,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = out_dir / (golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("state_final_km (EarthFixed) =", body_fixed)
    print("state_final_km_mj2000eq (EarthMJ2000Eq) =", mj2000eq)

    script_path.unlink(missing_ok=True)
    report_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
