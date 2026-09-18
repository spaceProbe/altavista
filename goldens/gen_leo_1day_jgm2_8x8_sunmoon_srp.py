"""Generate the golden arc `leo_1day_jgm2_8x8_sunmoon_srp.json` with GMAT's own propagator
(docs/native-dynamics-plan.md milestone N3, "N3's solar radiation pressure -- cannonball SRP
with a conical shadow model").

Run explicitly, never from a test:

  .venv/bin/python goldens/gen_leo_1day_jgm2_8x8_sunmoon_srp.py --reason "..." --tolerance-m <measured>

**Why this golden exists, and why this exact arc.** `goldens/leo_1day_jgm2_8x8_sunmoon.json`
(the P0 arc) already pins JGM2 8x8 + Sun + Moon with no SRP and no drag. This golden is the
SAME arc shape (same epoch, same initial Keplerian elements, same ballistic set: SMA 6878.0,
ECC 0.001, INC 51.6, RAAN 30.0, AOP 0.0, TA 0.0; DryMass 500, Cd 2.2, Cr 1.8, DragArea 5,
SRPArea 5; epoch "01 Jan 2026 00:00:00.000") with spherical `SolarRadiationPressure` added and
NO drag -- SRP is deliberately the ONLY force added relative to a golden that already exists,
so the native model (which has no native drag yet -- that is task 3b/3c) can fly the WHOLE
86,400 s arc and any residual is attributable to SRP alone, not entangled with an
unimplemented drag model the way `leo_1day_jgm2_8x8_sunmoon_drag_srp.json` (M4.3's golden,
which this script does NOT touch or regenerate) would be.

**This arc was chosen partly because it passes through Earth's shadow.** A 500 km-class LEO
(SMA 6878 km => ~500 km altitude) at 51.6 deg inclination, epoch 01 Jan 2026, is close enough
to the December solstice that the orbit plane's beta angle (measured here: ~26 deg for
RAAN=30, well under the ~68 deg critical angle asin(BodyRadius/SMA) above which no eclipse is
geometrically possible at this altitude) is small enough that the arc crosses Earth's umbra
and penumbra every ~90-minute revolution -- see this file's own `shadow_summary` block (below)
for the measured umbra/penumbra/full-sun fractions over the arc.

**A defect found and root-caused while measuring this:** `SolarRadiationPressure.PercentSun`,
read back from the SAME configured force object this script passed to `fm.AddForce(srp)`,
stays frozen at its C++ constructor default (`1.0`, full sun) for the entire arc, even on an
orbit independently confirmed (below) to spend ~35-40% of its time in Earth's umbra. Root
cause: `gen_leo_1day_jgm2_8x8.py`'s own module doc already recorded the general form of this
wrinkle for the `Propagator`/integrator front-end objects ("the object that actually steps ...
is `prop.GetPropagator()`'s return [value], read only after `PrepareInternals()`"); the same
is true of the `ForceModel`'s own forces -- `PropSetup`/`Propagator::PrepareInternals()` builds
its OWN internal `ODEModel`, and while `ODEModel::AddForce` (`third_party/gmat-src/src/base/
forcemodel/ODEModel.cpp`) stores the exact pointer passed to it with no clone at THAT layer,
`PrepareInternals()` itself clones the whole `ForceModel` (and therefore every force inside
it, `SolarRadiationPressure` included) before the internal `Propagator` ever steps -- so the
`percentSun` MEMBER VARIABLE that `SolarRadiationPressure::GetDerivatives` updates on every
call belongs to that internal CLONE, never to the configured object this script's own `srp`
handle still points at. `Flux`/`Flux_Pressure`/`Nominal_Sun`/`SunRadius`/`BodyRadius` (this
file's own `srp_readback`, below) are unaffected -- they are set once, either at construction
(`Flux`/`Flux_Pressure`/`Nominal_Sun`, pure C++ defaults) or at `Initialize()` time
(`SunRadius`/`BodyRadius`, from `Sun`/`Earth`'s own `EquatorialRadius`, called on the
CONFIGURED object by the top-level `gmat.Initialize()` this script calls BEFORE
`PrepareInternals()` clones anything) -- so they read correctly off the handle this script
already has; only the propagation-time-varying `PercentSun` is stale. Worked around, per this
task's own literal instruction ("according to YOUR OWN shadow function"): `shadow_summary`
below is computed by this script's own Python port of the Montenbruck & Gill / GMAT
`ShadowState::FindShadowState` conical-shadow formula (identical to `crates/av-orbital/src/
srp.rs`'s Rust implementation -- see that module's own doc comment for the derivation and the
exact GMAT source it was read from), applied to GMAT's own REPORTED spacecraft and Sun
positions (never to `PercentSun`), sampled at a finer, INDEPENDENT propagation (its own
`Spacecraft`/`ForceModel`/`Propagator` objects, `MaxStep` 60 s rather than this golden's own
600 s, so a penumbra window -- measured a few tens of seconds wide -- is not stepped over) that
never touches `x0`/`final_state` (those still come from the ONE propagation this script's own
module doc names, at this golden's own declared `PROP` settings). `fraction_penumbra` in the
committed golden is strictly positive, exercising exactly the case ADR-002's third amendment
records ("SRP inside a penumbra: shadow partials omitted, GMAT warns at initialization -- not
exercised by the [drag/SRP golden's] arc"), which THIS arc, unlike that one, does exercise.

Every propagator and force-model field below is READ BACK off the live GMAT objects after
`PrepareInternals()`, not echoed from what this script requested -- `gen_leo_1day_jgm2_8x8.py`'s
own module doc names the measured wrinkle (`propagator_readback` must come from
`prop.GetPropagator()`'s return value, not the front-end `Propagator` or the bare integrator
object) that this script follows identically.

**Every SRP constant this task names, read off the live, INITIALIZED `SolarRadiationPressure`
force object** (`srp.GetField(...)`, GMAT's own `SolarRadiationPressure::GetRealParameter` --
`third_party/gmat-src/src/base/forcemodel/SolarRadiationPressure.cpp`): `Flux`, `Flux_Pressure`
(= `Flux / GmatPhysicalConstants::c`, already divided by GMAT itself -- this script never reads
or re-derives the speed of light on its own), `Nominal_Sun` (the 1 AU reference distance the
inverse-square law is normalised to), `SunRadius` and `BodyRadius` (the apparent-angular-radius
shadow-cone geometry's own inputs -- both read from `Sun`/`Earth`'s own `EquatorialRadius` at
`Initialize()` time), and `SRPModel` (confirms `"Spherical"`, not `"SPADFile"`/`"NPlate"`).

**The tolerance is measured, not assumed** (`--tolerance-m` has no default -- ADR-002's goldens
rule, and round 1's own lesson: "a default tolerance pins nothing"). Procedure: (1) generate
this golden with a placeholder tolerance far above anything plausible so the file exists and
the native-model test can run at all; (2) run
`crates/av-orbital/tests/srp_goldens.rs`'s trajectory-residual test and read its printed
measured residual; (3) re-run this script with `--tolerance-m` set to that measured residual
plus a stated margin, so the file committed to the repository carries the tolerance actually in
force, never a copy or a guess -- see this script's own printed output and the worker's report
for the exact measured value and the regeneration command used.
"""
import argparse
import datetime
import hashlib
import json
import math
import sys
from pathlib import Path

