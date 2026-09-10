"""F4/F4b worked study (docs/feasibility-plan.md's F4 milestone): drag-sail area versus burn
magnitude, scored on final radius, with the Gates maneuver dispersion drawn -- now TWO grids
over the same DRM.

Authors and launches ``drms/drag_sail_vs_burn.sweep.yaml`` (grid "mvr", F4) and
``drms/drag_sail_vs_burn_flt.sweep.yaml`` (grid "flt", F4b) through ``altavista.feasibility``
(the F3 authoring package) against the SAME, unmodified ``drms/drag_sail_vs_burn.drm.yaml``,
reusing ``drms/demo_two_instance.sos.yaml``/``demo_two_instance.system.yaml``/
``demo_two_instance_ctrl.system.yaml`` completely unmodified, exactly as
``drms/demo_two_instance_sweep.sweep.yaml`` (F1b/F2b's own fixture) does.

Run with:
    cd /Users/probe/code/AltaVista-feasibility
    export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
    .venv/bin/python altavista/feasibility/study_drag_sail_vs_burn.py [--grid mvr|flt|mvrdrag|both] [--out-dir DIR]

Kept as a standalone script (not a pytest test, not a package-internal module) per this task's
own brief: "Author and launch through altavista.feasibility, and keep the script you used".

**Why ``--grid`` defaults to ``flt``, not ``both``.** Grid "mvr" (F4's original grid) is
already committed to disk as ``drms/drag_sail_vs_burn.sweep.yaml`` with a hand-prepended header
comment (see that file's own header, and ``docs/studies/drag-sail-vs-burn.md``'s "Hashes and
reproduction" section) that ``altavista.feasibility.yaml_io.to_yaml`` cannot author (no
header-comment field -- a disclosed gap, not silently patched around). Re-running grid "mvr"'s
own ``emit_sweep_yaml`` call reproduces the identical ``hash:`` (a YAML comment is not part of
the parsed message) but overwrites the file WITHOUT that header comment, exactly as that file's
own header already discloses. So this script's default run (``--grid flt``) authors and runs
only the new grid, leaving the already-committed, already-documented grid "mvr" file untouched.
Pass ``--grid mvr`` or ``--grid both`` explicitly to re-author grid "mvr" too (and remember to
re-prepend its header comment by hand afterward, exactly as F4's own worker did) -- both code
paths are exercised and kept working, not merely present as dead code, so the repository still
has one script that CAN reproduce everything the document claims.

## Grid "flt" (F4b): axis choice and budget arithmetic (written before running; see
## docs/studies/drag-sail-vs-burn.md for the measured wall time against this)

Grid "mvr" (F4) put both axes (DragArea and dv_x) on ``demo_mvr``, and found the DragArea axis
physically inert there -- ``demo_mvr``'s own SOS declaration never sets
``force_model.drag_model`` (``drms/demo_two_instance.sos.yaml``'s own header comment says so in
as many words), so no ``DragForce`` object is ever constructed for that instance
(``crate::drm::binding::materialize_gmat``). Grid "flt" moves the DragArea axis to ``demo_flt``,
the instance that genuinely carries a ``DragForce`` (JacchiaRoberts + the CSSI weather-source
trio, ``demo_two_instance.sos.yaml``'s own ``parameter_overrides`` on ``demo_flt``) -- so this
grid asks the question grid "mvr" could not answer: does the platform show a REAL gradient when
the axis actually reaches a force object.

- DragArea axis: ``demo_flt.spacecraft.DragArea``, values [5.0, 25.0] m^2 -- IDENTICAL to grid
  "mvr"'s own DragArea values (not chosen to make the figure look better; the same two values
  for the same physical reason: 5.0 is ``leo_demo_sys``'s own declared baseline
  (drms/demo_two_instance.system.yaml's spacecraft.DragArea), 25.0 a deployed drag sail (5x
  increase) -- also the same range ``demo_two_instance_sweep.sweep.yaml`` (F1b/F2b's own
  fixture) already uses on this same instance, ``demo_flt``. Keeping the identical range lets
  this grid's own measured DragArea sensitivity be compared apples-to-apples against grid
  "mvr"'s own (null) DragArea result and against the F1b/F2b fixture's own prior measurement on
  the same instance. A drag sail's own deployed area is a real design choice this size range
  reflects: a ~500 kg smallsat (``demo_two_instance.system.yaml``'s own DryMass) with a compact
  undeployed cross-section (5 m^2) and a modestly-sized deployed drag/deorbit sail (25 m^2, e.g.
  in the range of flown CubeSail/InflateSail/AEOLDOS-class deorbit sails, which run from a few
  m^2 to a few tens of m^2) -- not a fictional "whatever number moves the plot most".
- dv_x axis: unchanged from grid "mvr" -- event ``burn1``'s ``dv_x``, [10.0, 20.0, 30.0] m/s
  (see grid "mvr"'s own axis-choice comment above for the full justification; reused verbatim
  so both grids share an identical dv_x axis and the same 18-sample structure).
- ``dispersed: true``, ``monte_carlo_draws=3`` -- identical to grid "mvr", so the two grids are
  directly comparable (same draw count, same dispersion sigmas, same DRM).

Grid: 2 (DragArea: 5.0, 25.0 m^2) x 3 (dv_x: 10.0, 20.0, 30.0 m/s) = 6 points x 3 draws = 18
samples -- IDENTICAL grid size to grid "mvr". Budget arithmetic (written before running):
grid "mvr" (this same DRM, same host, same day, same debug binary, --workers 2, 18 samples)
measured **76.5 s wall time** (docs/studies/drag-sail-vs-burn.md's own "Grid, draws, and the
wall-time budget" section) -- an effective ~4.25 s/sample once process-spawn/GMAT-init overhead
is included, far below the ~11.58 s/sample the F1b/F2b fixture itself measured (grid "mvr"'s own
document attributes the difference to warm OS file caches/binaries from the physics pre-check
run immediately before it). This grid reuses the identical binary, DRM, SOS, and system files
(only the sweep axes differ), started in the same session shortly after grid "mvr"'s own
proof-of-DragArea-inertness scratch run, so caches should again be warm: the PRIMARY estimate is
grid "mvr"'s own measured rate, 18 x 4.25 s ~= 77 s. As a conservative UPPER BOUND, the F1b/F2b
fixture's own slower cold-cache rate (11.58 s/sample) gives 18 x 11.58 s ~= 208 s. Either way,
--workers is kept at 2 (matching both prior studies and the contention rule's instruction not
to raise it past what the host can sustain), and even the conservative 208 s estimate leaves
this grid alone with >4x headroom inside the 15-minute (900 s) whole-of-F4 budget; combined with
grid "mvr"'s own already-measured 76.5 s, the conservative TOTAL F4 (both grids) estimate is
76.5 + 208 = 284.5 s (~4.7 min), still comfortably under 900 s.

## Hypothesis (written before running; docs/studies/drag-sail-vs-burn.md records the measured
## values against this) -- all four axis-to-score sensitivities on grid "flt"

The port wiring is one-directional: ``demo_mvr --(cd_cmd_out, rmag)--> demo_ctrl
--(cd_sail_cmd_out, Cd)--> demo_flt`` (drms/demo_two_instance.sos.yaml's own header comment).
``demo_flt`` only ever CONSUMES a command derived from ``demo_mvr``'s own trajectory; nothing
flows the other way, and ``demo_flt``/``demo_mvr`` are separate GMAT spacecraft objects with
separate state, bound to the same ``leo_demo_sys`` SystemDefinition but not otherwise coupled.

1. ``demo_flt_rmag_at_end`` vs. DragArea axis (direct, on ``demo_flt`` itself, which genuinely
   carries a ``DragForce``): hypothesized to be a REAL, resolved, monotonic effect -- larger
   DragArea means more atmospheric drag means faster orbital decay means SMALLER
   ``demo_flt_rmag_at_end`` at DragArea=25.0 than at DragArea=5.0. Magnitude: order tens of
   metres, based on ``crates/av-sweep/tests/fixture_study.rs``'s own
   ``two_points_differing_only_in_drag_area_produce_different_products`` test comment (measured
   2026-09-09 on this identical instance/axis pair, different dv_x bracket: ~97-101 m between
   DragArea 5.0 and 25.0) and this task's own manager brief (~47 m measured on the same
   instance/range over the identical 7200 s arc) -- both single-digit-to-triple-digit metres,
   not kilometres. Predicted: tens of metres, resolved far above float noise (>>1 m).
2. ``demo_flt_rmag_at_end`` vs. dv_x axis (INDIRECT, through the controller's range-latch
   timing: dv_x changes ``demo_mvr``'s own trajectory shape, which changes WHEN ``demo_mvr``'s
   own rmag output crosses ``demo_ctrl``'s 6,884,300 m threshold, which changes how much of the
   remaining run ``demo_flt`` spends at the post-latch Cd=220.0 vs. the pre-latch baseline
   Cd=2.2): hypothesized SMALL BUT NOT EXACTLY ZERO -- a real coupling path exists (the latch
   timing genuinely depends on ``demo_mvr``'s own dv_x-dependent trajectory), but it is many
   orders of magnitude weaker than the direct DragArea effect above. Magnitude: order 0.001-5 m,
   based on this task's own manager brief ("round 1 measured that indirect coupling at
   0.001-4.2 m") and ``fixture_study.rs``'s own
   ``the_controller_latch_coupling_makes_demo_flt_weakly_but_not_exactly_draw_sensitive`` test
   (measured 0.588 m / 2.397 m on an earlier 2-point grid, same coupling path). No monotonic
   trend is predicted (the coupling is through discrete 10 Hz controller-tick boundary timing,
   not a smooth physical channel), only "small, nonzero, present at every point".
3. ``demo_mvr_rmag_at_end`` vs. dv_x axis (direct, on ``demo_mvr`` itself, the same burn as grid
   "mvr"'s own dv_x axis, on the same instance and the same DRM): hypothesized to reproduce grid
   "mvr"'s own measured result closely, since ``demo_mvr``'s own dynamics do not depend on
   ``demo_flt``'s DragArea at all (one-directional port wiring, above) -- moving the DragArea
   axis to a different instance should not change ``demo_mvr``'s own dv_x sensitivity.
   Predicted: ~2.6 km per m/s of dv_x (grid "mvr"'s own measured slope), ~52 km total across the
   20 m/s bracket, monotonically increasing (more prograde dv_x -> more orbital energy ->
   larger final rmag).
4. ``demo_mvr_rmag_at_end`` vs. DragArea axis (on ``demo_flt``, a DIFFERENT instance):
   hypothesized to be EXACTLY ZERO -- not merely small. Two independent reasons, both checked
   against the source, not assumed: (a) the port wiring is strictly one-directional,
   ``demo_mvr -> demo_ctrl -> demo_flt``; nothing ``demo_flt`` does is ever consumed by
   ``demo_mvr`` or ``demo_ctrl``, so no signal can propagate from ``demo_flt``'s DragArea back
   to ``demo_mvr``'s own trajectory; (b) ``demo_flt`` and ``demo_mvr`` are declared as two
   separate ``SystemInstance`` entries in ``drms/demo_two_instance.sos.yaml``, each materializing
   its own independent GMAT ``Spacecraft``/force-model object graph
   (``crate::drm::binding::materialize_gmat``) -- there is no shared mutable state between them
   at all, unlike DragArea-on-``demo_mvr`` (grid "mvr"'s own null result), which was at least a
   legal override on the SAME instance whose value merely went unread. This is a stronger,
   structural "exactly zero" than grid "mvr"'s: there, DragArea was declared on the scored
   instance itself but unread by any force object; here, DragArea is not even declared on the
   scored instance. Predicted: bit-for-bit identical ``demo_mvr_rmag_at_end`` at DragArea=5.0
   and DragArea=25.0, at every dv_x point and every draw.

## Grid "mvrdrag" (round 3, F5.3 -- question 195's second follow-up): closing the gap
## ``docs/feasibility-plan.md``'s own "Open for the lead" paragraph names ("a study that
## genuinely varies drag on the manoeuvring vehicle needs a new SosConfiguration enabling drag
## on demo_mvr"). Hypotheses written BEFORE running, per this track's own standing rule; see
## ``docs/studies/drag-sail-vs-burn.md`` for what was actually measured against every one of
## these.

Both axes -- DragArea and dv_x -- sit on ``demo_mvr``, exactly like grid "mvr" (F4), but this
grid runs against a NEW DRM (``drms/drag_sail_vs_burn_mvrdrag.drm.yaml``) whose
``sos_configuration_id`` points at a NEW SOS (``drms/demo_two_instance_drag.sos.yaml``) that
gives ``demo_mvr`` the identical four ``force_model.drag_*`` overrides ``demo_flt`` already
carries. So, unlike grid "mvr", the DragArea axis now reaches a real ``DragForce`` on
``demo_mvr`` (``crate::drm::binding::materialize_gmat`` only constructs one ``if let
Some(drag_model) = &spec.drag_model`` -- now true for both instances in this SOS).

**The manager's own reasoning (recorded, then sharpened with arithmetic below), all four
axis-to-score pairs on this grid:**

1. ``demo_mvr_rmag_at_end`` vs. DragArea-on-``demo_mvr`` (direct) -- manager's bracket: small
   but definitively nonzero, order 0.5-20 m. **Sharpened with arithmetic, adopted with a
   tighter point estimate.** ``demo_mvr`` holds Cd=2.2 for the entire 7200 s run (the sail
   command flows only towards ``demo_flt``, never back -- one-directional port wiring,
   ``demo_two_instance.sos.yaml``'s own header). ``demo_flt``, by contrast, spends the DRM's
   own pre-latch segment (t=0..6207.4 s, ``drms/demo_two_instance_ctrl.system.yaml``'s own
   measured crossing time) at Cd=2.2 and the post-latch tail (6207.4..7200 s, 992.6 s) at
   Cd=220.0. If orbital decay scales (to first order, for a small perturbation) with the
   time-integral of Cd*A, the ratio of ``demo_mvr``'s own integral to ``demo_flt``'s own
   integral is:
     demo_mvr: 2.2 * 7200                       = 15840  (Cd*A units of s, per unit A)
     demo_flt: 2.2 * 6207.4 + 220.0 * 992.6      = 13656.28 + 218372.0 = 232028.28
     ratio = 15840 / 232028.28 = 0.0683 (6.83%)
   Grid 2's (F4b) own clean Nominal Check A measured ``demo_flt_rmag_at_end`` move
   -97.980581 m for the identical DragArea 5.0->25.0 step at dv_x=20.0. Scaling that by the
   0.0683 ratio predicts ``demo_mvr_rmag_at_end`` moves roughly **-6.7 m** (DragArea=25.0
   giving a SMALLER final rmag than DragArea=5.0, same decay direction). This sits inside the
   manager's own 0.5-20 m bracket, nearer its lower-middle; expect the measured full-scale
   dispersed value to land within perhaps a factor of 2-3 of -6.7 m (the linear Cd*A*time
   scaling is a first-order approximation -- real atmospheric density is altitude-dependent
   and demo_mvr's own orbit shifts slightly between the two DragArea cases, a second-order
   effect this estimate does not capture), but the sign and the single-digit-to-low-double-digit
   metre order of magnitude are the confident part of the prediction.
2. ``demo_flt_rmag_at_end`` vs. DragArea-on-``demo_mvr`` (indirect, via the controller's
   range-latch timing) -- manager's framing: between exactly zero and a few metres, quite
   possibly exactly zero, a discrete tick-boundary effect. **Adopted, with a physical argument
   for which side of that range to expect.** The controller's threshold (6,884,300 m) sits
   only 578 m below ``demo_mvr``'s own true apoapsis radius (6,884,878 m,
   ``demo_two_instance_ctrl.system.yaml``'s own header), and the measured crossing happens at
   t=6207.4 s -- close to apoapsis, where dr/dt is small. Round 2's own F4b document already
   measured latch-timing jitter driven by a DIFFERENT small perturbation to demo_mvr's own
   trajectory near this identical crossing (draw-to-draw Gates burn dispersion, not DragArea):
   0.04-2.07 m (per-point ``demo_flt_rmag_at_end`` std_dev, F4b's "Draw-to-draw" table). The
   DragArea-driven perturbation predicted in (1) above (order metres on demo_mvr's own rmag) is
   a perturbation of comparable character -- a small change to demo_mvr's own trajectory near
   the same crossing region, just via a different causal channel (added drag vs. burn
   dispersion). Predicted: **either exactly zero (bit-for-bit identical, if the crossing lands
   on the identical 0.1 s tick at both DragArea values) or a shift of the same small order round
   2 already measured for this crossing, 0.04-2.07 m** -- not a large, well-resolved effect
   the way pair 1 above or grid "flt"'s own direct DragArea-on-demo_flt effect (tens to ~140 m)
   are. An exact zero here is a real, publishable result, not a failure, exactly as this task's
   own brief states.
3. ``demo_mvr_rmag_at_end`` vs. dv_x-on-``demo_mvr`` (direct) -- manager's framing: expected to
   closely reproduce grid "mvr"'s own ~2.6 km/(m/s), but NOT exactly, since demo_mvr now
   carries drag. **Adopted, direction stated before measuring:** at every commanded dv_x, this
   grid's own demo_mvr now decays under real atmospheric drag for the full 7200 s (both
   DragArea=5.0 and DragArea=25.0, unlike grid "mvr" where demo_mvr carried no drag at all).
   Predicted direction: this grid's own ``demo_mvr_rmag_at_end`` values are systematically
   **LOWER** than grid "mvr"'s own recorded values at the matching dv_x (orbital decay only
   ever reduces final radius, never increases it) -- most directly comparable at DragArea=5.0
   (grid "mvr"'s own baseline area), where the offset should reflect the cost of adding
   *any* drag at all (a larger comparison than pair 1's own DragArea=5->25 delta within this
   grid). The dv_x SLOPE itself (~2.6 km/(m/s)) is predicted to stay close to grid "mvr"'s own
   measured value, since the burn's own ~52 km delta-v-driven swing is four orders of
   magnitude larger than any drag-driven offset predicted above -- a small constant-ish
   downward shift superimposed on an unchanged slope, not a changed slope.
4. **Wall time.** Grid "mvr" (76.5 s/18 samples = 4.25 s/sample) and grid "flt" (70.4 s/18 =
   3.91 s/sample) both already ran with ONE GMAT instance carrying a JacchiaRoberts atmosphere
   (``demo_flt``, in every SOS either of those two grids ever used). This grid's own new SOS
   gives a SECOND GMAT instance (``demo_mvr``) a JacchiaRoberts atmosphere too -- the marginal
   cost is one more CSSI-backed drag/atmosphere object per sample, not a second scenario from
   scratch. JGM2 8x8 + Sun/Moon gravity dominates per-step cost far more than one drag term
   (the F1b/F2b fixture's own cold-cache rate, 11.58 s/sample, already reflects one
   drag-carrying instance plus cold caches, and grids "mvr"/"flt" both ran 2.7-3x faster than
   that once warm), so a modest, not dramatic, slowdown is predicted. **Primary estimate:**
   grid "mvr"'s own rate scaled by a conservative 1.3x for the second drag instance, 4.25 s *
   1.3 = 5.5 s/sample, 18 * 5.5 = ~100 s (~1.7 min). **Conservative upper bound:** the F1b/F2b
   fixture's own cold-cache rate scaled the same 1.3x, 11.58 s * 1.3 = 15.05 s/sample, 18 *
   15.05 = ~271 s (~4.5 min). Both estimates leave large headroom inside the 900 s (15 min)
   budget -- the run proceeds; see ``docs/studies/drag-sail-vs-burn.md`` for the measured wall
   time against this.
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from altavista.feasibility import SweepAxis, SweepDeclaration, Provenance, emit_sweep_yaml, run_study, discover_repo_root


def build_grid_mvr() -> SweepDeclaration:
    """Grid "mvr" (F4): both axes on ``demo_mvr`` -- unchanged from the original F4 study.
    Kept here (not deleted) so this script can still author+run it on request (``--grid mvr`` /
    ``--grid both``); see this module's own docstring for why the default run skips it."""
    sweep = SweepDeclaration(
        id="drag_sail_vs_burn",
        drm_id="drag_sail_vs_burn_drm",
        axes=[
            SweepAxis.parameter_axis("demo_mvr", "spacecraft.DragArea", values=[5.0, 25.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 20.0, 30.0]),
        ],
        monte_carlo_draws=3,
        dispersed=True,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT", tool="F4 drag-sail-vs-burn study authoring"),
    )
    sweep.validate()
    return sweep


