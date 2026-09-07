"""Generate `covariance_bodyfixed_leo_2h.json`: GMAT's own frame-converted `OrbitErrorCovariance`
report, read through a genuine GMAT script + `ReportFile`, for the identical LEO arc
`gen_bodyfixed_leo_2h.py`/`gen_icrf_leo_2h.py` already pin (M21.4, `docs/open-questions.md`
question 138, ADR-002's fourth amendment's "Amendment 2026-09-04 (fourth)": "closing it needs
the rotation matrix from the same GMAT call applied as R P Rᵀ").

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_covariance_bodyfixed_leo_2h.py --reason "..."

**Why a script + `ReportFile`, not the object API.** Same documented quirk every other
`gen_*.py` in this directory already pins: a `ReportFile` driven through the GMAT *object* API's
`Execute()` does not reliably write output, and `SaveScript` segfaults. The working pattern is a
real GMAT *script*, run through `gmat.LoadScript` + `gmat.RunScript`, with `ReportFile`s injected
and `SolverIterations = Current` set on each.

**Two `ReportFile`s, not one.** GMAT's `OrbitErrorCovariance` is an `Rmatrix66` `Parameter`
(`third_party/gmat-src/src/base/parameter/OrbitCovarianceParameters.cpp`); `ReportFile::
WriteMatrix` (`third_party/gmat-src/src/base/subscriber/ReportFile.cpp`) writes one physical
output *line* per matrix *row* (`rmat.ToRowString(row, ...)`, space-separated within that one
CSV field) rather than flattening a matrix parameter into 36 scalar columns on one line -- mixing
it into `GoldenReport`'s existing 13 scalar columns (`A1ModJulian`, `EarthFixed`/`EarthMJ2000Eq`
`X..VZ`, one physical line per step) would leave every scalar column blank on 5 of every 6
physical lines (`ReportFile::WriteData`'s own `numRow >= row+1` gate). `GoldenCovReport` instead
carries *only* the two `OrbitErrorCovariance` columns, so every one of its lines is a genuine
6-column matrix row for both frames at once -- the last 6 physical lines are exactly the final
step's `EarthMJ2000Eq` (column 0) and `EarthFixed` (column 1) covariance, row by row.

**What this golden is -- and is not -- ground truth for.** `OrbitData::GetCovarianceRmat66`
(`third_party/gmat-src/src/base/parameter/OrbitData.cpp`) converts a Cartesian covariance to a
different `CoordinateSystem` with a 6x6 `transform` built *only* from `CoordinateConverter::
GetLastRotationMatrix()` -- block-diagonal `[[R,0],[0,R]]` -- and never calls `GetLastRotationDot
Matrix()` at all. That is the correct Jacobian for two frames that share no relative angular
velocity (e.g. `EarthMJ2000Eq` -> `EarthICRF`, a fixed frame-bias rotation, `Rdot = 0`
identically), but `EarthFixed` rotates with Earth (`Rdot != 0`), so GMAT's own report through
this Parameter is NOT the physically correct answer for a rotating target frame -- seeing this
first hand (rather than assuming it) is exactly why this script exists: `cov_bodyfixed_gmat_
report_km`'s position block (rows/cols 0-2) matches `R P R^T` built from `gmatffi_convert_state_
and_rotation`'s own `rotation` output (the position block never involves `Rdot`), while its
velocity and position-velocity cross blocks do NOT match the full `[[R,0],[Rdot,R]]` transform
-- see `crates/gmat-sys/tests/convert_rotation.rs` for the measured numbers and the resulting
pin decision (this repository's own M21.4 report has the full account: which of the REQUIRED
PIN's two options -- "GMAT's own report" vs. "R P Rᵀ built from GMAT's reported rotation" -- was
used, and why). This golden's own `cov_mj2000eq_report_km` (the *same* Parameter, reported back
in the covariance's own declared frame, where `mInternalCS == mParameterCS` short-circuits any
conversion at all -- `OrbitData::GetCovarianceRmat66`'s own early-return branch) is a pure
identity sanity check: it must equal `p0_km` exactly, proving the declared covariance round-trips
through GMAT's own storage/report path unchanged.

Same vehicle/orbit/force-model/epoch/duration/`Earth.NutationUpdateInterval = 0` as
`gen_bodyfixed_leo_2h.py` -- "the same arc", so the state/epoch this golden's own `report_last_
row` carries is directly comparable to `bodyfixed_leo_2h.json`'s.

**Two GMAT scripting quirks found empirically while building this script** (both confirmed by
bisection against a minimal script, not assumed): (1) `OrbitData::GetCovarianceRmat66` throws
"Coordinate conversions may only be performed on Cartesian Covariance matrices" the moment it
needs to actually convert (`mInternalCS != mParameterCS`) unless the spacecraft's own
`DisplayStateType` is literally `Cartesian` at that instant -- `Golden` is still constructed with
`DisplayStateType = Keplerian` (so `SMA`/`ECC`/... are accepted as the initial-state fields,
exactly every other golden in this directory), then switched to `Cartesian` inside the mission
sequence, after construction, before `GoldenCovReport`'s first sample -- a display-convention
flip, not a re-specification of the physical state. (2) a full 2-D matrix literal with
semicolons (`Golden.OrbitErrorCovariance = [a 0 0; 0 b 0; ...]`) parses fine as a resource-time
(pre-`BeginMissionSequence`) field assignment but is refused by GMAT's own parser as a
mission-sequence Assignment command; `diag([...])` (a 1-D vector) is accepted in both places, so
the (diagonal-only, sufficient here) `diag(...)` form is used inside the mission sequence,
*after* the `DisplayStateType` switch above.
"""
import argparse, csv, datetime, json, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Copied verbatim from gen_bodyfixed_leo_2h.py / gen_icrf_leo_2h.py / gen_leo_1day.py --
# "the same arc" every frame-conversion golden in this directory pins.
EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {"SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
FORCE_MODEL_GRAVITY_FILE = "JGM2.cof"
FORCE_MODEL_GRAVITY_DEGREE = 8
FORCE_MODEL_GRAVITY_ORDER = 8
FORCE_MODEL_POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 7200.0  # same arc as bodyfixed_leo_2h.json / icrf_leo_2h.json

# Declared initial covariance (GMAT native km / km-s units), diagonal but deliberately
# ANISOTROPIC per axis -- (100, 150, 200 m)^2 position variance, (0.1, 0.15, 0.2 m/s)^2
# velocity variance. An isotropic P0 (same variance on all three position/velocity diagonal
# entries) is a poor discriminating fixture here: R * (c*I) * R^T = c*I for ANY orthogonal R,
# so an isotropic position (or velocity) block would rotate to the identical matrix whether R
# is correct, wrong, or the identity -- it cannot show a rotation actually happened.
# Anisotropic per-axis variances make the position/velocity blocks rotation-sensitive; this
# script's own `max_identity_check_abs_diff` (GMAT's own report of the covariance back in its
# declared EarthMJ2000Eq) and `crates/gmat-sys/tests/convert_rotation.rs`'s position-block
# comparison both exercise a genuine, non-trivial rotation as a result.
P0_POS_VAR_KM2 = [0.01, 0.0225, 0.04]  # (100 m)^2, (150 m)^2, (200 m)^2
P0_VEL_VAR_KM2_S2 = [1.0e-8, 2.25e-8, 4.0e-8]  # (0.1 m/s)^2, (0.15 m/s)^2, (0.2 m/s)^2

SCRIPT_TEMPLATE = """\
%----------------------------------------------------------------------------
% M21.4 ground truth: GMAT's own OrbitErrorCovariance report, converted to EarthFixed,
% for goldens/covariance_bodyfixed_leo_2h.json
%----------------------------------------------------------------------------

% Same review note as gen_bodyfixed_leo_2h.py: force Earth to recompute its nutation every
% call instead of caching for the default 60 s, so this ReportFile evaluates BodyFixedAxes
% (and its own rotation-rate) at the same instant a cold gmat_sys::Gmat::convert_with_rotation
% call does.
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
Golden.Id = 'Golden';

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

Create ReportFile GoldenCovReport;
GoldenCovReport.Filename = '{cov_report_path}';
GoldenCovReport.Precision = 16;
GoldenCovReport.WriteHeaders = false;
GoldenCovReport.LeftJustify = On;
GoldenCovReport.ZeroFill = Off;
GoldenCovReport.FixedWidth = false;
GoldenCovReport.Delimiter = ',';
GoldenCovReport.WriteReport = true;
GoldenCovReport.SolverIterations = Current;
GoldenCovReport.Add = {{Golden.EarthMJ2000Eq.OrbitErrorCovariance, Golden.EarthFixed.OrbitErrorCovariance}};

BeginMissionSequence;

% Empirically required (found by bisection while building this script, recorded here so a
% future edit does not silently drop it): OrbitData::GetCovarianceRmat66
% (third_party/gmat-src/src/base/parameter/OrbitData.cpp) throws "Coordinate conversions may
% only be performed on Cartesian Covariance matrices" whenever it needs to actually convert
% (mInternalCS != mParameterCS, i.e. the EarthFixed report below) unless the spacecraft's own
% DisplayStateType is literally "Cartesian" at that instant -- constructing Golden with
% DisplayStateType = Keplerian above (needed to accept SMA/ECC/... as the initial-state fields,
% exactly every other golden in this directory) and switching it here, in the mission sequence,
% after construction, keeps the already-established Keplerian initial state unchanged (this is
% a display-convention flip, not a re-specification of the state) while satisfying that check
% before GoldenCovReport's first EarthFixed sample is written.
Golden.DisplayStateType = Cartesian;
% A full 2-D matrix literal with semicolons (`[a 0 0; 0 b 0; ...]`) is only accepted as a
% resource-time (pre-BeginMissionSequence) field assignment -- as a mission-sequence Assignment
% command it fails GMAT's own script parser (confirmed empirically); `diag(vector)` is accepted
% in both places, so it is used here instead. This only produces a diagonal P0 (sufficient for
% this golden -- see the anisotropic-variance comment above).
Golden.OrbitErrorCovariance = diag([{p0pos0} {p0pos1} {p0pos2} {p0vel0} {p0vel1} {p0vel2}]);

Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {duration_s}}};
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    args = ap.parse_args()

    out_dir = Path(__file__).parent
    script_path = out_dir / "_gen_covariance_bodyfixed_leo_2h.script"
    report_path = out_dir / "_gen_covariance_bodyfixed_leo_2h.rpt"
    cov_report_path = out_dir / "_gen_covariance_bodyfixed_leo_2h_cov.rpt"

    script_text = SCRIPT_TEMPLATE.format(
        epoch=EPOCH, sma=SAT["SMA"], ecc=SAT["ECC"], inc=SAT["INC"], raan=SAT["RAAN"], aop=SAT["AOP"], ta=SAT["TA"],
        dry_mass=SAT["DryMass"], cd=SAT["Cd"], cr=SAT["Cr"], drag_area=SAT["DragArea"], srp_area=SAT["SRPArea"],
        point_masses=", ".join(FORCE_MODEL_POINT_MASSES), gravity_file=FORCE_MODEL_GRAVITY_FILE,
        gravity_degree=FORCE_MODEL_GRAVITY_DEGREE, gravity_order=FORCE_MODEL_GRAVITY_ORDER,
        integrator=PROP["integrator"], initial_step_s=PROP["initial_step_s"], accuracy=PROP["accuracy"],
        min_step_s=PROP["min_step_s"], max_step_s=PROP["max_step_s"], report_path=str(report_path),
        cov_report_path=str(cov_report_path), duration_s=DURATION_S,
        p0pos0=repr(P0_POS_VAR_KM2[0]), p0pos1=repr(P0_POS_VAR_KM2[1]), p0pos2=repr(P0_POS_VAR_KM2[2]),
        p0vel0=repr(P0_VEL_VAR_KM2_S2[0]), p0vel1=repr(P0_VEL_VAR_KM2_S2[1]), p0vel2=repr(P0_VEL_VAR_KM2_S2[2]),
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
    body_fixed_state = last[1:7]
    mj2000eq_state = last[7:13]

    if not cov_report_path.exists():
        raise RuntimeError(f"GMAT did not write a ReportFile at {cov_report_path}")
    with open(cov_report_path, newline="") as fh:
        cov_rows = list(csv.reader(fh))
    if len(cov_rows) < 6:
        raise RuntimeError(f"ReportFile {cov_report_path} has fewer than 6 lines: {len(cov_rows)}")
    last_block = cov_rows[-6:]
    p0_mj2000eq_km = []
    cov_bodyfixed_gmat_report_km = []
    for line in last_block:
        if len(line) != 2:
            raise RuntimeError(f"expected 2 matrix columns (MJ2000Eq, EarthFixed) per covariance report line, got {len(line)}: {line}")
        mj_row = [float(v) for v in line[0].split()]
        bf_row = [float(v) for v in line[1].split()]
        if len(mj_row) != 6 or len(bf_row) != 6:
            raise RuntimeError(f"expected 6 elements per matrix row, got {len(mj_row)}/{len(bf_row)}: {line}")
        p0_mj2000eq_km.extend(mj_row)
        cov_bodyfixed_gmat_report_km.extend(bf_row)

    p0_diag = P0_POS_VAR_KM2 + P0_VEL_VAR_KM2_S2
    p0_km = [0.0] * 36
    for i in range(6):
        p0_km[i * 6 + i] = p0_diag[i]

    # Identity sanity check: GMAT's own report of the covariance back in its own declared frame
    # (EarthMJ2000Eq) must equal the declared P0 exactly (OrbitData::GetCovarianceRmat66's
    # mInternalCS == mParameterCS early-return branch, no conversion at all).
    max_identity_err = max(abs(a - b) for a, b in zip(p0_mj2000eq_km, p0_km))
    if max_identity_err > 1e-12:
        raise RuntimeError(f"GMAT's own EarthMJ2000Eq covariance report does not match the declared P0 (max abs diff {max_identity_err:e}) -- something about this script's Add list or matrix literal is wrong")

    golden = {
        "name": "covariance_bodyfixed_leo_2h",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "note": (
            "GMAT's own OrbitErrorCovariance Parameter, reported in EarthFixed (GMAT's built-in "
            "Earth body-fixed CoordinateSystem) and, as an identity sanity check, back in its own "
            "declared EarthMJ2000Eq -- read from a genuine GMAT ReportFile (script run, "
            "SolverIterations=Current). Same vehicle/orbit/force-model/epoch/duration as "
            "bodyfixed_leo_2h.json. See this file's own module doc comment "
            "(goldens/gen_covariance_bodyfixed_leo_2h.py) for why GMAT's own EarthFixed report "
            "is NOT full ground truth for a correct (Rdot-including) covariance rotation: "
            "OrbitData::GetCovarianceRmat66 builds only the block-diagonal [[R,0],[0,R]] "
            "transform, never calling CoordinateConverter::GetLastRotationDotMatrix() at all, "
            "which is only exact for a non-rotating target frame (e.g. EarthICRF) and NOT for "
            "EarthFixed, which rotates with Earth. crates/gmat-sys/tests/convert_rotation.rs "
            "measures the resulting position-block agreement / velocity-and-cross-block "
            "disagreement directly and this repository's M21.4 report explains the resulting "
            "pin decision."
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
            "state_bodyfixed_km": body_fixed_state,
            "state_mj2000eq_km": mj2000eq_state,
        },
        "p0_km": {
            "note": "declared diagonal P0 in EarthMJ2000Eq, GMAT-native km/km-s units: anisotropic (100, 150, 200 m)^2 position variance, (0.1, 0.15, 0.2 m/s)^2 velocity variance per axis (deliberately not isotropic -- see this script's own module doc comment for why), same physical covariance scale as leo_1day_jgm2_8x8_sunmoon.json's stm.p0_si.",
            "row_major_6x6": p0_km,
        },
        "cov_mj2000eq_report_km_row_major_6x6": p0_mj2000eq_km,
        "cov_bodyfixed_gmat_report_km_row_major_6x6": cov_bodyfixed_gmat_report_km,
        "max_identity_check_abs_diff": max_identity_err,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = out_dir / (golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("epoch_a1mjd =", a1mjd)
    print("state_bodyfixed_km =", body_fixed_state)
    print("identity check max abs diff =", max_identity_err)

    script_path.unlink(missing_ok=True)
    report_path.unlink(missing_ok=True)
    cov_report_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
