"""Generate `ground_contact_gmat.json`: GMAT's own `ContactLocator` AOS/LOS report for a
Cape-Canaveral-like ground site against `leo_demo_sys`'s own orbit (`drms/demo_two_instance.
system.yaml`'s vehicle, itself `goldens/gen_leo_1day.py`'s own SAT/force model), plus a dense,
regularly-sampled `Golden.EarthFixed.{X,Y,Z}` `ReportFile` over the identical arc -- the
acceptance pin for `crate::drm::ground::contact_windows`/`elevation_of` (M25.1, `docs/sil-plan.md`'s
M25 milestone: "Contact windows pinned against GMAT's own contact locator for the same site and
arc").

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_ground_contact_gmat.py --reason "..."

**Why a script + `ReportFile`/`ContactLocator`, not the object API.** The same documented quirk
`goldens/gen_bodyfixed_leo_2h.py`/`gen_leo_1day_rmag.py` already pin: a `ReportFile` driven
through the GMAT *object* API's `Execute()` does not reliably write output, and `SaveScript`
segfaults (`gmat-api-quirks.md`). `ContactLocator` is not itself driven by a solver loop the
`SolverIterations = Current` quirk applies to (`RunMode = Automatic` auto-runs `FindEvents` once
the mission sequence's own `Propagate` commands finish), but the *reused*, safe pattern is
identical: one script text, run through `gmat.LoadScript`/`gmat.RunScript`, output files read
from disk afterward -- never the object API.

**Earth shape pinned to this repository's own WGS84 constants.** `ContactLocator` computes
elevation against `GroundStation.HorizonReference = Ellipsoid`'s own `Earth.EquatorialRadius`/
`.Flattening` -- explicitly set here to the *exact* WGS84 values `crate::drm::ground::
{WGS84_A_M, WGS84_F}` uses (`6378.137` km, `1/298.257223563`), not GMAT's own slightly different
built-in default (`6378.1363` km) -- so a disagreement in the comparison test can only be this
task's own geometry, never a mismatched reference ellipsoid between the two implementations.

**Light time and stellar aberration disabled** (`UseLightTimeDelay`/`UseStellarAberration =
false`) -- `crate::drm::ground::elevation_of` does neither correction (an instantaneous,
non-relativistic topocentric transform, matching every other native model's own "GMAT-free,
cheap" contract), so this keeps the comparison purely geometric rather than introducing a real,
expected (sub-millisecond light time at LEO range) discrepancy this task's own tolerance would
otherwise have to absorb for a reason that has nothing to do with the geometry being pinned.

**Same vehicle/orbit/force-model/epoch as `drms/demo_two_instance.system.yaml`'s `leo_demo_sys`**
-- reused rather than a new orbit invented for this golden, exactly `gen_bodyfixed_leo_2h.py`'s
own "same vehicle" convention. Arc shortened to 3 hours (not the 1-day/2-hour arcs those goldens
use) -- long enough for two full passes over the declared site at this orbit's own ~93-minute
period, short enough to keep the dense position `ReportFile` (30 s cadence, 361 rows) a
reasonably sized golden file.
"""
import argparse, csv, datetime, json, os, sys
from pathlib import Path

GMAT_BIN = "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

# Copied verbatim from goldens/gen_leo_1day.py / drms/demo_two_instance.system.yaml's
# leo_demo_sys -- "the same vehicle" this golden's own header comment states.
EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {"SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
FORCE_MODEL_GRAVITY_FILE = "JGM2.cof"
FORCE_MODEL_GRAVITY_DEGREE = 8
FORCE_MODEL_GRAVITY_ORDER = 8
FORCE_MODEL_POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}

# Cape-Canaveral-like site, matching drms/demo_ground_segment_ground.system.yaml exactly.
SITE = {"lat_deg": 28.5, "lon_deg": -80.6, "height_m": 0.0}
ELEVATION_MASK_DEG = 10.0
WGS84_EQUATORIAL_RADIUS_KM = 6378.137
WGS84_FLATTENING = 1.0 / 298.257223563

DURATION_S = 10800.0  # 3 hours: ~2 full passes at this orbit's ~93-minute period.
STEP_S = 30.0
N_STEPS = int(DURATION_S / STEP_S)  # 360, so N_STEPS+1 = 361 rows including t=0

