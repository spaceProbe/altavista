"""Generate the golden arc `leo_1day_jgm2_8x8_mars_jupiter.json` with GMAT's own propagator
(docs/native-dynamics-plan.md milestone N2, "N2's remaining golden -- Mars and Jupiter as
third bodies").

Run explicitly, never from a test:

  .venv/bin/python goldens/gen_leo_1day_jgm2_8x8_mars_jupiter.py --reason "..." --tolerance-m <measured>

**Why this golden exists, and why Mars and Jupiter specifically.** N2's own Sun/Moon golden
(`leo_1day_jgm2_8x8_sunmoon.json`, the P0 arc) already pins `crate::de::DeEphemeris` against
GMAT for the Sun and the Moon -- but the Moon's own DE record is GEOCENTRIC (already relative
to Earth) and the Sun, while barycentric, sits at the far end of the same Earth/Moon-barycentre
split every other body uses. Mars and Jupiter are deliberately NOT Sun/Moon: they exercise the
reader's BARYCENTRIC records specifically (`DeBody::Mars`, `DeBody::Jupiter`, read directly from
`IPT` entries 3 and 4, then converted barycentric -> geocentric via the EMRAT split documented in
`crates/av-orbital/src/de.rs`'s own module doc) -- a code path the Sun/Moon golden cannot
exercise on its own, since the Sun's barycentric-to-geocentric conversion shares the exact same
Earth-position term the Moon's own (trivial, direct) record never touches, and Mars/Jupiter add
nothing else in common with Sun/Moon that could mask a planet-specific indexing bug (wrong `IPT`
row, wrong `nsub`, wrong `GM4`/`GM5` constant lookup).

Same arc shape as `leo_1day_jgm2_8x8.json` (N1's own gravity-only golden): epoch
"01 Jan 2026 00:00:00.000", SMA 6878.0, ECC 0.001, INC 51.6, RAAN 30.0, AOP 0.0, TA 0.0;
DryMass 500, Cd 2.2, Cr 1.8, DragArea 5, SRPArea 5; JGM2 8x8 central-body Earth gravity;
PrinceDormand78 at Accuracy 1e-13, MaxStep 600; one day (86,400 s). The ONLY force-model
difference from `leo_1day_jgm2_8x8.json` is two `PointMassForce` objects, `BodyName = "Mars"`
and `BodyName = "Jupiter"` -- deliberately not Sun and Moon, which the P0 golden already pins.

Every propagator and force-model field below is READ BACK off the live GMAT objects after
`PrepareInternals()`, not echoed from what this script requested -- `gen_leo_1day_jgm2_8x8.py`'s
own module doc names the measured wrinkle (`propagator_readback` must come from
`prop.GetPropagator()`'s return value, not the front-end `Propagator` or the bare integrator
object) that this script follows identically.

**Round 2 (task 2b) addendum.** `--tolerance-ephemeris-abs-m`/`--tolerance-ephemeris-rel` were
added when the round-2 root-cause task moved the ten-epoch ephemeris check's own tolerance out
of `crates/av-orbital/tests/thirdbody_mars_jupiter.rs`'s `const TOLERANCE_ABS_M`/`TOLERANCE_REL`
into this file (round 1's decision 3, applied here for the first time to this particular
check): that test's own `ten_epoch_ephemeris_agreement_with_gmat_reported_positions` now reads
`tolerance_ephemeris_abs_m`/`tolerance_ephemeris_rel` from this golden instead. Same rule as
`--tolerance-m`: no default, measure the test's own printed value first.

**The tolerance is measured, not assumed (`--tolerance-m` has no default -- deliberately, unlike
this repository's OLDER generators, which inherited a 0.05 m default from `gen_leo_1day.py` and
were later found, in round 1's own review, to have pinned nothing: "A default tolerance pins
nothing" is round 1's own lesson, restated verbatim in this task's brief).** The procedure
actually followed: (1) generate this golden with a placeholder tolerance far above anything
plausible so the file exists and the test can run at all; (2) run
`crates/av-orbital/tests/thirdbody_mars_jupiter.rs`'s trajectory-residual test and read its
printed measured residual; (3) re-run this script with `--tolerance-m` set to that measured
residual plus a stated margin, so the file committed to the repository carries the tolerance
actually in force, never a copy or a guess. See this script's own printed output, and the
worker's report, for the exact measured value and the regeneration command used.

**The ten-epoch ephemeris check (N2's own explicit ask: "ephemeris positions against GMAT's
reported values at ten epochs").** At ten epochs evenly spread across the arc (t0, t0+1/9*86400s,
..., t0+86400s), this script records GMAT's own reported position of Mars and Jupiter relative
to Earth, in `EarthMJ2000Eq`, in km.

*Route used, and why (measured, not assumed -- see this script's `note` field, copied into the
golden itself).* The task's suggested primary route, `gmat.GetObject("Mars")` (or
`GetRuntimeObject`) followed by `.GetMJ2000Position(gmat.A1Mjd(a1_mjd))`, was tried FIRST and
raised a GMAT `APIException` ("ArrayTemplate error : out-of-bounds") for EVERY body tried (Mars,
Jupiter, Earth, Sun, Luna alike -- so the failure is not body-specific), constructed exactly as
this repository's own generators construct objects (`gmat.Construct`, `gmat.Initialize()`,
`prop.PrepareInternals()`, no `gmat.Execute()`/Sandbox mission-sequence run). The likely cause:
`GetMJ2000Position` is served by each `CelestialBody`'s own ephemeris-file connection, which
appears to be wired up only inside a full Sandbox/`Execute()` mission run, not by the direct-API
construct-and-step path every golden generator in this repository otherwise uses (this was
probed directly, not guessed -- see the worker's report for the exact probe transcript).
Rather than switch this whole script over to a scripted Sandbox run (which every other golden
generator in this repository deliberately avoids), this script uses a route that is provably
equivalent and reuses machinery this platform already trusts: `gmat.CoordinateConverter().Convert`
-- the SAME primitive `crates/gmat-sys`'s own `Gmat::convert`/`Gmat::convert_with_rotation` calls
(ADR-002's fourth amendment) and that this platform's own frame conversions are already pinned
against. A `CoordinateSystem` is built per body exactly as `gmat_sys::Gmat::coordinate_system`
builds one (`Origin = <body>`, `Axes` = a freshly constructed `MJ2000Eq` axis object bound via
`SetReference`, never `SetField("Axes", ...)`, which GMAT's API refuses to initialize) and the
null state `[0,0,0,0,0,0]` in that body's own frame -- i.e. the body's own position in its own
frame, trivially the origin -- is converted into `EarthMJ2000Eq`. By construction, that
conversion IS the body's position relative to Earth in `EarthMJ2000Eq`: a `CoordinateSystem`
conversion between two body-centred frames is exactly the vector between the two origins,
expressed in the target axes, which is the same quantity `GetMJ2000Position`/a `ReportFile`
parameter such as `Mars.EarthMJ2000Eq.X` would report (both geometric, no light-time or
aberration correction applied by default in any of the three routes). This was RUN, not assumed
(see the probe transcript in the worker's report); the `ReportFile`-from-a-plain-script fallback
the task names was not additionally exercised because this route already succeeded and reuses an
already-validated GMAT primitive.

Also recorded per this task's own ask: each epoch in BOTH `epoch_a1mjd` (GMAT's own A.1 Modified
Julian Date) and `epoch_tai_ns` (TAI nanoseconds), the latter computed by this script using the
EXACT SAME formula as `av_cdm::time::Tai::from_a1_mjd`
(`crates/av-cdm/src/time.rs`: `GMAT_MJD_AT_UNIX_EPOCH = 10587.5`, `NS_PER_DAY = 86400000000000.0`,
`A1_MINUS_TAI_NS = 34381700`, `tai_ns = round((a1_mjd - GMAT_MJD_AT_UNIX_EPOCH) * NS_PER_DAY) -
A1_MINUS_TAI_NS`, with ties broken away from zero exactly as Rust's `f64::round()` does) so the
Rust test does not have to redo an epoch conversion this generator already did, and so a caller
that only trusts the golden's own `epoch_tai_ns` field never depends on this script's Python
arithmetic agreeing with Rust's own -- both are restatements of the identical, exact, table-free
formula (A.1 vs TAI is a fixed offset, no leap-second table involved), verified as identical in
this task's own report.

Also recorded (re-verified here, per this task's own instruction, rather than assumed from N2's
Sun/Moon report): `SolarSystem.EphemerisSource`, `SolarSystem.DEFilename`, and each of
`Mars`/`Jupiter`'s own `PosVelSource` -- read off the SAME live GMAT instance this script's arc
runs in, not a different one, so a divergence between the propagated arc's ephemeris source and
the ten-epoch check's ephemeris source cannot go unnoticed.
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
POINT_MASSES = ["Mars", "Jupiter"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 86400.0
GOLDEN_NAME = "leo_1day_jgm2_8x8_mars_jupiter"
BALLISTIC_FIELDS = ("DryMass", "Cd", "Cr", "DragArea", "SRPArea", "TotalMass")
DE_FILE_NAME = "leDE1941.405"

# av_cdm::time::Tai's own constants (crates/av-cdm/src/time.rs), restated here so this script
# can compute epoch_tai_ns identically without depending on Rust at generation time.
GMAT_MJD_AT_UNIX_EPOCH = 10_587.5
NS_PER_DAY = 86_400_000_000_000.0
A1_MINUS_TAI_NS = 34_381_700


def a1_mjd_to_tai_ns(a1_mjd: float) -> int:
    """Mirrors `Tai::from_a1_mjd` exactly, including Rust's round-half-away-from-zero `f64::round()`."""
    delta = (a1_mjd - GMAT_MJD_AT_UNIX_EPOCH) * NS_PER_DAY
    a1_ns = int(delta + (0.5 if delta >= 0 else -0.5))
    return a1_ns - A1_MINUS_TAI_NS


