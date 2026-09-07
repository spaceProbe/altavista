"""Generate `gmat_port_cd_command.json`: the GMAT-script reference for M18.3's headline test
(`docs/open-questions.md` question 126) -- a SIGNAL-commanded `Cd` change mid-run, on a force
model that actually includes drag, so the command has a genuine, measurable effect on the
propagated arc. Pinned against `crates/gmat-sys/tests/gmat_port_cd_command.rs`, which drives
`gmat_sys::model::GmatModel::step_with_ports` (M18.3's own new override) through the identical
two-stage propagation and Cd change, never GMAT's own `Propagator` object.

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_gmat_port_cd_command.py --reason "..."

**Why a script + `ReportFile`, not the object API** -- same reason `goldens/
gen_leo_1day_rmag.py` gives (this repository's documented `ReportFile`-through-`Execute()`
quirk): a real GMAT script, run through `gmat.LoadScript`/`gmat.RunScript`, with a `ReportFile`
injected and `SolverIterations = Current` set on it, never `Execute()` through the object API
(writes nothing) and never `gmat.SaveScript` (segfaults -- both documented in this repository's
own memory notes and `crates/gmat-sys/README.md`).

**Why drag at all.** `drms/demo_two_instance.system.yaml`'s own force model (JGM2 8x8 + Sun/Moon
point masses only) has no atmospheric drag, so a `Cd` change there is physically inert -- exactly
the trap M18.3's own brief calls out. This fixture is a *separate*, deliberately drag-inclusive
system (`DragForce` + `JacchiaRoberts` atmosphere, the same choice and orbit altitude
`goldens/gen_leo_1day.py --drag-srp` already uses for `leo_1day_jgm2_8x8_sunmoon_drag_srp.json`,
minus `SolarRadiationPressure` -- SRP does not depend on `Cd` at all, so it is left out here to
keep the fixture focused on the one force `Cd` actually scales) so a `Cd` change has a real,
non-zero effect to measure.

**The mid-run command.** Two `Propagate` statements of 3600 s each (`DURATION_S / 2`), with a
plain script assignment `Golden.Cd = {cd_after}` in between -- the script-engine's own analogue
of what `gmat_sys::model::GmatModel::step_with_ports` does when it applies a consumed SIGNAL
value via `gmat_sys::DerivativeModel::set_real_parameter` before its own second `step` call.
`ReportFile.Add` includes `Golden.Cd` itself, so the Rust side can locate the exact transition
row (the last row reporting `cd_before` immediately followed by one reporting `cd_after`) without
assuming anything about the script engine's own internal reporting cadence.
"""
import argparse, csv, datetime, json, math, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Same ~250 km-altitude LEO `goldens/gen_leo_1day.py --drag-srp` uses (SMA 6628 km vs. the plain
# golden's 6878 km), low enough that atmospheric density -- and hence a Cd change -- moves the
# arc measurably over a couple of hours. Same inclination/spacecraft ballistic parameters.
EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {"SMA": 6628.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
CD_BEFORE = 2.2
CD_AFTER = 4.4
FORCE_MODEL_GRAVITY_FILE = "JGM2.cof"
FORCE_MODEL_GRAVITY_DEGREE = 8
FORCE_MODEL_GRAVITY_ORDER = 8
FORCE_MODEL_POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 300.0}
DURATION_S = 7200.0
COMMAND_AT_S = DURATION_S / 2.0

SCRIPT_TEMPLATE = """\
%----------------------------------------------------------------------------
% M18.3 ground truth: a mid-run Cd change ({cd_before} -> {cd_after} at t={command_at_s}s) on a
% drag-inclusive force model, for gmat_port_cd_command.json / gmat-sys/tests/gmat_port_cd_command.rs
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
Golden.Cd = {cd_before};
Golden.Cr = {cr};
Golden.DragArea = {drag_area};
Golden.SRPArea = {srp_area};

Create ForceModel GoldenFM;
GoldenFM.CentralBody = Earth;
GoldenFM.PrimaryBodies = {{Earth}};
GoldenFM.PointMasses = {{{point_masses}}};
GoldenFM.Drag = JacchiaRoberts;
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
GoldenReport.Add = {{Golden.A1ModJulian, Golden.EarthMJ2000Eq.X, Golden.EarthMJ2000Eq.Y, Golden.EarthMJ2000Eq.Z, Golden.EarthMJ2000Eq.VX, Golden.EarthMJ2000Eq.VY, Golden.EarthMJ2000Eq.VZ, Golden.Earth.RMAG, Golden.Cd}};

BeginMissionSequence;

Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {command_at_s}}};
Golden.Cd = {cd_after};
Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {remaining_s}}};
"""


def _run_and_report(cd_after_value):
    """Run the two-stage script (Cd = CD_BEFORE, propagate half, Cd = cd_after_value, propagate
    the other half) and return every parsed ReportFile row. `cd_after_value == CD_BEFORE`
    reproduces the uncommanded counterfactual arc (same two-stage structure, but the "command"
    is a no-op) -- used to measure the baseline-vs-commanded difference independently of the
    Rust side, confirming the effect is real before it is ever compared against Rust at all.
    """
    out_dir = Path(__file__).parent
    tag = "cmd" if cd_after_value != CD_BEFORE else "nocmd"
    script_path = out_dir / f"_gen_gmat_port_cd_command_{tag}.script"
    report_path = out_dir / f"_gen_gmat_port_cd_command_{tag}.rpt"

    script_text = SCRIPT_TEMPLATE.format(
        epoch=EPOCH, sma=SAT["SMA"], ecc=SAT["ECC"], inc=SAT["INC"], raan=SAT["RAAN"], aop=SAT["AOP"], ta=SAT["TA"],
        dry_mass=SAT["DryMass"], cd_before=CD_BEFORE, cd_after=cd_after_value, cr=SAT["Cr"], drag_area=SAT["DragArea"], srp_area=SAT["SRPArea"],
        point_masses=", ".join(FORCE_MODEL_POINT_MASSES), gravity_file=FORCE_MODEL_GRAVITY_FILE,
        gravity_degree=FORCE_MODEL_GRAVITY_DEGREE, gravity_order=FORCE_MODEL_GRAVITY_ORDER,
        integrator=PROP["integrator"], initial_step_s=PROP["initial_step_s"], accuracy=PROP["accuracy"],
        min_step_s=PROP["min_step_s"], max_step_s=PROP["max_step_s"], report_path=str(report_path),
        command_at_s=COMMAND_AT_S, remaining_s=DURATION_S - COMMAND_AT_S,
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
        rows = [[float(v) for v in row] for row in csv.reader(fh) if row]
    if not rows:
        raise RuntimeError(f"ReportFile {report_path} is empty")
    for row in rows:
        if len(row) != 9:
            raise RuntimeError(f"expected 9 columns (A1ModJulian, X, Y, Z, VX, VY, VZ, RMAG, Cd), got {len(row)}: {row}")

    script_path.unlink(missing_ok=True)
    report_path.unlink(missing_ok=True)
    return rows


def _row_dict(row):
    a1mjd, x, y, z, vx, vy, vz, rmag, cd = row
    return {"epoch_a1mjd": a1mjd, "x_km": x, "y_km": y, "z_km": z, "vx_kmps": vx, "vy_kmps": vy, "vz_kmps": vz, "rmag_km": rmag, "cd": cd}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument("--tolerance-m", type=float, default=1.0, help="position tolerance the Rust test compares its own commanded arc against (see that test's own measured agreement)")
    args = ap.parse_args()

    commanded_rows = _run_and_report(CD_AFTER)
    baseline_rows = _run_and_report(CD_BEFORE)  # cd_after == cd_before: a structurally identical, un-commanded run

    # Locate the exact transition row in the commanded run: the last row still reporting
    # CD_BEFORE (the pre-command state, used as this fixture's own t=COMMAND_AT_S cross-check)
    # and the first row reporting CD_AFTER.
    last_before_idx = max(i for i, r in enumerate(commanded_rows) if r[8] == CD_BEFORE)
    first_after_idx = next(i for i, r in enumerate(commanded_rows) if r[8] == CD_AFTER)
    if first_after_idx != last_before_idx + 1:
        raise RuntimeError(f"expected the Cd transition to be one contiguous report row apart, got rows {last_before_idx} -> {first_after_idx}")

    commanded_final = commanded_rows[-1]
    baseline_final = baseline_rows[-1]

    def _pos_km(row):
        return row[1:4]

    def _dist_m(a, b):
        return math.sqrt(sum((1000.0 * (ai - bi)) ** 2 for ai, bi in zip(a, b)))

    arc_difference_m = _dist_m(_pos_km(commanded_final), _pos_km(baseline_final))

    golden = {
        "name": "gmat_port_cd_command",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "note": (
            "GMAT script + ReportFile (SolverIterations=Current, never Execute() through the "
            "object API) reference for M18.3 (docs/open-questions.md question 126): a "
            "SIGNAL-commanded Cd change (2.2 -> 4.4) applied mid-run through a drag-inclusive "
            "force model (JGM2 8x8 + Sun/Moon + JacchiaRoberts drag), pinning "
            "crates/gmat-sys/tests/gmat_port_cd_command.rs's own gmat_sys::model::GmatModel::"
            "step_with_ports-driven arc. 'baseline' is a structurally identical two-stage run "
            "whose second-stage Cd is left at 2.2 (a no-op 'command') -- its difference from "
            "'commanded' at duration_s is this fixture's own proof that Cd has a real, "
            "non-vacuous effect on this force model (unlike drms/demo_two_instance.system.yaml's "
            "own gravity+point-mass-only force model, deliberately)."
        ),
        "frame": "EarthMJ2000Eq", "units": {"position": "km", "velocity": "km/s", "rmag": "km"},
        "epoch_utc": EPOCH,
        "spacecraft": {**SAT, "Cd_before": CD_BEFORE, "Cd_after": CD_AFTER},
        "force_model": {"central_body": "Earth", "gravity_file": FORCE_MODEL_GRAVITY_FILE, "gravity_degree": FORCE_MODEL_GRAVITY_DEGREE,
                         "gravity_order": FORCE_MODEL_GRAVITY_ORDER, "point_masses": FORCE_MODEL_POINT_MASSES, "drag": "JacchiaRoberts"},
        "propagator": PROP,
        "duration_s": DURATION_S,
        "command_at_s": COMMAND_AT_S,
        "tolerance_m": args.tolerance_m,
        "commanded": {
            "pre_command_row": _row_dict(commanded_rows[last_before_idx]),
            "final_row": _row_dict(commanded_final),
        },
        "baseline_uncommanded": {
            "final_row": _row_dict(baseline_final),
        },
        "measured_arc_difference_commanded_vs_baseline_m": arc_difference_m,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("commanded final (km):", commanded_final[1:4], "baseline final (km):", baseline_final[1:4])
    print("measured arc difference (commanded vs. baseline; must be non-zero and physically sensible):", arc_difference_m, "m")


if __name__ == "__main__":
    main()
