"""Generate the golden `leo_1day_maneuver_gmat_lvlh.json` (M11.3, `docs/open-questions.md`
question 102; file renamed from `leo_1day_maneuver_lvlh.json` by M12.4 question 106 so its name
can never be mistaken for this platform's own ratified convention): the same LEO orbit / force
model as `goldens/gen_leo_1day.py` / `drms/leo_1day_golden.system.yaml`'s `leo_sys`, with one
impulsive burn applied mid-arc through **altavista's own reference maneuver path**
(`altavista.scenario.Scenario.maneuver`, `frame="LVLH"`) -- not a hand re-derivation of any
rotation matrix.

`Scenario.maneuver`'s `"LVLH"` branch fires a real GMAT `ImpulsiveBurn` with
`CoordinateSystem=Local`, `Origin=Earth`, `Axes=LVLH` -- **GMAT's own literal, native local burn
axes**, per docs/open-questions.md question 102's decision ("pin LVLH against GMAT
ImpulsiveBurn with Axes = LVLH"). This is NOT the same convention as this platform's ratified
`AXES_KIND_VVLH` (Z=-R, Y=-N, X=N x R, question 73; renamed from `AXES_KIND_LVLH` by question
106 for exactly this reason) -- see `altavista/FRAMES.md`'s "GMAT ImpulsiveBurn LVLH vs
AXES_KIND_VVLH" section and `altavista/scenario.py::Scenario._fire_impulsive_burn`'s own doc
comment for the empirical measurement: GMAT's `Axes=LVLH` is X=R, Y=N x R (in-track), Z=N --
numerically **identical** to `AXES_KIND_RIC` (X=R, Z=N), not to `AXES_KIND_VVLH`.

Consequence for this golden's own DRM pin (`drms/leo_1day_maneuver_vvlh.*.yaml`,
`crates/av-kernel/tests/drm_maneuver.rs`): tagging this burn's `ScenarioEvent` `AXES_KIND_VVLH`
and running it through `crates/av-kernel`'s executor (whose `dv_to_inertial` `AxesKind::Vvlh`
branch implements the *ratified* Z=-R,Y=-N,X=N×R convention, question 73) does **not** reproduce
this golden -- confirmed and quantified in `drm_maneuver.rs`, not silently avoided. Tagging the
identical `dv` values `AXES_KIND_RIC` instead does reproduce it, because that is what GMAT's
`Axes=LVLH` field actually computed. Neither the golden's numbers nor `dv_to_inertial` are
altered to paper over this -- see `altavista/FRAMES.md` for the full write-up. Separately, the old
name `AXES_KIND_LVLH` is now `reserved` in `core.proto` -- a DRM that still declares it is a
typed load error, not a silent mismatch (`crates/av-kernel/tests/drm_maneuver.rs::
a_drm_naming_axes_kind_lvlh_is_a_typed_load_error`).

`DV_LVLH_KM_S` below is deliberately the same three numbers as
`goldens/gen_leo_1day_maneuver_ric.py`'s `DV_RIC_KM_S`: since GMAT's `Axes=LVLH` and altavista's
ObjectReferenced RIC system are the same physical triad, this golden's
`state_pre_burn`/`state_post_burn`/`final_state` come out bit-for-bit identical to
`leo_1day_maneuver_ric.json`'s -- itself a direct empirical proof of the mapping above, not an
assertion taken on faith.

M12.4 rename note: this is a pure file rename plus doc-comment/label updates -- the scenario
built below (vehicle, force model, epoch, burn, `frame="LVLH"` passed to `Scenario.maneuver`) is
byte-for-byte the same computation `gen_leo_1day_maneuver_lvlh.py` ran, so the regenerated
`state_pre_burn`/`state_post_burn`/`final_state`/`initial_state` numbers below are required to
stay identical to the retired script's own output; only `name`/`note`/`generated`/`reason`/
`sha256` (a self-hash over that text) are expected to change.

Run explicitly, never from a test:
  .venv/bin/python goldens/gen_leo_1day_maneuver_gmat_lvlh.py --reason "..."
"""
import argparse
import datetime
import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import altavista as gv  # noqa: E402

