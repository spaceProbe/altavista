"""Generate the golden `demo_two_instance.json` (M17.3, `docs/open-questions.md` question 123;
M19.4, question 131, adds the drag-sail SIGNAL coupling): two spacecraft, identical vehicle/orbit
to `goldens/gen_leo_1day.py` / `drms/leo_1day_golden.system.yaml`'s `leo_sys` -- "the golden LEO
dynamics" -- but no longer identical FORCE MODELS from the start (M19.4 changes this):

  * `DemoFlt` -- JGM2 8x8 + Sun/Moon + atmospheric drag (JacchiaRoberts, the packaged CSSI
    space-weather file this repository's own GMAT install ships) from t=0. At t = 1800 s, its
    own force model is rebuilt with gravity Order dropped from 8 to 0 (Degree stays 8; drag
    unchanged) -- the same real, physically measurable DYNAMICS fault
    `crates/av-kernel/src/drm/fault.rs::apply_gmat_target`'s own `"force_model.gravity_order"`
    branch is pinned against. At `COMMAND_EPOCH_S` (derived below, not hand-picked), its own `Cd`
    jumps from 2.2 to `CD_AFTER_COMMAND` (a drag-sail deployment) -- the SIGNAL command
    `drms/demo_two_instance_ctrl.system.yaml`'s own declared range condition fires, applied as a
    live field mutation (no force-model reconstruction, mirroring `gmat_sys::model::GmatModel::
    step_with_ports`'s own `set_real_parameter` call -- question 130's "no segment opens per
    command").
  * `DemoMvr` -- JGM2 8x8 + Sun/Moon, NO drag (unchanged from M17.3). Propagates unchanged
    (order stays 8) to t = 5400 s, receives the identical 20 m/s prograde VNB burn
    `goldens/gen_leo_1day_maneuver_vnb.py` pins, then propagates unchanged to t = 7200 s.

**Why `DemoFlt` and `DemoMvr` are no longer propagated together for [0, 1800 s].** Through M18.3
both instances shared byte-identical physics up to the fault, so this script propagated them
together and asserted `state_at_fault_flt == state_at_fault_mvr` as a sanity check. M19.4 gives
`DemoFlt` drag from t=0 (`drms/demo_two_instance.sos.yaml`'s own `demo_flt`-only
`force_model.drag_*` overrides -- `demo_mvr` shares the same base `leo_demo_sys` SystemDefinition
but declares no drag, so it is untouched), so the two instances' own physics diverge immediately;
this script now builds two fully independent propagation chains and drops that equality check.

**`COMMAND_EPOCH_S`: independently derived, not copied from the Rust executor's own output.**
`_find_command_epoch_s` below propagates a THIRD, independent `DemoMvr`-shaped spacecraft (same
declared physics: JGM2 8x8 + Sun/Moon, no drag, the identical 5400 s VNB burn) and searches its
own `rmag(t)` for the first time it rises to or past `COMMAND_THRESHOLD_M` -- the same declared
condition `drms/demo_two_instance_ctrl.system.yaml` names -- then adds `2 * NATIVE_PERIOD_S`
(0.2 s): the two-hop SIGNAL router delay (`docs/open-questions.md` question 108's own "delivered
at the receiver's own next step" rule) between `demo_mvr` emitting the value that crosses the
threshold and `demo_flt` actually applying the resulting command, both at 10 Hz. This is a
genuine, independent re-derivation of the trigger epoch (down to ~1 s precision -- the search
below samples every NATIVE_PERIOD_S near the crossing, not merely at the 60 s output grid), not a
copy of the real Rust-executed fixture's own recorded `EVENT_KIND_PORT_COMMAND` epoch; see
`crates/av-kernel/tests/demo_two_instance.rs`'s own `COMMAND_TAI_NS` constant for that
independently-checked cross-reference (both should agree to within a native step or two -- and do,
measured while building this script).

**Expected order of magnitude, stated before this script's own numbers were computed the first
time (M19.4's own task brief requires this in writing, before running):** M18.3's own
`gmat_port_cd_command.json` measured 891.8 m of divergence over 7200 s at SMA 6628 km (~250 km
altitude) for a Cd change of 2.2 -> 4.4 (a factor of 2), commanded at the run's own midpoint
(3600 s) and held for the remaining 3600 s. This fixture's own orbit is SMA 6878 km (~500 km
altitude) -- roughly 250 km higher, where Jacchia-Roberts atmospheric density is 2-3 orders of
magnitude lower (a rough scale-height argument: ~40-60 km scale height at these altitudes, so
250 km is 4-6 scale heights, e^-5 =~ 0.007). To get a comparably measurable (tens-of-meters, not
sub-millimeter) effect from a much thinner atmosphere, this task's own `demo_ctrl` commands a much
larger Cd change (2.2 -> 220.0, a factor of 100, representing a drag-sail deployment rather than a
mere ballistic-coefficient tweak) held for a comparable duration (~900-1000 s here, vs. M18.3's
3600 s -- shorter, partially offsetting the larger Cd factor). Net expectation, reasoned from
M18.3's own calibration (linear in both the Cd multiplier and the commanded duration, roughly
quadratic secular growth over the LONGER of the two but here the shorter commanded window
dominates): low tens of meters, comfortably above 1 m (unambiguously non-vacuous) and comfortably
below the kilometre scale (would suggest a units/configuration error). Measured below: see this
script's own printed `measured_command_arc_divergence_m` and the golden JSON's own field of the
same name.

Run explicitly, never from a test:
  .venv/bin/python goldens/gen_demo_two_instance.py --reason "..."
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
# leo_sys spacecraft.* parameters (and drms/demo_two_instance.system.yaml's leo_demo_sys, which
# declares the identical physics under a new id) -- this golden pins the fault, the burn, and
# (M19.4) the drag-sail command, not a different vehicle.
KEPLERIAN = dict(SMA=6878.0, ECC=0.001, INC=51.6, RAAN=30.0, AOP=0.0, TA=0.0)
BALLISTICS = dict(DryMass=500.0, Cd=2.2, Cr=1.8, DragArea=5.0, SRPArea=5.0)
FAULT_S = 1800.0     # demo_flt: force_model.gravity_order 8 -> 0
MANEUVER_S = 5400.0  # demo_mvr: 20 m/s prograde VNB burn
END_S = 7200.0
STEP_S = 60.0
DV_VNB_KM_S = [0.02, 0.0, 0.0]  # same magnitude as gen_leo_1day_maneuver_vnb.py

# M19.4 (question 131): must match drms/demo_two_instance_ctrl.system.yaml's own declared
# condition.threshold_m/condition.mode and drms/demo_two_instance.sos.yaml's own
# demo_flt force_model.drag_* overrides / demo_ctrl's own port.emit_value exactly -- this script
# is an independent reference for the SAME declared configuration, not a free-standing choice.
COMMAND_THRESHOLD_M = 6_884_300.0
CD_BEFORE_COMMAND = 2.2
CD_AFTER_COMMAND = 220.0
NATIVE_PERIOD_S = 0.1  # step_rate_hz = 10.0 on every instance in demo_two_instance.sos.yaml
ROUTER_HOPS = 2        # demo_mvr -> demo_ctrl -> demo_flt, one native step of delay each

DRAG_MODEL = "JacchiaRoberts"
DRAG_CSSI_FILE = "SpaceWeather-All-v1.2.txt"


def state_km(sat):
    return list(sat.trajectory.pos[-1]) + list(sat.trajectory.vel[-1])


def _force_model_with_drag(sc, degree, order, point_masses, name):
    """`altavista.Scenario.force_model`'s own drag branch (`drag="JacchiaRoberts"`) does not set a
    weather source at all -- it leaves GMAT's own `DragForce` defaults (`HistoricWeatherSource`/
    `PredictedWeatherSource` = `"ConstantFluxAndGeoMag"`, constant F10.7/Kp, never touching a
    file), the same defaults `crates/gmat-sys/tests/drag_srp_stm.rs`/M18.3's own
    `gmat_port_cd_command.json` use. This task's own brief requires the packaged CSSI
    space-weather file specifically (`docs/open-questions.md` question 131), so this is a
    from-scratch force-model builder -- otherwise identical to `Scenario.force_model`'s own
    GravityField/PointMassForce construction -- with the three extra `DragForce` fields
    `crate::drm::binding::materialize_gmat` also sets, field-for-field.
    """
    g = sc.gmat
    fm = g.Construct("ForceModel", name)
    fm.SetField("CentralBody", "Earth")
    grav = g.Construct("GravityField", name + "Grav")
    grav.SetField("BodyName", "Earth")
    grav.SetField("PotentialFile", "JGM2.cof")
    grav.SetField("Degree", int(degree))
    grav.SetField("Order", int(order))
    fm.AddForce(grav)
    for i, body in enumerate(point_masses):
        pm = g.Construct("PointMassForce", f"{name}Pm{i}")
        pm.SetField("BodyName", body)
        fm.AddForce(pm)
    df = g.Construct("DragForce", name + "Drag")
    df.SetField("AtmosphereModel", DRAG_MODEL)
    df.SetField("HistoricWeatherSource", "CSSISpaceWeatherFile")
    df.SetField("PredictedWeatherSource", "CSSISpaceWeatherFile")
    df.SetField("CSSISpaceWeatherFile", DRAG_CSSI_FILE)
    atmos = g.Construct(DRAG_MODEL, name + "Atmos")
    df.SetReference(atmos)
    fm.AddForce(df)
    return fm


def _find_command_epoch_s():
    """Independently derive the epoch demo_ctrl's own range condition fires at -- see this
    module's own docstring for the full method. Returns (command_epoch_s, crossing_epoch_s,
    rmag_at_crossing_m)."""
    sc = gv.Scenario("demo_two_instance_search", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("SearchMvr", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)
    fm = sc.force_model(degree=8, order=8, point_masses=("Luna", "Sun"))
    prop = sc.propagator(force_model=fm, integrator="PrinceDormand78", max_step=600.0, min_step=0.0, initial_step=60.0, accuracy=1e-13)

    # [0, MANEUVER_S]: coarse (60 s) -- this task's own condition never fires before the maneuver
    # (checked below at this coarser resolution: rmag never approaches COMMAND_THRESHOLD_M in
    # this span; the orbit's own ECC = 0.001 makes rmag vary only ~14 km peak to peak, so a 60 s
    # grid cannot hide a real crossing here without also showing rmag within a few km of
    # threshold at its own coarse samples, which it does not -- see the printed diagnostic below).
    sc.propagate([sat], seconds=MANEUVER_S, step=STEP_S, propagator=prop)
    pre_burn_rmags = [1000.0 * (x * x + y * y + z * z) ** 0.5 for x, y, z in sat.trajectory.pos]
    closest_before_maneuver = max(pre_burn_rmags)
    print(f"[gen_demo_two_instance] search: max rmag before the maneuver = {closest_before_maneuver:.1f} m (threshold {COMMAND_THRESHOLD_M} m) -- must stay below threshold")
    if closest_before_maneuver >= COMMAND_THRESHOLD_M:
        raise RuntimeError("the range condition would already be satisfied before the maneuver -- COMMAND_THRESHOLD_M needs to be re-chosen")

    sc.maneuver(sat, dv=DV_VNB_KM_S, frame="VNB", name="search_burn")

    # [MANEUVER_S, END_S]: fine (NATIVE_PERIOD_S = 0.1 s), the real router's own native grid --
    # find the first sample at or past threshold.
    sc.propagate([sat], seconds=END_S - MANEUVER_S, step=NATIVE_PERIOD_S, propagator=prop)
    # `Spacecraft.trajectory` accumulates samples across EVERY propagate()/maneuver() call for
    # this spacecraft's whole lifetime (the coarse [0, MANEUVER_S] stage above included), not just
    # the latest call -- so `t[0]` is this scenario's own absolute epoch (t=0), and a crossing
    # index found anywhere in `pos`/`t` converts to an absolute elapsed-seconds-from-t=0 value
    # directly, with no extra `MANEUVER_S` offset (that stage's own samples are already baked into
    # the same cumulative, absolute-epoch-anchored arrays).
    pos = sat.trajectory.pos  # [[x, y, z], ...] km, cumulative over the whole spacecraft
    ts = sat.trajectory.t  # A1ModJulian days, cumulative, same indexing as pos
    t0_a1mjd = ts[0]
    crossing_idx = None
    for i, (x, y, z) in enumerate(pos):
        rmag_m = 1000.0 * (x * x + y * y + z * z) ** 0.5
        if rmag_m >= COMMAND_THRESHOLD_M:
            crossing_idx = i
            break
    if crossing_idx is None:
        raise RuntimeError(f"rmag never reached COMMAND_THRESHOLD_M = {COMMAND_THRESHOLD_M} m anywhere in [{MANEUVER_S}, {END_S}] s")
    crossing_epoch_s = (ts[crossing_idx] - t0_a1mjd) * 86400.0
    if not (MANEUVER_S <= crossing_epoch_s <= END_S):
        raise RuntimeError(f"crossing at absolute t={crossing_epoch_s:.1f} s falls outside the searched [{MANEUVER_S}, {END_S}] s window -- the coarse pre-maneuver check above should have caught an earlier crossing; this indicates a bug in this search, not a real earlier crossing")
    x, y, z = pos[crossing_idx]
    rmag_at_crossing_m = 1000.0 * (x * x + y * y + z * z) ** 0.5
    # Round to the native 0.1 s grid (the discrete resolution the real router/consumer actually
    # evaluates the condition at), then add the two-hop delivery delay.
    crossing_epoch_s = round(crossing_epoch_s / NATIVE_PERIOD_S) * NATIVE_PERIOD_S
    command_epoch_s = round((crossing_epoch_s + ROUTER_HOPS * NATIVE_PERIOD_S) / NATIVE_PERIOD_S) * NATIVE_PERIOD_S
    print(f"[gen_demo_two_instance] search: rmag crosses {COMMAND_THRESHOLD_M} m at t={crossing_epoch_s:.1f} s (rmag={rmag_at_crossing_m:.1f} m); command applies at t={command_epoch_s:.1f} s")
    return command_epoch_s, crossing_epoch_s, rmag_at_crossing_m


def _demo_flt_arc(command_epoch_s, cd_after):
    """Build DemoFlt's own three-stage arc: [0, FAULT_S] Order=8+drag, [FAULT_S, command_epoch_s]
    Order=0+drag (Cd stays 2.2), [command_epoch_s, END_S] Order=0+drag (Cd = cd_after). The Cd
    change is a live field mutation (`sat.obj.SetField("Cd", cd_after)`), never a force-model
    reconstruction -- mirroring `gmat_sys::DerivativeModel::set_real_parameter`/question 130's own
    "no segment opens per command" contract. `cd_after == CD_BEFORE_COMMAND` reproduces the
    uncommanded counterfactual (a structurally identical run whose "command" is a no-op),
    matching `goldens/gen_gmat_port_cd_command.py`'s own baseline methodology.
    """
    sc = gv.Scenario(f"demo_two_instance_flt_{cd_after}", frame="EarthMJ2000Eq")
    sat = sc.spacecraft("DemoFlt", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)

    fm_pre = _force_model_with_drag(sc, degree=8, order=8, point_masses=("Luna", "Sun"), name=f"gvFltPreFM_{cd_after}")
    prop_pre = sc.propagator(force_model=fm_pre, integrator="PrinceDormand78", max_step=600.0, min_step=0.0, initial_step=60.0, accuracy=1e-13)
    sc.propagate([sat], seconds=FAULT_S, step=STEP_S, propagator=prop_pre)
    state_at_fault = state_km(sat)

    fm_post = _force_model_with_drag(sc, degree=8, order=0, point_masses=("Luna", "Sun"), name=f"gvFltPostFM_{cd_after}")
    prop_post = sc.propagator(force_model=fm_post, integrator="PrinceDormand78", max_step=600.0, min_step=0.0, initial_step=60.0, accuracy=1e-13)
    sc.propagate([sat], seconds=command_epoch_s - FAULT_S, step=STEP_S, propagator=prop_post)
    state_pre_command = state_km(sat)

    sat.obj.SetField("Cd", cd_after)
    sc.propagate([sat], seconds=END_S - command_epoch_s, step=STEP_S, propagator=prop_post)
    state_final = state_km(sat)

    return state_at_fault, state_pre_command, state_final


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this golden is (re)generated; recorded in the file")
    ap.add_argument("--tolerance-m", type=float, default=0.05,
                     help="demo_flt's own tolerance: the SAME 0.05 m the pre-M19.4 golden and demo_mvr use. The measured residual against this golden is 0.0108 m, dominated by the ~0.2 s difference between this script's own independent command-epoch search (crossing at 6207.4 s) and the real router's native-tick/two-hop delivery grid (command applied at 6207.6 s), at an estimated ~5 mm per 0.1 s of mismatch (Delta_a * T_remaining, Delta_a ~ 6e-6 m/s^2 excess drag acceleration, T_remaining ~ 900 s). 0.05 m leaves ~4.6x margin over that measured residual. An earlier M19.4 draft set this to 1.0 m, three orders of magnitude above the sensitivity it was nominally sized against; the manager review reverted it, since a bound that loose would not catch a real regression in the commanded arc.")

    ap.add_argument("--tolerance-m-mvr", type=float, default=0.05,
                     help="demo_mvr's own tolerance: UNCHANGED from the pre-M19.4 golden -- demo_mvr's own physics (no drag, no SIGNAL command) are untouched by this task, so its own comparison keeps the same tolerance class as leo_1day_jgm2_8x8_sunmoon.json's own VNB/RIC/VVLH maneuver goldens.")
    args = ap.parse_args()

    command_epoch_s, crossing_epoch_s, rmag_at_crossing_m = _find_command_epoch_s()

    sc = gv.Scenario("demo_two_instance", frame="EarthMJ2000Eq")
    sat_flt = sc.spacecraft("DemoFltInit", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)
    sat_mvr = sc.spacecraft("DemoMvrInit", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)
    # spacecraft() does not itself record a trajectory sample or populate Spacecraft.state (that
    # only happens inside propagate()) -- read the initial Cartesian state directly off the GMAT
    # object instead (Spacecraft.cartesian(), the same GetCartesianState() call
    # crate::drm::binding::materialize_gmat's own initial_state_si() reaches through the shim).
    x0_flt = sat_flt.cartesian()
    x0_mvr = sat_mvr.cartesian()

    # -- demo_flt: three-stage arc (fault, then the drag-sail command). --
    state_at_fault_flt, state_pre_command_flt, state_final_flt = _demo_flt_arc(command_epoch_s, CD_AFTER_COMMAND)
    rmag_at_end_flt_m = (sum(x * x for x in state_final_flt[0:3])) ** 0.5 * 1000.0

    # -- baseline (uncommanded) demo_flt: identical arc, but Cd never changes -- proves the
    # command's own effect is real and lets the comparison isolate it (M18.3's own methodology).
    _, _, state_final_flt_baseline = _demo_flt_arc(command_epoch_s, CD_BEFORE_COMMAND)
    arc_divergence_m = (sum((a - b) ** 2 for a, b in zip(state_final_flt[0:3], state_final_flt_baseline[0:3]))) ** 0.5 * 1000.0

    # -- demo_mvr: unchanged from before M19.4 (no drag, its own fault-boundary bystander re-
    # materialization doesn't exist for it -- it never has a fault of its own). --
    sc2 = gv.Scenario("demo_two_instance_mvr", frame="EarthMJ2000Eq")
    sat_mvr2 = sc2.spacecraft("DemoMvr", epoch=EPOCH, keplerian=KEPLERIAN, **BALLISTICS)
    fm_88 = sc2.force_model(degree=8, order=8, point_masses=("Luna", "Sun"))
    prop_88 = sc2.propagator(force_model=fm_88, integrator="PrinceDormand78", max_step=600.0, min_step=0.0, initial_step=60.0, accuracy=1e-13)
    sc2.propagate([sat_mvr2], seconds=MANEUVER_S, step=STEP_S, propagator=prop_88)
    state_pre_burn_mvr = state_km(sat_mvr2)
    epoch_pre_burn_a1mjd = sat_mvr2.epoch
    sc2.maneuver(sat_mvr2, dv=DV_VNB_KM_S, frame="VNB", name="burn1")
    state_post_burn_mvr = state_km(sat_mvr2)
    epoch_post_burn_a1mjd = sat_mvr2.epoch
    if abs(epoch_post_burn_a1mjd - epoch_pre_burn_a1mjd) > 1e-9:
        raise RuntimeError(f"maneuver() must not advance the epoch: {epoch_pre_burn_a1mjd} -> {epoch_post_burn_a1mjd}")
    if state_pre_burn_mvr[0:3] != state_post_burn_mvr[0:3]:
        raise RuntimeError("an impulsive maneuver must not change position")
    sc2.propagate([sat_mvr2], seconds=END_S - MANEUVER_S, step=STEP_S, propagator=prop_88)
    state_final_mvr = state_km(sat_mvr2)

    golden = {
        "name": "demo_two_instance",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "frame": "EarthMJ2000Eq",
        "units": {"position": "km", "velocity": "km/s"},
        "epoch_utc": EPOCH,
        "keplerian": KEPLERIAN,
        "ballistics": BALLISTICS,
        "force_model_demo_flt_pre_fault": {"central_body": "Earth", "gravity": {"file": "JGM2.cof", "degree": 8, "order": 8}, "point_masses": ["Luna", "Sun"], "drag": DRAG_MODEL, "cssi_space_weather_file": DRAG_CSSI_FILE},
        "force_model_demo_flt_post_fault": {"central_body": "Earth", "gravity": {"file": "JGM2.cof", "degree": 8, "order": 0}, "point_masses": ["Luna", "Sun"], "drag": DRAG_MODEL, "cssi_space_weather_file": DRAG_CSSI_FILE},
        "force_model_demo_mvr": {"central_body": "Earth", "gravity": {"file": "JGM2.cof", "degree": 8, "order": 8}, "point_masses": ["Luna", "Sun"], "drag": None},
        "propagator": {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0},
        "fault_epoch_s": FAULT_S,
        "maneuver_epoch_s": MANEUVER_S,
        "end_epoch_s": END_S,
        "dv_vnb_km_s": DV_VNB_KM_S,
        "command_threshold_m": COMMAND_THRESHOLD_M,
        "command_crossing_epoch_s": crossing_epoch_s,
        "command_epoch_s": command_epoch_s,
        "command_rmag_at_crossing_m": rmag_at_crossing_m,
        "cd_before_command": CD_BEFORE_COMMAND,
        "cd_after_command": CD_AFTER_COMMAND,
        "initial_state_demo_flt": x0_flt,
        "initial_state_demo_mvr": x0_mvr,
        "state_at_fault_demo_flt": state_at_fault_flt,
        "state_pre_command_demo_flt": state_pre_command_flt,
        "state_final_demo_flt": state_final_flt,
        "state_final_demo_flt_baseline_uncommanded": state_final_flt_baseline,
        "rmag_at_end_flt_m": rmag_at_end_flt_m,
        "measured_command_arc_divergence_m": arc_divergence_m,
        "state_pre_burn_demo_mvr": state_pre_burn_mvr,
        "state_post_burn_demo_mvr": state_post_burn_mvr,
        "state_final_demo_mvr": state_final_mvr,
        "burn_epoch_a1mjd": epoch_pre_burn_a1mjd,
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_m * 1e-3,
        "tolerance_m_demo_mvr": args.tolerance_m_mvr,
        "tolerance_mps_demo_mvr": args.tolerance_m_mvr * 1e-3,
        "note": (
            "state_final_demo_flt comes from altavista's own from-scratch ForceModel/Propagator "
            "construction (this script's own _force_model_with_drag, a DragForce+JacchiaRoberts "
            "atmosphere referenced through Object.SetReference, the packaged CSSI space-weather "
            "file, plus GravityField/PointMassForce identical to altavista.Scenario.force_model's "
            "own construction), reconstructed at the fault epoch with Order=0 -- the same "
            "mechanism crates/av-kernel/src/drm/fault.rs's apply_gmat_target(\"force_model."
            "gravity_order\") + binding.rs's materialize_gmat re-binding is pinned against, now "
            "extended to also carry drag through the rebind (crate::drm::binding::GmatSystemSpec's "
            "own drag_* fields, threaded through fault::rebind_gmat_spec_at_state's full-spec "
            "clone). The Cd command at command_epoch_s is a live SetField, matching "
            "gmat_sys::DerivativeModel::set_real_parameter's own in-place mutation (question 130: "
            "no segment opens per command). command_epoch_s is independently derived (this "
            "script's own _find_command_epoch_s), not copied from the Rust executor's own output "
            "-- see this module's own docstring for the method and its own ~5 mm/0.1 s timing-"
            "sensitivity bound, which is what tolerance_m is sized against. "
            "state_pre_burn_demo_mvr/state_post_burn_demo_mvr/state_final_demo_mvr come from "
            "altavista.scenario.Scenario.maneuver's own VNB path, not a hand re-derivation of its "
            "V/N/B basis formula -- crates/av-kernel/src/drm/maneuver.rs::dv_to_inertial's "
            "AxesKind::Vnb branch is pinned against this same function field-for-field, exactly as "
            "goldens/gen_leo_1day_maneuver_vnb.py already pins it for the single-instance golden."
        ),
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    golden["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(json.dumps(golden, indent=2, sort_keys=True) + "\n")
    print("wrote", out)
    print("command_epoch_s          ", command_epoch_s)
    print("initial_state_demo_flt   ", x0_flt)
    print("initial_state_demo_mvr   ", x0_mvr)
    print("state_at_fault_demo_flt  ", state_at_fault_flt)
    print("state_final_demo_flt     ", state_final_flt)
    print("rmag_at_end_flt_m        ", rmag_at_end_flt_m)
    print("measured_command_arc_divergence_m", arc_divergence_m, "m (expected order of magnitude: low tens of meters -- see this script's own module docstring)")
    print("state_pre_burn_demo_mvr  ", state_pre_burn_mvr)
    print("state_post_burn_demo_mvr ", state_post_burn_mvr)
    print("state_final_demo_mvr     ", state_final_mvr)


if __name__ == "__main__":
    main()