def make_body_centered_cs(name: str, body: str):
    axis = gmat.Construct("MJ2000Eq", f"{name}Axes")
    cs = gmat.Construct("CoordinateSystem", name)
    cs.SetField("Origin", body)
    cs.SetReference(axis)
    return cs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument(
        "--tolerance-m",
        type=float,
        required=True,
        help=(
            "the position tolerance (m) crates/av-orbital/tests/thirdbody_mars_jupiter.rs reads "
            "from this file -- NO DEFAULT (round 1's own lesson: 'a default tolerance pins "
            "nothing'). Measure the actual trajectory residual first (run the test, read its "
            "printed value), then pass that measured value here plus a stated margin; never a "
            "value copied from another golden or left at a script default."
        ),
    )
    ap.add_argument(
        "--tolerance-ephemeris-abs-m",
        type=float,
        required=True,
        help=(
            "round 2 (task 2b): the ten-epoch ephemeris check's own absolute position "
            "tolerance (m), moved into this file out of "
            "thirdbody_mars_jupiter.rs's own const TOLERANCE_ABS_M (round 1's decision 3). NO "
            "DEFAULT -- measure ten_epoch_ephemeris_agreement_with_gmat_reported_positions's "
            "printed max |diff| first, then pass that plus a stated margin."
        ),
    )
    ap.add_argument(
        "--tolerance-ephemeris-rel",
        type=float,
        required=True,
        help=(
            "round 2 (task 2b): the ten-epoch ephemeris check's own relative position "
            "tolerance, moved into this file out of thirdbody_mars_jupiter.rs's own const "
            "TOLERANCE_REL. NO DEFAULT -- measure the same test's printed max relative first, "
            "then pass that plus a stated margin."
        ),
    )
    args = ap.parse_args()

    gravity_path = GMAT_ROOT / "data" / "gravity" / "earth" / GRAVITY_FILE
    gravity_sha256 = hashlib.sha256(gravity_path.read_bytes()).hexdigest()
    de_path = GMAT_ROOT / "data" / "planetary_ephem" / "de" / DE_FILE_NAME
    de_sha256 = hashlib.sha256(de_path.read_bytes()).hexdigest()

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
    point_mass_objs = {}
    for body in POINT_MASSES:
        pm = gmat.Construct("PointMassForce", f"GoldenPM_{body}")
        pm.SetField("BodyName", body)
        fm.AddForce(pm)
        point_mass_objs[body] = pm

    prop = gmat.Construct("Propagator", "GoldenProp")
    gator = gmat.Construct(PROP["integrator"], "GoldenGator")
    prop.SetReference(gator)
    prop.SetReference(fm)
    prop.SetField("InitialStepSize", PROP["initial_step_s"])
    prop.SetField("Accuracy", PROP["accuracy"])
    prop.SetField("MinStep", PROP["min_step_s"])
    prop.SetField("MaxStep", PROP["max_step_s"])

    # The ten-epoch ephemeris check's CoordinateSystems, constructed BEFORE gmat.Initialize()
    # (GMAT's API requires every object that needs Initialize() to already be constructed and
    # referenced -- exactly gmat_sys::Gmat::coordinate_system's own convention).
    earth_cs = make_body_centered_cs("N2EarthMJ2000Eq", "Earth")
    body_cs = {body: make_body_centered_cs(f"N2{body}MJ2000Eq", body) for body in POINT_MASSES}

    gmat.Initialize()
    prop.AddPropObject(sat)
    prop.PrepareInternals()
    internal_prop = prop.GetPropagator()

    x0 = [float(v) for v in list(internal_prop.GetState())[:6]]
    a1_epoch = sat.GetEpoch()

    # --- Ephemeris source, re-verified on THIS live instance (never assumed from another
    # golden/report) -- this task's own instruction. ---
    ss = gmat.GetSolarSystem()
    ephemeris_source = {
        "solar_system_ephemeris_source": ss.GetField("EphemerisSource"),
        "solar_system_de_filename": ss.GetField("DEFilename"),
        "body_pos_vel_source": {body: gmat.GetObject(body).GetField("PosVelSource") for body in POINT_MASSES},
    }

    # --- The ten-epoch ephemeris check, BEFORE stepping the arc (so the epochs are pinned to
    # the arc's own start, independent of any propagation happening below). ---
    conv = gmat.CoordinateConverter()
    zero_state = gmat.Rvector6(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    body_positions = []
    for i in range(10):
        frac = i / 9.0
        dt_s = DURATION_S * frac
        a1_mjd_i = a1_epoch + dt_s / 86_400.0
        epoch_obj = gmat.A1Mjd(a1_mjd_i)
        entry = {
            "index": i,
            "dt_s": dt_s,
            "epoch_a1mjd": a1_mjd_i,
            "epoch_tai_ns": a1_mjd_to_tai_ns(a1_mjd_i),
        }
        for body in POINT_MASSES:
            out_state = gmat.Rvector6()
            conv.Convert(epoch_obj, zero_state, body_cs[body], out_state, earth_cs)
            entry[f"{body.lower()}_position_km"] = [out_state.GetElement(j) for j in range(3)]
        body_positions.append(entry)

    elapsed = 0.0
    while elapsed < DURATION_S - 1e-9:
        dt = min(PROP["max_step_s"], DURATION_S - elapsed)
        # Step(dt) returns False after MaxStepAttempts substep attempts, leaving the state
        # partially advanced (same rule every other generator in this directory follows).
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
    point_mass_readback = {body: {"body_name": obj.GetField("BodyName")} for body, obj in point_mass_objs.items()}

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
            "point_masses": POINT_MASSES,
            "srp": False, "drag": None, "relativity": False, "tides": False,
        },
        "point_mass_readback": point_mass_readback,
        "gravity_file_name": GRAVITY_FILE,
        "gravity_file_sha256": gravity_sha256,
        "gravity_file_readback": gravity_readback,
        "central_body_readback": central_body_readback,
        "de_file_name": DE_FILE_NAME,
        "de_file_sha256": de_sha256,
        "ephemeris_source": ephemeris_source,
        "solver_iterations": None,
        "propagator": PROP,
        "propagator_readback": propagator_readback,
        "duration_s": DURATION_S,
        "initial_state": x0, "final_state": x1,
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_m * 1e-3,
        "tolerance_ephemeris_abs_m": args.tolerance_ephemeris_abs_m,
        "tolerance_ephemeris_rel": args.tolerance_ephemeris_rel,
        "body_positions": body_positions,
        "body_positions_note": (
            "GMAT's own reported position of Mars/Jupiter relative to Earth in EarthMJ2000Eq, km, "
            "at 10 epochs evenly spread across the arc (index 0 = t0, index 9 = t0+duration_s). "
            "gmat.GetObject/GetRuntimeObject + .GetMJ2000Position(A1Mjd) was tried FIRST and raised "
            "a GMAT APIException ('ArrayTemplate error : out-of-bounds') for every body tried "
            "(body-independent -- see this script's own module doc and the worker's report for the "
            "probe transcript), so this uses gmat.CoordinateConverter().Convert instead: the SAME "
            "primitive crates/gmat-sys's own Gmat::convert/convert_with_rotation calls "
            "(ADR-002's fourth amendment). A CoordinateSystem is built per body (Origin=<body>, "
            "Axes=a freshly constructed MJ2000Eq object bound via SetReference -- exactly "
            "gmat_sys::Gmat::coordinate_system's own construction), and the null state "
            "[0,0,0,0,0,0] in that body's own frame is converted into EarthMJ2000Eq -- by "
            "construction, this IS the body's position relative to Earth in EarthMJ2000Eq (both "
            "geometric, no light-time/aberration, same as GetMJ2000Position or a ReportFile "
            "Mars.EarthMJ2000Eq.X parameter would report). The ReportFile-from-a-plain-script "
            "fallback this task names was not additionally exercised because this route already "
            "succeeded."
        ),
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    golden["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(json.dumps(golden, indent=2, sort_keys=True) + "\n")
    print("wrote", out, "final state", x1)
    print("gravity_file_sha256", gravity_sha256)
    print("de_file_sha256", de_sha256)
    print("propagator_readback", propagator_readback)
    print("ephemeris_source", ephemeris_source)
    print("body_positions[0]", body_positions[0])
    print("body_positions[9]", body_positions[9])


if __name__ == "__main__":
    main()