SCRIPT_TEMPLATE = """\
%----------------------------------------------------------------------------
% M25.1 acceptance pin: GMAT's own ContactLocator AOS/LOS report, plus a dense
% Golden.EarthFixed position series, for goldens/ground_contact_gmat.json
%----------------------------------------------------------------------------

Earth.NutationUpdateInterval = 0;
Earth.EquatorialRadius = {earth_eq_radius_km};
Earth.Flattening = {earth_flattening};

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

Create GroundStation GS;
GS.CentralBody = Earth;
GS.StateType = Spherical;
GS.HorizonReference = Ellipsoid;
GS.Location1 = {site_lat_deg};
GS.Location2 = {site_lon_deg};
GS.Location3 = {site_height_km};
GS.MinimumElevationAngle = {elevation_mask_deg};

Create ContactLocator CL;
CL.Target = Golden;
CL.Observers = {{GS}};
CL.Filename = '{contact_report_path}';
CL.RunMode = Automatic;
CL.UseEntireInterval = true;
CL.UseLightTimeDelay = false;
CL.UseStellarAberration = false;
CL.WriteReport = true;

Create ReportFile GoldenReport;
GoldenReport.Filename = '{position_report_path}';
GoldenReport.Precision = 16;
GoldenReport.WriteHeaders = false;
GoldenReport.LeftJustify = On;
GoldenReport.ZeroFill = Off;
GoldenReport.FixedWidth = false;
GoldenReport.Delimiter = ',';
GoldenReport.WriteReport = true;
GoldenReport.SolverIterations = Current;
GoldenReport.Add = {{Golden.A1ModJulian, Golden.EarthFixed.X, Golden.EarthFixed.Y, Golden.EarthFixed.Z}};

Create Variable gsI;

BeginMissionSequence;

For gsI = 1:1:{n_steps}
   Propagate GoldenProp(Golden) {{Golden.ElapsedSecs = {step_s}}};
EndFor;
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    args = ap.parse_args()

    out_dir = Path(__file__).parent
    script_path = out_dir / "_gen_ground_contact_gmat.script"
    contact_report_path = out_dir / "_gen_ground_contact_gmat_contact.txt"
    position_report_path = out_dir / "_gen_ground_contact_gmat_positions.rpt"

    script_text = SCRIPT_TEMPLATE.format(
        earth_eq_radius_km=WGS84_EQUATORIAL_RADIUS_KM, earth_flattening=WGS84_FLATTENING,
        epoch=EPOCH, sma=SAT["SMA"], ecc=SAT["ECC"], inc=SAT["INC"], raan=SAT["RAAN"], aop=SAT["AOP"], ta=SAT["TA"],
        dry_mass=SAT["DryMass"], cd=SAT["Cd"], cr=SAT["Cr"], drag_area=SAT["DragArea"], srp_area=SAT["SRPArea"],
        point_masses=", ".join(FORCE_MODEL_POINT_MASSES), gravity_file=FORCE_MODEL_GRAVITY_FILE,
        gravity_degree=FORCE_MODEL_GRAVITY_DEGREE, gravity_order=FORCE_MODEL_GRAVITY_ORDER,
        integrator=PROP["integrator"], initial_step_s=PROP["initial_step_s"], accuracy=PROP["accuracy"],
        min_step_s=PROP["min_step_s"], max_step_s=PROP["max_step_s"],
        site_lat_deg=SITE["lat_deg"], site_lon_deg=SITE["lon_deg"], site_height_km=SITE["height_m"] / 1000.0,
        elevation_mask_deg=ELEVATION_MASK_DEG,
        contact_report_path=str(contact_report_path), position_report_path=str(position_report_path),
        n_steps=N_STEPS, step_s=STEP_S,
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

    if not position_report_path.exists():
        raise RuntimeError(f"GMAT did not write the position ReportFile at {position_report_path}")
    with open(position_report_path, newline="") as fh:
        position_rows = list(csv.reader(fh))
    if len(position_rows) != N_STEPS + 1:
        raise RuntimeError(f"expected {N_STEPS + 1} position rows (t=0..{DURATION_S}s at {STEP_S}s), got {len(position_rows)}")
    epoch0_a1mjd = float(position_rows[0][0])
    samples = []
    for row in position_rows:
        vals = [float(v) for v in row]
        t_s = (vals[0] - epoch0_a1mjd) * 86400.0  # A1ModJulian is in days; back out elapsed seconds.
        samples.append({"t_s": t_s, "x_km": vals[1], "y_km": vals[2], "z_km": vals[3]})

    # The mission epoch (t=0), converted through the *same* Gregorian->MJD function the contact
    # window boundaries below are converted through -- self-consistent regardless of which
    # absolute time system that function actually returns (A1 vs UTC MJD), since any fixed
    # offset between systems cancels out of an (epoch_b - epoch_a) elapsed-time difference.
    # Deliberately NOT the ReportFile's own `Golden.A1ModJulian` column (a different, GMAT-
    # internal conversion path) -- mixing the two independently-computed epoch references is
    # exactly what produced a spurious ~37 s (one leap-second-table's worth) offset in an
    # earlier iteration of this script; see this script's own git history / task report for the
    # measurement that caught it.
    tsc = gmat.TimeSystemConverter.Instance()
    epoch0_mjd = tsc.ConvertGregorianToMjd(EPOCH)

    if not contact_report_path.exists():
        raise RuntimeError(f"GMAT did not write the ContactLocator report at {contact_report_path}")
    contact_text = contact_report_path.read_text()
    # Legacy ContactLocator report format: "Start Time (UTC)  Stop Time (UTC)  Duration (s)" rows
    # under a "Target: ... Observer: ..." header -- parse the two Gregorian timestamps per row and
    # the trailing duration; convert each timestamp to elapsed seconds since EPOCH via GMAT's own
    # time system so this script, not the Rust comparison test, owns the one UTCGregorian parse.
    windows = []
    for line in contact_text.splitlines():
        line = line.strip()
        if not line or line.startswith("Target") or line.startswith("Observer") or line.startswith("Start Time") or line.startswith("Coverage") or line.startswith("Number of"):
            continue
        # Row shape: "01 Jan 2026 00:12:34.567    01 Jan 2026 00:24:56.789      742.222"
        parts = line.rsplit(None, 1)
        if len(parts) != 2:
            continue
        try:
            duration_s = float(parts[1])
        except ValueError:
            continue
        times_part = parts[0]
        # Each Gregorian timestamp is "DD Mon YYYY HH:MM:SS.sss" -- 4 space-separated tokens.
        tokens = times_part.split()
        if len(tokens) != 8:
            continue
        start_str = " ".join(tokens[0:4])
        stop_str = " ".join(tokens[4:8])
        start_mjd = tsc.ConvertGregorianToMjd(start_str)
        stop_mjd = tsc.ConvertGregorianToMjd(stop_str)
        start_s = (start_mjd - epoch0_mjd) * 86400.0
        stop_s = (stop_mjd - epoch0_mjd) * 86400.0
        windows.append({"start_s": start_s, "stop_s": stop_s, "duration_s": duration_s})
    if not windows:
        raise RuntimeError(f"parsed zero contact windows from {contact_report_path} -- report format may have changed; raw text:\n{contact_text}")

    golden = {
        "name": "ground_contact_gmat",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "note": (
            "GMAT's own ContactLocator AOS/LOS windows (Legacy report format) for the declared "
            "Cape-Canaveral-like GroundStation against leo_demo_sys's own orbit, plus a dense "
            "(30 s cadence) Golden.EarthFixed.{X,Y,Z} position series over the identical arc. "
            "Earth.EquatorialRadius/.Flattening pinned to this repository's own WGS84 constants "
            "(crate::drm::ground::{WGS84_A_M, WGS84_F}); UseLightTimeDelay/UseStellarAberration "
            "disabled so the comparison is purely geometric. crates/av-kernel/tests/"
            "ground_contact_gmat.rs feeds the position series through crate::drm::ground::"
            "elevation_of + contact_windows and compares the resulting windows against these "
            "GMAT-reported ones."
        ),
        "site": {"lat_deg": SITE["lat_deg"], "lon_deg": SITE["lon_deg"], "height_m": SITE["height_m"]},
        "elevation_mask_deg": ELEVATION_MASK_DEG,
        "epoch_utc": EPOCH,
        "keplerian": SAT,
        "force_model": {"central_body": "Earth", "gravity": {"file": FORCE_MODEL_GRAVITY_FILE, "degree": FORCE_MODEL_GRAVITY_DEGREE, "order": FORCE_MODEL_GRAVITY_ORDER}, "point_masses": FORCE_MODEL_POINT_MASSES},
        "propagator": PROP,
        "duration_s": DURATION_S,
        "step_s": STEP_S,
        "units": {"position": "km", "time": "s (elapsed since epoch_utc)"},
        "position_series": samples,
        "gmat_contact_windows_s": windows,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    out = out_dir / (golden["name"] + ".json")
    out.write_text(body + "\n")
    print("wrote", out)
    print("gmat_contact_windows_s =", windows)

    script_path.unlink(missing_ok=True)
    contact_report_path.unlink(missing_ok=True)
    position_report_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
