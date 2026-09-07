"""Generate the golden `leo_1day_maneuver_vnb.json` (M10.1, `docs/open-questions.md` question
97): the same LEO orbit / force model as `goldens/gen_leo_1day.py` / `drms/leo_1day_golden.
system.yaml`'s `leo_sys` (JGM2 8x8 Earth gravity, Sun + Moon point masses, PrinceDormand78 at
Accuracy 1e-13 / MaxStep 600), with one impulsive 20 m/s prograde burn applied mid-arc through
**altavista's own reference maneuver path** (`altavista.scenario.Scenario.maneuver`, `frame="VNB"`)
-- not a hand re-derivation of its V/N/B basis formula. This is the pin
`crates/av-kernel/src/drm/maneuver.rs::dv_to_inertial`'s `AxesKind::Vnb` branch is measured
against (`crates/av-kernel/tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_vnb_burn`): the
same scenario, expressed as a DRM (`drms/leo_1day_maneuver_vnb.*.yaml`, a `"maneuver"`
`Scenario.events` entry), run through the executor, and compared against this golden's
`state_pre_burn`/`state_post_burn`/`final_state` at the same tolerance class as
`leo_1day_jgm2_8x8_sunmoon.json` (0.05 m / 5e-5 m/s).

Run explicitly, never from a test:
  .venv/bin/python goldens/gen_leo_1day_maneuver_vnb.py --reason "..."
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
# 20 m/s prograde -- the same magnitude examples/01_leo_propagate.py's own "Reboost" maneuver
# uses, chosen for that precedent rather than tuned to anything.
DV_VNB_KM_S = [0.02, 0.0, 0.0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument("--tolerance-m", type=float, default=0.05, help="same tolerance class as leo_1day_jgm2_8x8_sunmoon.json")
    args = ap.parse_args()

    sc = gv.Scenario("leo_1day_maneuver_vnb", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("Golden", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)
    # Defaults (degree=order=8, point_masses=("Luna","Sun")) reproduce leo_sys's JGM2 8x8 +
    # Sun/Moon force model exactly -- see altavista.scenario.Scenario.force_model's own defaults.
    fm = sc.force_model()
    prop = sc.propagator(force_model=fm, integrator="PrinceDormand78", max_step=600.0, min_step=0.0, initial_step=60.0, accuracy=1e-13)

    sc.propagate([sat], seconds=PRE_BURN_S, step=STEP_S, propagator=prop)
    x0 = list(sat.trajectory.pos[0]) + list(sat.trajectory.vel[0])
    x_pre_burn = list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])
    epoch_pre_burn_a1mjd = sat.epoch

    # The reference implementation this task pins against -- altavista's own VNB burn path, not a
    # copy of its formula.
    sc.maneuver(sat, dv=DV_VNB_KM_S, frame="VNB", name="burn1")
    x_post_burn = list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])
    epoch_post_burn_a1mjd = sat.epoch
    if abs(epoch_post_burn_a1mjd - epoch_pre_burn_a1mjd) > 1e-9:
        raise RuntimeError(f"maneuver() must not advance the epoch: {epoch_pre_burn_a1mjd} -> {epoch_post_burn_a1mjd}")
    if x_pre_burn[0:3] != x_post_burn[0:3]:
        raise RuntimeError("an impulsive maneuver must not change position")

    sc.propagate([sat], seconds=POST_BURN_S, step=STEP_S, propagator=prop)
    x_final = list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])

    golden = {
        "name": "leo_1day_maneuver_vnb",
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
        "dv_vnb_km_s": DV_VNB_KM_S,
        "initial_state": x0,
        "state_pre_burn": x_pre_burn,
        "state_post_burn": x_post_burn,
        "final_state": x_final,
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_m * 1e-3,
        "note": (
            "state_pre_burn/state_post_burn/final_state all come from altavista.scenario.Scenario."
            "maneuver's own VNB path (altavista/scenario.py), not a hand re-derivation of its V/N/B "
            "basis formula -- crates/av-kernel/src/drm/maneuver.rs::dv_to_inertial's AxesKind::Vnb "
            "branch is pinned against this same function field-for-field (see that function's own "
            "doc comment)."
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
