"""Generate the golden arc `leo_1day_jgm2_8x8_sunmoon.json` with GMAT's own propagator (ADR-002).

Run explicitly, never from a test:  .venv/bin/python goldens/gen_leo_1day.py --reason "..."
The golden pins: initial state (EarthMJ2000Eq, km, km/s), GMAT's end state after 86400 s with
PrinceDormand78 at Accuracy 1e-13 / MaxStep 600, the force model, and the GMAT version.
Consumers (crates/gmat-sys, native models) integrate the same arc over GetDerivatives and
must land within `tolerance_m`.

M3.2 extension: also records GMAT's own 42-state (Cartesian + row-major STM) propagation of
the *same* arc (`PropagationStateManager::SetProperty("STM", sc)` before `PrepareInternals()`,
per `GMAT_API_Cookbook`'s "STM and Covariance Propagation" chapter and
`docs/teamlog/adr-002-amendment-draft-stm.md`), plus a declared P0 (SI) and the covariance it
propagates to at t1 (`P(t1) = Phi(t0,t1) P0 Phi(t0,t1)^T`). `crates/av-kernel`'s
`golden_acceptance.rs` and `services/gmat-service`'s covariance test both pin against these new
fields directly -- this is GMAT's own propagator computing the STM (depth 1's own mechanism,
ADR-002), the *reference* the kernel's independently-integrated STM (depth 2, `gmat-sys`) is
compared against, not the kernel's own mechanism (which never reads GMAT's STM back after the
fact -- see the second ADR-002 amendment).

M4.3 extension: `--drag-srp` switches to a second, independent golden (own output file,
`leo_1day_jgm2_8x8_sunmoon_drag_srp.json`, the plain golden above is never touched) on a lower
orbit so drag and SRP actually perturb the arc, with `DragForce` (`JacchiaRoberts` atmosphere --
see the flag's help for why) and `SolarRadiationPressure` (spherical model) added to the force
model, to measure whether `GetDerivatives` fills their A-matrix contributions when the STM is
requested (ADR-002 second amendment's open question; `docs/teamlog/
adr-002-amendment-draft-drag-srp.md`).
"""
import argparse, json, sys, datetime, hashlib
from pathlib import Path

sys.path.insert(1, "/Users/probe/code/AltaVista/GMAT R2026a/bin")
import gmatpy as gmat
gmat.Setup("/Users/probe/code/AltaVista/GMAT R2026a/bin/api_startup_file.txt")

EPOCH = "01 Jan 2026 00:00:00.000"
FORCE_MODEL = {"central_body": "Earth", "gravity": {"file": "JGM2.cof", "degree": 8, "order": 8},
               "point_masses": ["Luna", "Sun"], "srp": False, "drag": None}