EPOCH = "01 Jan 2026 00:00:00.000"
# Same vehicle/orbit as goldens/gen_leo_1day.py's SAT / drms/leo_1day_golden.system.yaml's
# leo_sys spacecraft.* parameters -- this golden pins the *burn*, not a different vehicle.
KEPLERIAN = dict(SMA=6878.0, ECC=0.001, INC=51.6, RAAN=30.0, AOP=0.0, TA=0.0)
BALLISTICS = dict(DryMass=500.0, Cd=2.2, Cr=1.8, DragArea=5.0, SRPArea=5.0)
PRE_BURN_S = 3600.0
POST_BURN_S = 3600.0
STEP_S = 60.0
# 20 m/s -- same three numbers as gen_leo_1day_maneuver_ric.py's DV_RIC_KM_S, deliberately (see
# module docstring): GMAT's ImpulsiveBurn Axes=LVLH is measured to be the same X=R,Z=N triad as
# that golden's ObjectReferenced RIC system, so using the identical dv proves it directly.
DV_LVLH_KM_S = [0.0, 0.02, 0.0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument("--tolerance-m", type=float, default=0.05, help="same tolerance class as leo_1day_jgm2_8x8_sunmoon.json")
    args = ap.parse_args()

    sc = gv.Scenario("leo_1day_maneuver_gmat_lvlh", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("Golden", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)
    # Defaults (degree=order=8, point_masses=("Luna","Sun")) reproduce leo_sys's JGM2 8x8 +
    # Sun/Moon force model exactly -- see altavista.scenario.Scenario.force_model's own defaults.
    fm = sc.force_model()
    prop = sc.propagator(force_model=fm, integrator="PrinceDormand78", max_step=600.0, min_step=0.0, initial_step=60.0, accuracy=1e-13)

    sc.propagate([sat], seconds=PRE_BURN_S, step=STEP_S, propagator=prop)
    x0 = list(sat.trajectory.pos[0]) + list(sat.trajectory.vel[0])
    x_pre_burn = list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])
    epoch_pre_burn_a1mjd = sat.epoch

    # The reference implementation this task pins against -- altavista's own LVLH burn path (a
    # real GMAT ImpulsiveBurn fired with CoordinateSystem=Local, Axes=LVLH, GMAT's own native
    # local burn axes), not a copy of its rotation. "LVLH" here is altavista's literal-GMAT-axes
    # option (see Scenario.maneuver's own docstring) -- unrelated to this platform's ratified
    # AXES_KIND_VVLH, which altavista realizes through frame="VVLH" instead.
    sc.maneuver(sat, dv=DV_LVLH_KM_S, frame="LVLH", name="burn1")
    x_post_burn = list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])
    epoch_post_burn_a1mjd = sat.epoch
    if abs(epoch_post_burn_a1mjd - epoch_pre_burn_a1mjd) > 1e-9:
        raise RuntimeError(f"maneuver() must not advance the epoch: {epoch_pre_burn_a1mjd} -> {epoch_post_burn_a1mjd}")
    if x_pre_burn[0:3] != x_post_burn[0:3]:
        raise RuntimeError("an impulsive maneuver must not change position")

    sc.propagate([sat], seconds=POST_BURN_S, step=STEP_S, propagator=prop)
    x_final = list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])

    golden = {
        "name": "leo_1day_maneuver_gmat_lvlh",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "frame": "EarthMJ2000Eq",
        "units": {"position": "km", "velocity": "km/s"},
        "epoch_utc": EPOCH,
        "keplerian": KEPLERIAN,
        "ballistics": BALLISTICS,
        "force_model": {"central_body": "Earth", "gravity": {"file": "JGM2.cof", "degree": 8, "order": 8}, "point_masses": ["Luna", "Sun"]},
        "propagator": {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0},
        "pre_burn_duration_s": PRE_BURN_S,
        "post_burn_duration_s": POST_BURN_S,
        "burn_epoch_a1mjd": epoch_pre_burn_a1mjd,
        "dv_lvlh_km_s": DV_LVLH_KM_S,
        "initial_state": x0,
        "state_pre_burn": x_pre_burn,
        "state_post_burn": x_post_burn,
        "final_state": x_final,
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_m * 1e-3,
        "note": (
            "state_pre_burn/state_post_burn/final_state all come from altavista.scenario.Scenario."
            "maneuver's own LVLH path (altavista/scenario.py::Scenario._fire_impulsive_burn), which "
            "fires a real GMAT ImpulsiveBurn with CoordinateSystem=Local, Origin=Earth, "
            "Axes=LVLH -- GMAT's own literal, native local burn axes -- not a hand re-derivation "
            "of any rotation. IMPORTANT (question 102's crux, verified empirically, M11.3; file "
            "renamed from leo_1day_maneuver_lvlh.json by M12.4 question 106): GMAT's ImpulsiveBurn "
            "Axes=LVLH is X=R, Y=N x R (in-track), Z=N -- numerically IDENTICAL to AXES_KIND_RIC "
            "(X=R, Z=N), and DIFFERENT from this platform's own ratified AXES_KIND_VVLH (Z=-R, "
            "Y=-N, X=N x R, question 73's Vehicle-Velocity-Local-Horizontal convention; renamed "
            "from AXES_KIND_LVLH by question 106 so its name would never collide with this "
            "golden's GMAT-native LVLH). dv_lvlh_km_s above is deliberately the same three numbers "
            "as goldens/leo_1day_maneuver_ric.json's dv_ric_km_s, and this golden's "
            "state_pre_burn/state_post_burn/final_state are bit-for-bit identical to that "
            "golden's -- direct empirical proof of the mapping, not an assertion taken on faith. "
            "crates/av-kernel/src/drm/maneuver.rs::dv_to_inertial's AxesKind::Vvlh branch "
            "(read-only, ratified) implements the DIFFERENT VVLH convention, so a ScenarioEvent "
            "tagged AXES_KIND_VVLH does NOT reproduce this golden through the executor, and the "
            "retired name AXES_KIND_LVLH is now a typed load error rather than a silent mismatch "
            "-- see crates/av-kernel/tests/drm_maneuver.rs for the measured mismatch and "
            "altavista/FRAMES.md for the full write-up. Nothing here reorients this golden's "
            "numbers, or av-kernel's ratified AxesKind::Vvlh, to force a match."
        ),
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    golden["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(json.dumps(golden, indent=2, sort_keys=True) + "\n")
    print("wrote", out)
    print("initial_state  ", x0)
    print("state_pre_burn ", x_pre_burn)
    print("state_post_burn", x_post_burn)
    print("final_state    ", x_final)


if __name__ == "__main__":
    main()