def build_grid_flt() -> SweepDeclaration:
    """Grid "flt" (F4b): DragArea axis moved to ``demo_flt`` (the instance that genuinely
    carries a ``DragForce``), dv_x axis unchanged on ``demo_mvr``'s ``burn1``. See this module's
    own docstring for the full axis-choice and hypothesis writeup."""
    sweep = SweepDeclaration(
        id="drag_sail_vs_burn_flt",
        drm_id="drag_sail_vs_burn_drm",
        axes=[
            SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 25.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 20.0, 30.0]),
        ],
        monte_carlo_draws=3,
        dispersed=True,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT", tool="F4b drag-sail-vs-burn (flt) study authoring"),
    )
    sweep.validate()
    return sweep


def build_grid_mvrdrag() -> SweepDeclaration:
    """Grid "mvrdrag" (round 3, F5.3): both axes on ``demo_mvr`` -- the identical axis shape as
    grid "mvr" -- but run against the new ``drms/drag_sail_vs_burn_mvrdrag.drm.yaml`` /
    ``drms/demo_two_instance_drag.sos.yaml``, which gives ``demo_mvr`` a real ``DragForce`` too
    (grid "mvr"'s own SOS never does). See this module's own docstring, "Grid "mvrdrag""
    section, for the full hypothesis writeup (all four axis-to-score pairs, written and
    committed to disk BEFORE this grid was ever run)."""
    sweep = SweepDeclaration(
        id="drag_sail_vs_burn_mvrdrag",
        drm_id="drag_sail_vs_burn_mvrdrag_drm",
        axes=[
            SweepAxis.parameter_axis("demo_mvr", "spacecraft.DragArea", values=[5.0, 25.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 20.0, 30.0]),
        ],
        monte_carlo_draws=3,
        dispersed=True,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT",
                               tool="round 3 F5.3 drag-sail-vs-burn (mvrdrag) study authoring"),
    )
    sweep.validate()
    return sweep