SAT = {"SMA": 6878.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
       "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
PROP = {"integrator": "PrinceDormand78", "accuracy": 1e-13, "initial_step_s": 60.0, "min_step_s": 0.0, "max_step_s": 600.0}
DURATION_S = 86400.0

# M4.3: a second orbit, ~250 km altitude circular LEO (SMA 6628 km vs. the plain golden's
# 6878 km / ~500 km altitude) so atmospheric density -- and hence drag -- is large enough to
# move the trajectory measurably over one day (measured below: ~9 km of along-track drift
# attributable to drag beyond the plain golden's gravity+point-mass-only dynamics). Same
# inclination/spacecraft physical parameters as the plain golden so only the force model and
# altitude differ.
FORCE_MODEL_DRAG_SRP = {"central_body": "Earth", "gravity": {"file": "JGM2.cof", "degree": 8, "order": 8},
                         "point_masses": ["Luna", "Sun"], "srp": True, "drag": "JacchiaRoberts"}
SAT_DRAG_SRP = {"SMA": 6628.0, "ECC": 0.001, "INC": 51.6, "RAAN": 30.0, "AOP": 0.0, "TA": 0.0,
                "DryMass": 500.0, "Cd": 2.2, "Cr": 1.8, "DragArea": 5.0, "SRPArea": 5.0}
# Declared P0 (SI, question 11: covariance is always explicit -- this is the one place it is
# declared for this golden). Diagonal: (100 m)^2 position variance, (0.1 m/s)^2 velocity
# variance -- round numbers, not fit to anything, chosen only to be non-degenerate (unequal
# position/velocity scale) so a row/column-major or unit-conversion bug would not cancel out.
P0_SI_DIAG = [100.0 ** 2] * 3 + [0.1 ** 2] * 3
# Spacecraft physical properties that every seed must carry (question 81).
BALLISTIC_FIELDS = ("DryMass", "Cd", "Cr", "DragArea", "SRPArea")

# The golden's own purpose statement (round-4 pattern also used by gen_leo_1day_rmag.py/
# gen_leo_1day_planetodetic_lon.py/gen_covariance_bodyfixed_leo_2h.py, ADR-002's goldens rule:
# the file's "reason" field states why the golden exists, not why it was last regenerated).
# Fixed here rather than taken from --reason so a tolerance-only or metadata-only regeneration
# never erases it -- question 230's own defect (round 4's task) was exactly this generator
# overwriting "reason" with each run's own note. Neither leo_1day_jgm2_8x8_sunmoon.json nor
# leo_1day_jgm2_8x8_sunmoon_drag_srp.json carries a separate "note" field with an independent
# purpose narrative (unlike gen_leo_1day_rmag.py's golden), and each has exactly one commit in
# its git history (474b76a932a76cd96a5f45f20832f3e7af5b48c2, "Initial import of the Alta Vista
# platform" -- a squashed import, so no earlier, truer "reason" exists to recover instead); the
# text below is the only "reason" each golden has ever carried, restored verbatim rather than
# replaced.
GOLDEN_REASON = (
    "question 81: STM spacecraft now carries the ballistic properties; plain arc unaffected "
    "physically, regenerated for the recorded ballistics field"
)
GOLDEN_REASON_DRAG_SRP = (
    "question 81: STM spacecraft flew with GMAT default ballistics (850 kg, 15 m2, 1 m2); now "
    "identical to the 6-state vehicle"
)

# Round 5, question 230's stm-block follow-up: the six tolerance constants
# crates/av-orbital/tests/stm_goldens.rs's native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden
# asserts, moved into this golden's own "stm" block as a single golden_comparison_tolerance
# object with six named keys (not at this file's own top level, where they would be confusable
# with the depth-2 gmat-sys tolerance_m/tolerance_mps already there -- a different comparison
# entirely, see that test's own module doc comment). Each key is itself an object with its own
# value/unit/source (rather than one shared unit/source for all six) because the six constants
# bound genuinely different quantities in different units (m, m/s, a dimensionless STM-element
# max-abs-diff, a relative STM-element max-diff, a dimensionless det(Phi) abs-diff, and a
# relative Frobenius-norm covariance error) -- a single shared unit/source would either be wrong
# for five of the six or reduced to a vague catch-all; six per-key objects let each one say
# exactly what it bounds. All six have been asserted, unchanged, since commit 101b81e ("N4: the
# native state transition matrix, pinned against GMAT's own 42-state run") -- confirmed with
# `git log -S` against the exact "NAME: f64 = VALUE" text for each constant, not guessed.
STM_TOLERANCE = {
    "traj_tolerance_m": {
        "value": 4e-3,
        "unit": "m",
        "source": (
            "the constant crates/av-orbital/tests/stm_goldens.rs's "
            "native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden has asserted since commit "
            "101b81e (N4: the native state transition matrix, pinned against GMAT's own "
            "42-state run), tests/stm_goldens.rs:148 -- bounds the native trajectory position "
            "residual vs this golden's own final_state, set just above the measured residual "
            "(2.7646366405146665e-3 m, debug build, that commit's own host), never loosened "
            "past what was measured"
        ),
    },
    "traj_tolerance_mps": {
        "value": 4e-6,
        "unit": "m/s",
        "source": (
            "the constant crates/av-orbital/tests/stm_goldens.rs's "
            "native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden has asserted since commit "
            "101b81e (N4: the native state transition matrix, pinned against GMAT's own "
            "42-state run), tests/stm_goldens.rs:149 -- bounds the native trajectory velocity "
            "residual vs this golden's own final_state, set just above the measured residual "
            "(3.063141046268765e-6 m/s, debug build, that commit's own host), never loosened "
            "past what was measured"
        ),
    },
    "stm_max_abs_tolerance": {
        "value": 1e-4,
        "unit": "dimensionless (STM element max-abs-diff)",
        "source": (
            "the constant crates/av-orbital/tests/stm_goldens.rs's "
            "native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden has asserted since commit "
            "101b81e (N4: the native state transition matrix, pinned against GMAT's own "
            "42-state run), tests/stm_goldens.rs:176 -- bounds the max absolute disagreement "
            "over all 36 elements of the native STM vs this golden's own final_stm, set just "
            "above the measured value (6.392269142452278e-5, debug build, that commit's own "
            "host), never loosened past what was measured"
        ),
    },
    "stm_max_rel_tolerance": {
        "value": 1e-6,
        "unit": "relative (STM element max-rel-diff)",
        "source": (
            "the constant crates/av-orbital/tests/stm_goldens.rs's "
            "native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden has asserted since commit "
            "101b81e (N4: the native state transition matrix, pinned against GMAT's own "
            "42-state run), tests/stm_goldens.rs:177 -- bounds the max relative disagreement "
            "over all 36 elements of the native STM vs this golden's own final_stm, set just "
            "above the measured value (5.58039616036866e-7, debug build, that commit's own "
            "host), never loosened past what was measured"
        ),
    },
    "det_phi_tolerance": {
        "value": 1e-9,
        "unit": "dimensionless (det(Phi) abs-diff)",
        "source": (
            "the constant crates/av-orbital/tests/stm_goldens.rs's "
            "native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden has asserted since commit "
            "101b81e (N4: the native state transition matrix, pinned against GMAT's own "
            "42-state run), tests/stm_goldens.rs:187 -- bounds the disagreement between the "
            "native det(Phi) and this golden's own stm.det_phi_t1, set just above the measured "
            "value (1.262279170077818e-11, debug build, that commit's own host), never loosened "
            "past what was measured"
        ),
    },
    "cov_rel_tolerance": {
        "value": 1e-6,
        "unit": "relative (Frobenius-norm propagated-covariance error)",
        "source": (
            "the constant crates/av-orbital/tests/stm_goldens.rs's "
            "native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden has asserted since commit "
            "101b81e (N4: the native state transition matrix, pinned against GMAT's own "
            "42-state run), tests/stm_goldens.rs:201 -- bounds the relative Frobenius-norm "
            "disagreement between the native-propagated covariance (via "
            "av_dynamics::propagate_covariance) and this golden's own stm.cov_t1_si, set just "
            "above the measured value (5.748873148206185e-10, debug build, that commit's own "
            "host), never loosened past what was measured"
        ),
    },
}


def _matmul6(a, b):
    return [sum(a[i * 6 + k] * b[k * 6 + j] for k in range(6)) for i in range(6) for j in range(6)]


def _transpose6(a):
    return [a[j * 6 + i] for i in range(6) for j in range(6)]


def _propagate_covariance(phi, p0, n=6):
    """P(t) = Phi P0 Phi^T, explicitly symmetrized; returns (flat row-major P, max
    pre-symmetrization asymmetry) -- mirrors `av_dynamics::propagate_covariance` and
    `gmat_service.model._propagate_covariance` exactly (three independent implementations of
    the same three-line formula, one per language/binding, is intentional: a bug shared by
    copy-paste would not be caught by any of them agreeing)."""
    tmp = _matmul6(phi, p0)
    p = _matmul6(tmp, _transpose6(phi))
    max_asym = max(abs(p[i * n + j] - p[j * n + i]) for i in range(n) for j in range(i + 1, n))
    sym = [0.5 * (p[i * n + j] + p[j * n + i]) for i in range(n) for j in range(n)]
    return sym, max_asym


def _det6(m):
    import numpy as np
    return float(np.linalg.det(np.array(m).reshape(6, 6)))


def _add_forces(fm, force_model, prefix):
    """Populate `fm` (a ForceModel) from a force-model config dict: gravity, point masses,
    and -- M4.3 -- drag (`DragForce` + an atmosphere object) and spherical SRP, exactly as
    `altavista.scenario.Scenario.force_model()` builds them through the API."""
    g = gmat.Construct("GravityField", f"{prefix}_Grav"); g.SetField("BodyName", "Earth")
    g.SetField("PotentialFile", force_model["gravity"]["file"]); g.SetField("Degree", force_model["gravity"]["degree"]); g.SetField("Order", force_model["gravity"]["order"])
    fm.AddForce(g)
    for b in force_model["point_masses"]:
        pm = gmat.Construct("PointMassForce", f"{prefix}_PM_{b}"); pm.SetField("BodyName", b); fm.AddForce(pm)
    if force_model["drag"]:
        df = gmat.Construct("DragForce", f"{prefix}_Drag")
        df.SetField("AtmosphereModel", force_model["drag"])
        atmos = gmat.Construct(force_model["drag"], f"{prefix}_Atmos")
        df.SetReference(atmos)
        fm.AddForce(df)
    if force_model["srp"]:
        fm.AddForce(gmat.Construct("SolarRadiationPressure", f"{prefix}_SRP"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--reason",
        required=True,
        help=(
            "why this run regenerates the golden; recorded in golden_regeneration_reason. "
            "The file's own 'reason' field is fixed above (the golden's purpose) and is never "
            "overwritten by this flag -- see GOLDEN_REASON/GOLDEN_REASON_DRAG_SRP."
        ),
    )
    ap.add_argument("--tolerance-m", type=float, default=0.05)
    ap.add_argument(
        "--drag-srp", action="store_true",
        help="M4.3: generate the second golden (own output file, "
             "leo_1day_jgm2_8x8_sunmoon_drag_srp.json; never touches the plain golden) on a "
             "~250 km-altitude LEO with DragForce (JacchiaRoberts atmosphere -- both "
             "JacchiaRoberts and MSISE90 gmat.Construct() headlessly with the packaged "
             "SpaceWeather-All-v1.2.txt data pack; JacchiaRoberts is chosen as the more "
             "commonly used LEO drag model in GMAT's own tutorials, no other reason) and "
             "spherical SolarRadiationPressure added to the force model, to measure whether "
             "GetDerivatives fills their A-matrix contributions (ADR-002 second amendment's "
             "open question).",
    )
    args = ap.parse_args()

    force_model = FORCE_MODEL_DRAG_SRP if args.drag_srp else FORCE_MODEL
    sat_cfg = SAT_DRAG_SRP if args.drag_srp else SAT
    golden_name = "leo_1day_jgm2_8x8_sunmoon_drag_srp" if args.drag_srp else "leo_1day_jgm2_8x8_sunmoon"

    sat = gmat.Construct("Spacecraft", "Golden")
    sat.SetField("DateFormat", "UTCGregorian"); sat.SetField("Epoch", EPOCH)
    sat.SetField("CoordinateSystem", "EarthMJ2000Eq"); sat.SetField("DisplayStateType", "Keplerian")
    for k, v in sat_cfg.items():
        sat.SetField(k, v)
    fm = gmat.Construct("ForceModel", "GoldenFM"); fm.SetField("CentralBody", force_model["central_body"])
    _add_forces(fm, force_model, "Golden")
    prop = gmat.Construct("Propagator", "GoldenProp"); gator = gmat.Construct(PROP["integrator"], "GoldenGator")
    prop.SetReference(gator); prop.SetReference(fm)
    prop.SetField("InitialStepSize", PROP["initial_step_s"]); prop.SetField("Accuracy", PROP["accuracy"])
    prop.SetField("MinStep", PROP["min_step_s"]); prop.SetField("MaxStep", PROP["max_step_s"])
    gmat.Initialize(); prop.AddPropObject(sat); prop.PrepareInternals(); gator = prop.GetPropagator()
    x0 = [float(v) for v in list(gator.GetState())[:6]]
    a1_epoch = sat.GetEpoch()
    elapsed = 0.0
    while elapsed < DURATION_S - 1e-9:
        dt = min(PROP["max_step_s"], DURATION_S - elapsed)
        # Step(dt) returns False after MaxStepAttempts substep attempts, leaving the state
        # partially advanced (lead review 2026-09-02; tests/test_gmat_step_return.py).
        if not gator.Step(dt):
            raise RuntimeError(f"Propagator.Step({dt}) returned False at elapsed={elapsed}: raise MaxStepAttempts or use smaller chunks")
        elapsed += dt
    x1 = [float(v) for v in list(gator.GetState())[:6]]

    # --- STM: GMAT's own 42-state propagation of the *same* arc (same x0/epoch/force model),
    # seeded as a fresh Cartesian spacecraft (never given Keplerian elements -- GMAT refuses to
    # mix state-type field writes on one object outside a mission sequence) so
    # PropagationStateManager::SetProperty("STM", sc) sizes the state at 42 before
    # PrepareInternals() builds it (GMAT_API_Cookbook's "STM and Covariance Propagation"
    # chapter). This is GMAT's own propagator computing the STM -- depth 1's own mechanism
    # (ADR-002) -- and is recorded here as the *reference* the kernel's independently-
    # integrated STM (depth 2) is measured against, not the mechanism the kernel itself uses. ---
    stm_sat = gmat.Construct("Spacecraft", "GoldenStmSat")
    stm_sat.SetField("DateFormat", "A1ModJulian"); stm_sat.SetField("Epoch", repr(a1_epoch))
    stm_sat.SetField("CoordinateSystem", "EarthMJ2000Eq"); stm_sat.SetField("DisplayStateType", "Cartesian")
    for field, v in zip(("X", "Y", "Z", "VX", "VY", "VZ"), x0):
        stm_sat.SetField(field, v)
    # The ballistic properties travel with the seed (question 81). Without this the STM
    # spacecraft flew with GMAT's defaults (850 kg, 15 m^2 drag area, 1 m^2 SRP area) while
    # the 6-state run used sat_cfg's, which put the drag+SRP golden's STM/covariance on a
    # different vehicle than its own final_state: 467,025.793 m apart after one day.
    for k in BALLISTIC_FIELDS:
        stm_sat.SetField(k, sat_cfg[k])
    stm_fm = gmat.Construct("ForceModel", "GoldenStmFM"); stm_fm.SetField("CentralBody", force_model["central_body"])
    _add_forces(stm_fm, force_model, "GoldenStm")
    stm_prop = gmat.Construct("Propagator", "GoldenStmProp"); stm_gator_obj = gmat.Construct(PROP["integrator"], "GoldenStmGator")
    stm_prop.SetReference(stm_gator_obj); stm_prop.SetReference(stm_fm)
    stm_prop.SetField("InitialStepSize", PROP["initial_step_s"]); stm_prop.SetField("Accuracy", PROP["accuracy"])
    stm_prop.SetField("MinStep", PROP["min_step_s"]); stm_prop.SetField("MaxStep", PROP["max_step_s"])
    gmat.Initialize(); stm_prop.AddPropObject(stm_sat)
    stm_psm = stm_prop.GetPropStateManager()
    if not stm_psm.SetProperty("STM", stm_sat):
        raise RuntimeError('PropagationStateManager.SetProperty("STM", sc) returned False')
    stm_prop.PrepareInternals(); stm_gator = stm_prop.GetPropagator()
    # Both vehicles must be physically identical, not only kinematically (question 81).
    for k in ("TotalMass", "DragArea", "SRPArea", "Cd", "Cr"):
        a, b = sat.GetRealParameter(k), stm_sat.GetRealParameter(k)
        if a != b:
            raise RuntimeError(f"STM spacecraft {k} = {b} differs from the 6-state spacecraft's {a}")
    state0_42 = [float(v) for v in list(stm_gator.GetState())]
    if len(state0_42) != 42:
        raise RuntimeError(f"expected a 42-element STM-augmented state, got {len(state0_42)}")
    max_abs_identity_error_t0 = max(abs(state0_42[6 + i * 6 + j] - (1.0 if i == j else 0.0)) for i in range(6) for j in range(6))

    elapsed = 0.0
    while elapsed < DURATION_S - 1e-9:
        dt = min(PROP["max_step_s"], DURATION_S - elapsed)
        # Same question-77 rule as the plain 6-state loop above: check Step()'s return value,
        # chunked at MaxStep.
        if not stm_gator.Step(dt):
            raise RuntimeError(f"STM Propagator.Step({dt}) returned False at elapsed={elapsed}: raise MaxStepAttempts or use smaller chunks")
        elapsed += dt
    state1_42 = [float(v) for v in list(stm_gator.GetState())]
    final_stm = state1_42[6:42]  # row-major, index row*6+col == STM(row,col)

    # Independent Liouville/symplecticity check (a conservative gravitational system: det(Phi)
    # should stay near 1), via numpy rather than any hand-rolled or GMAT routine.
    det_phi_t1 = _det6(final_stm)

    # Propagate the declared P0 through this Phi -- the golden's own "expected answer" for
    # both av-kernel's and gmat-service's covariance tests.
    p0_si = [0.0] * 36
    for i in range(6):
        p0_si[i * 6 + i] = P0_SI_DIAG[i]
    cov_t1_si, cov_asymmetry = _propagate_covariance(final_stm, p0_si)

    golden = {
        "name": golden_name,
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": GOLDEN_REASON_DRAG_SRP if args.drag_srp else GOLDEN_REASON,
        "golden_regeneration_reason": args.reason,
        "gmat_version": "R2026a",
        "frame": "EarthMJ2000Eq", "units": {"position": "km", "velocity": "km/s"},
        "epoch_utc": EPOCH, "epoch_a1mjd": a1_epoch,
        "spacecraft": sat_cfg, "force_model": force_model, "propagator": PROP,
        "duration_s": DURATION_S,
        "initial_state": x0, "final_state": x1,
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_m * 1e-3,
        # M3.2 extension: GMAT's own 42-state (Cartesian + STM) propagation of the same arc.
        "stm": {
            "note": "GMAT's own PropagationStateManager('STM') propagation of the same x0/epoch/force model/propagator settings above; row-major, index row*6+col is Phi(row,col); this is the reference the kernel's independently-integrated STM (gmat-sys depth 2) is compared against, not the mechanism it uses (ADR-002 second amendment).",
            "final_stm": final_stm,
            "max_abs_identity_error_t0": max_abs_identity_error_t0,
            "det_phi_t1": det_phi_t1,
            "p0_si_diag": P0_SI_DIAG,
            "p0_si": p0_si,
            "cov_t1_si": cov_t1_si,
            "cov_t1_pre_symmetrization_asymmetry": cov_asymmetry,
            "cov_units": "SI (m^2 for position-position block, m^2/s^2 for velocity-velocity, m^2/s for the cross block)",
            "stm_spacecraft_ballistics": {k: stm_sat.GetRealParameter(k) for k in ("TotalMass", "DragArea", "SRPArea", "Cd", "Cr")},
        },
    }
    # Round 5, question 230's stm-block follow-up (see STM_TOLERANCE's own comment above): the
    # six tolerances crates/av-orbital/tests/stm_goldens.rs asserts as bare Rust constants,
    # moved here -- inside the "stm" block, not this file's own top level, where they would be
    # confusable with tolerance_m/tolerance_mps above (the gmat-sys depth-2 path's tolerances,
    # a different comparison entirely). Plain golden only: that test asserts them against
    # leo_1day_jgm2_8x8_sunmoon.json's own final_stm/det_phi_t1/cov_t1_si, and their `source`
    # strings quote residuals measured on that arc. No test asserts them against the drag+SRP
    # golden (its native residual is a different, recorded number -- goldens/README.md's
    # "Jacchia-Roberts / M5 residual"), so writing them there would put false provenance ("this
    # golden's own final_stm") into a file that is not the one they bound.
    if not args.drag_srp:
        golden["stm"]["golden_comparison_tolerance"] = STM_TOLERANCE
    body = json.dumps(golden, indent=2, sort_keys=True)
    golden["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(json.dumps(golden, indent=2, sort_keys=True) + "\n")
    print("wrote", out, "final state", x1)


if __name__ == "__main__":
    main()