GMAT_ROOT = Path("/Users/probe/code/AltaVista/GMAT R2026a")
sys.path.insert(1, str(GMAT_ROOT / "bin"))
import gmatpy as gmat  # noqa: E402

gmat.Setup(str(GMAT_ROOT / "bin" / "api_startup_file.txt"))


def _dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def _norm(a):
    return math.sqrt(_dot(a, a))


def illumination_fraction(r_sc_km, r_sun_km, sun_radius_km, body_radius_km):
    """Python port of `crates/av-orbital/src/srp.rs::illumination_fraction` -- the conical
    (umbra+penumbra) shadow formula, Montenbruck & Gill sec. 3.4.2, as GMAT's own
    `ShadowState::FindShadowState`/`GetPercentSunInPenumbra`
    (`third_party/gmat-src/src/base/solarsys/ShadowState.cpp`) implement it. `r_sc_km` is the
    spacecraft position relative to the central/occulting body (Earth); `r_sun_km` is the
    Sun's position relative to the SAME body. Used here only to measure `shadow_summary`
    (below) independently of GMAT's own `SolarRadiationPressure.PercentSun`, which this
    script's own module doc explains is stale when read back this way."""
    r_sun_mag = _norm(r_sun_km)
    unitsun = [v / r_sun_mag for v in r_sun_km]
    rdotsun = _dot(r_sc_km, unitsun)
    if rdotsun > 0.0:
        return 1.0
    sat_to_sun = [-(r_sc_km[i] - r_sun_km[i]) for i in range(3)]
    sat_to_sun_dist = _norm(sat_to_sun)
    sat_to_body_dist = _norm(r_sc_km)
    if sun_radius_km >= sat_to_sun_dist:
        return 1.0
    if body_radius_km >= sat_to_body_dist:
        return 0.0
    a = math.asin(max(-1.0, min(1.0, sun_radius_km / sat_to_sun_dist)))
    b = math.asin(max(-1.0, min(1.0, body_radius_km / sat_to_body_dist)))
    unit_body_to_sat = [v / sat_to_body_dist for v in r_sc_km]
    unit_sat_to_sun = [v / sat_to_sun_dist for v in sat_to_sun]
    cosc = max(-1.0, min(1.0, -_dot(unit_body_to_sat, unit_sat_to_sun)))
    c = math.acos(cosc)
    if a + b <= c:
        return 1.0
    elif c < b - a:
        return 0.0
    elif abs(a - b) < c and a + b > c:
        a2, b2 = a * a, b * b
        x = (c * c + a2 - b2) / (2.0 * c)
        y = math.sqrt(max(0.0, a2 - x * x))
        area = a2 * math.acos(max(-1.0, min(1.0, x / a))) + b2 * math.acos(max(-1.0, min(1.0, (c - x) / b))) - c * y
        return 1.0 - area / (math.pi * a2)
    else:
        return 1.0 - (b * b) / (a * a)