def run_grid(sweep: SweepDeclaration, sweep_filename: str, root: Path, out_dir: Path, *,
             workers: int, store_dir,
             drm_filename: str = "drag_sail_vs_burn.drm.yaml",
             sos_filename: str = "demo_two_instance.sos.yaml"):
    sweep_path = root / "drms" / sweep_filename
    digest = emit_sweep_yaml(sweep, sweep_path, repo_root=root)
    print(f"[{sweep.id}] sweep hash: {digest}", flush=True)
    print(f"[{sweep.id}] sweep written: {sweep_path}", flush=True)

    drm_path = root / "drms" / drm_filename
    sos_path = root / "drms" / sos_filename
    system_paths = [
        root / "drms" / "demo_two_instance.system.yaml",
        root / "drms" / "demo_two_instance_ctrl.system.yaml",
    ]

    print(f"[{sweep.id}] launching av-sweep: workers={workers} out_dir={out_dir}", flush=True)
    t0 = time.monotonic()
    results = run_study(sweep_path, drm_path, sos_path, system_paths, out_dir, workers=workers, store_dir=store_dir)
    elapsed = time.monotonic() - t0
    print(f"[{sweep.id}] wall time: {elapsed:.1f}s", flush=True)

    print(f"[{sweep.id}] samples: {len(results.samples)}", flush=True)
    for s in results.samples:
        status = "OK" if not s.error else f"FAILED: {s.error[:200]}"
        print(f"  point={s.point_index} draw={s.draw_index} run_id={s.run_id} config_hash={s.config_hash} "
              f"axis_values={dict(s.axis_values)} seeds={dict(s.seeds)} status={status}", flush=True)

    print(f"[{sweep.id}] aggregates: {len(results.aggregates)}", flush=True)
    for a in results.aggregates:
        print(f"  point={a.point_index} name={a.name} mean={a.mean!r} std_dev={a.std_dev!r} "
              f"min={a.min!r} max={a.max!r} draws={a.draws}", flush=True)

    return results, elapsed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--grid", choices=["mvr", "flt", "mvrdrag", "both"], default="flt",
                         help="which grid(s) to author+run (default: flt only -- see this "
                              "module's own docstring for why the default skips re-authoring "
                              "the already-committed grid mvr sweep file; 'mvrdrag' is round "
                              "3's own new grid -- see this module's own docstring, 'Grid "
                              "\"mvrdrag\"' section; 'both' is unchanged from round 2, mvr+flt "
                              "only -- it does not include mvrdrag)")
    parser.add_argument("--out-dir", type=Path, default=None, help="sample workspace root (default: scratchpad/f4b/study_out)")
    parser.add_argument("--store-dir", type=Path, default=None, help="optional FileStudyStore root")
    parser.add_argument("--workers", type=int, default=2)
    args = parser.parse_args()

    root = discover_repo_root()
    out_root = args.out_dir or Path(
        "/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/f4b/study_out"
    )

    # Each entry: kwargs for run_grid beyond (root, workers, store_dir). "mvr"/"flt" pass no
    # drm_filename/sos_filename override, so run_grid's own defaults ("drag_sail_vs_burn.drm.
    # yaml"/"demo_two_instance.sos.yaml") apply -- byte-identical to round 2's own behaviour.
    # "mvrdrag" is the only grid that overrides both, pointing at round 3's own new DRM/SOS.
    grids = []
    if args.grid in ("mvr", "both"):
        grids.append(dict(sweep=build_grid_mvr(), sweep_filename="drag_sail_vs_burn.sweep.yaml",
                           out_dir=out_root / "mvr"))
    if args.grid in ("flt", "both"):
        grids.append(dict(sweep=build_grid_flt(), sweep_filename="drag_sail_vs_burn_flt.sweep.yaml",
                           out_dir=out_root / "flt"))
    if args.grid == "mvrdrag":
        grids.append(dict(sweep=build_grid_mvrdrag(), sweep_filename="drag_sail_vs_burn_mvrdrag.sweep.yaml",
                           out_dir=out_root / "mvrdrag",
                           drm_filename="drag_sail_vs_burn_mvrdrag.drm.yaml",
                           sos_filename="demo_two_instance_drag.sos.yaml"))

    for g in grids:
        sweep = g.pop("sweep")
        sweep_filename = g.pop("sweep_filename")
        out_dir = g.pop("out_dir")
        run_grid(sweep, sweep_filename, root, out_dir, workers=args.workers, store_dir=args.store_dir, **g)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
