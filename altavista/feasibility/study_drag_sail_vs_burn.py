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
    .venv/bin/python altavista/feasibility/study_drag_sail_vs_burn.py [--grid mvr|flt|both] [--out-dir DIR]

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


def run_grid(sweep: SweepDeclaration, sweep_filename: str, root: Path, out_dir: Path, *,
             workers: int, store_dir):
    sweep_path = root / "drms" / sweep_filename
    digest = emit_sweep_yaml(sweep, sweep_path, repo_root=root)
    print(f"[{sweep.id}] sweep hash: {digest}", flush=True)
    print(f"[{sweep.id}] sweep written: {sweep_path}", flush=True)

    drm_path = root / "drms" / "drag_sail_vs_burn.drm.yaml"
    sos_path = root / "drms" / "demo_two_instance.sos.yaml"
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
    parser.add_argument("--grid", choices=["mvr", "flt", "both"], default="flt",
                         help="which grid(s) to author+run (default: flt only -- see this "
                              "module's own docstring for why the default skips re-authoring "
                              "the already-committed grid mvr sweep file)")
    parser.add_argument("--out-dir", type=Path, default=None, help="sample workspace root (default: scratchpad/f4b/study_out)")
    parser.add_argument("--store-dir", type=Path, default=None, help="optional FileStudyStore root")
    parser.add_argument("--workers", type=int, default=2)
    args = parser.parse_args()

    root = discover_repo_root()
    out_root = args.out_dir or Path(
        "/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/f4b/study_out"
    )

    grids = []
    if args.grid in ("mvr", "both"):
        grids.append((build_grid_mvr(), "drag_sail_vs_burn.sweep.yaml", out_root / "mvr"))
    if args.grid in ("flt", "both"):
        grids.append((build_grid_flt(), "drag_sail_vs_burn_flt.sweep.yaml", out_root / "flt"))

    for sweep, filename, out_dir in grids:
        run_grid(sweep, filename, root, out_dir, workers=args.workers, store_dir=args.store_dir)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