EPOCH = "01 Jan 2026 00:00:00.000"
SAT = {
    "SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
    "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0,
}
GRAVITY_FILE = "JGM2.cof"
DEGREE = 8
ORDER = 8
POINT_MASSES = ["Luna", "Sun"]
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 86400.0
GOLDEN_NAME = "leo_1day_jgm2_8x8_sunmoon_srp"
BALLISTIC_FIELDS = ("DryMass", "Cd", "Cr", "DragArea", "SRPArea", "TotalMass")
DE_FILE_NAME = "leDE1941.405"
SRP_FIELDS = ("UseAnalytic", "SunRadius", "BodyRadius", "Flux", "Flux_Pressure", "SRPModel", "Nominal_Sun")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument(
        "--tolerance-m",
        type=float,
        required=True,
        help=(
            "the position tolerance (m) crates/av-orbital/tests/srp_goldens.rs reads from this "
            "file -- NO DEFAULT (round 1's own lesson: 'a default tolerance pins nothing'). "
            "Measure the actual trajectory residual first (run the test, read its printed "
            "value), then pass that measured value here plus a stated margin."
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
    srp = gmat.Construct("SolarRadiationPressure", "GoldenSRP")
    fm.AddForce(srp)

    prop = gmat.Construct("Propagator", "GoldenProp")
    gator = gmat.Construct(PROP["integrator"], "GoldenGator")
    prop.SetReference(gator)
    prop.SetReference(fm)
    prop.SetField("InitialStepSize", PROP["initial_step_s"])
    prop.SetField("Accuracy", PROP["accuracy"])
    prop.SetField("MinStep", PROP["min_step_s"])
    prop.SetField("MaxStep", PROP["max_step_s"])

    # --- shadow_summary's own, SEPARATE, finer (MaxStep 60 s) spacecraft/force model/
    # propagator, gravity-only (its own module doc: "how much of the arc is in umbra/penumbra
    # according to this script's own shadow function" needs only position, not the SRP force
    # itself). Constructed here, BEFORE the one gmat.Initialize() call below, so this script
    # calls Initialize() exactly once, matching every other generator's convention. ---
    shadow_sun_axes = gmat.Construct("MJ2000Eq", "ShadowSunAxes")
    shadow_sun_cs = gmat.Construct("CoordinateSystem", "ShadowSunCS")
    shadow_sun_cs.SetField("Origin", "Sun")
    shadow_sun_cs.SetReference(shadow_sun_axes)
    shadow_earth_axes = gmat.Construct("MJ2000Eq", "ShadowEarthAxes")
    shadow_earth_cs = gmat.Construct("CoordinateSystem", "ShadowEarthCS")
    shadow_earth_cs.SetField("Origin", "Earth")
    shadow_earth_cs.SetReference(shadow_earth_axes)

    shadow_sat = gmat.Construct("Spacecraft", "ShadowSat")
    shadow_sat.SetField("DateFormat", "UTCGregorian")
    shadow_sat.SetField("Epoch", EPOCH)
    shadow_sat.SetField("CoordinateSystem", "EarthMJ2000Eq")
    shadow_sat.SetField("DisplayStateType", "Keplerian")
    for k, v in SAT.items():
        shadow_sat.SetField(k, v)
    shadow_fm = gmat.Construct("ForceModel", "ShadowFM")
    shadow_fm.SetField("CentralBody", "Earth")
    shadow_grav = gmat.Construct("GravityField", "ShadowGrav")
    shadow_grav.SetField("BodyName", "Earth")
    shadow_grav.SetField("PotentialFile", GRAVITY_FILE)
    shadow_grav.SetField("Degree", DEGREE)
    shadow_grav.SetField("Order", ORDER)
    shadow_fm.AddForce(shadow_grav)
    shadow_prop = gmat.Construct("Propagator", "ShadowProp")
    shadow_gator = gmat.Construct(PROP["integrator"], "ShadowGator")
    shadow_prop.SetReference(shadow_gator)
    shadow_prop.SetReference(shadow_fm)
    shadow_prop.SetField("Accuracy", PROP["accuracy"])
    shadow_step_s = 60.0
    shadow_prop.SetField("MaxStep", shadow_step_s)

    gmat.Initialize()
    prop.AddPropObject(sat)
    prop.PrepareInternals()
    internal_prop = prop.GetPropagator()
    shadow_prop.AddPropObject(shadow_sat)
    shadow_prop.PrepareInternals()
    shadow_internal = shadow_prop.GetPropagator()

    x0 = [float(v) for v in list(internal_prop.GetState())[:6]]
    a1_epoch = sat.GetEpoch()
    shadow_a1_epoch = shadow_sat.GetEpoch()
    if abs(shadow_a1_epoch - a1_epoch) > 1e-9:
        raise RuntimeError("shadow-sampling spacecraft epoch disagrees with the golden's own epoch")

    # --- Ephemeris source, re-verified on THIS live instance (never assumed from another
    # golden/report) -- matching gen_leo_1day_jgm2_8x8_mars_jupiter.py's own instruction. ---
    ss = gmat.GetSolarSystem()
    ephemeris_source = {
        "solar_system_ephemeris_source": ss.GetField("EphemerisSource"),
        "solar_system_de_filename": ss.GetField("DEFilename"),
        "body_pos_vel_source": {body: gmat.GetObject(body).GetField("PosVelSource") for body in POINT_MASSES},
    }

    # --- Every SRP constant this task names, read off the live, initialized force object. ---
    srp_readback = {f: srp.GetField(f) for f in SRP_FIELDS}

    # --- Step the arc -- the ONE propagation that produces x0/final_state, at this golden's
    # own declared PROP settings (MaxStep 600 s). ---
    elapsed = 0.0
    while elapsed < DURATION_S - 1e-9:
        dt = min(PROP["max_step_s"], DURATION_S - elapsed)
        # Step(dt) returns False after MaxStepAttempts substep attempts, leaving the state
        # partially advanced (same rule every other generator in this directory follows).
        if not internal_prop.Step(dt):
            raise RuntimeError(f"Propagator.Step({dt}) returned False at elapsed={elapsed}: raise MaxStepAttempts or use smaller chunks")
        elapsed += dt
    x1 = [float(v) for v in list(internal_prop.GetState())[:6]]

    # --- shadow_summary: sample the SEPARATE, finer (MaxStep 60 s) propagation constructed
    # above, measuring how much of the arc is in umbra/penumbra/full sun via this script's own
    # `illumination_fraction` (see this file's own module doc, "A defect found and
    # root-caused" -- GMAT's own PercentSun cannot be read back this way, so this is computed
    # independently rather than assumed absent). Sun position at each sample epoch via
    # gmat.CoordinateConverter (the same primitive `crates/gmat-sys::Gmat::convert` wraps,
    # ADR-002's fourth amendment). Never touches x0/final_state above. ---
    zero_state = gmat.Rvector6(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    conv = gmat.CoordinateConverter()
    nu_samples = []
    elapsed = 0.0
    while elapsed < DURATION_S - 1e-9:
        dt = min(shadow_step_s, DURATION_S - elapsed)
        if not shadow_internal.Step(dt):
            raise RuntimeError(f"shadow-sampling Propagator.Step({dt}) returned False at elapsed={elapsed}")
        elapsed += dt
        st = list(shadow_internal.GetState())[:6]
        out_state = gmat.Rvector6()
        conv.Convert(gmat.A1Mjd(shadow_a1_epoch + elapsed / 86400.0), zero_state, shadow_sun_cs, out_state, shadow_earth_cs)
        r_sun_km = [out_state.GetElement(j) for j in range(3)]
        nu = illumination_fraction(st[:3], r_sun_km, float(srp_readback["SunRadius"]), float(srp_readback["BodyRadius"]))
        nu_samples.append({"dt_s": elapsed, "nu": nu})

    n = len(nu_samples)
    n_umbra = sum(1 for s in nu_samples if s["nu"] <= 0.0)
    n_full = sum(1 for s in nu_samples if s["nu"] >= 1.0)
    n_penumbra = n - n_umbra - n_full
    penumbra_dts = [s["dt_s"] for s in nu_samples if 0.0 < s["nu"] < 1.0]
    shadow_summary = {
        "sample_step_s": shadow_step_s,
        "n_samples": n,
        "n_umbra": n_umbra,
        "n_penumbra": n_penumbra,
        "n_full_sun": n_full,
        "fraction_umbra": n_umbra / n,
        "fraction_penumbra": n_penumbra / n,
        "fraction_full_sun": n_full / n,
        "penumbra_dt_s_examples": penumbra_dts[:10],
        "nu_samples": nu_samples,
        "note": (
            "computed by this script's own illumination_fraction (a Python port of "
            "crates/av-orbital/src/srp.rs, itself a port of GMAT's ShadowState::"
            "FindShadowState/GetPercentSunInPenumbra), applied to a SEPARATE, finer "
            "(MaxStep=60s) propagation of the same initial state/gravity model, sampling "
            "GMAT's own reported spacecraft position and Sun position (via "
            "CoordinateConverter) every 60 s -- NOT GMAT's own SolarRadiationPressure."
            "PercentSun, which this file's own module doc explains cannot be read back this "
            "way (it reflects an internal clone PrepareInternals() builds, not the configured "
            "object this script holds a handle to). Never touches x0/final_state above."
        ),
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
            "srp": True, "drag": None, "relativity": False, "tides": False,
        },
        "point_mass_readback": point_mass_readback,
        "gravity_file_name": GRAVITY_FILE,
        "gravity_file_sha256": gravity_sha256,
        "gravity_file_readback": gravity_readback,
        "central_body_readback": central_body_readback,
        "de_file_name": DE_FILE_NAME,
        "de_file_sha256": de_sha256,
        "ephemeris_source": ephemeris_source,
        "srp_readback": srp_readback,
        "shadow_summary": shadow_summary,
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
    print("de_file_sha256", de_sha256)
    print("propagator_readback", propagator_readback)
    print("ephemeris_source", ephemeris_source)
    print("srp_readback", srp_readback)
    print("shadow_summary (fractions)", shadow_summary["fraction_umbra"], shadow_summary["fraction_penumbra"], shadow_summary["fraction_full_sun"])


if __name__ == "__main__":
    main()
