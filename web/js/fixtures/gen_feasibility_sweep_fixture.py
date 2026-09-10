"""Generates ``feasibility_sweep_fixture.json``: the wire-shaped ``scenario.sweep`` fixture
web/js/panels_check.mjs's feasibility-panel checks drive, and tests/test_viewer_feasibility_panel.py
reads to build the panels_check.mjs input file (F3b, docs/feasibility-plan.md's F3 milestone).

## Where every number comes from

The axis values, the two score names, the per-sample score VALUES, ``sweepHash`` and
``drmHash`` below are copied verbatim from the feasibility manager's own F3b task brief
(2026-09-09), itself measuring the real ``demo_two_instance_sweep`` study
(``drms/demo_two_instance_sweep.{drm,sweep}.yaml``, docs/feasibility-plan.md's own
"F2b update" / "the fixture study, measured" sections -- the same study, widened to two axes
by question 192(c)'s event-value axis support). Nothing in the ``_MEASURED_SAMPLES`` table
below is invented; this script performs NO GMAT run and needs no GMAT/venv -- it is pure
arithmetic over numbers already measured and handed down by the manager.

``mean``/``stdDev``/``min``/``max`` are computed HERE, by this script, from those measured
per-draw values, using the EXACT formula ``crates/av-sweep/src/aggregate.rs`` documents and
implements (read, not edited, per this task's file-ownership rules): ``mean = sum(values) / n``,
``stdDev`` = the POPULATION standard deviation (``sqrt(sum((x-mean)^2) / n)``, divided by ``n``
not ``n - 1`` -- see that file's own module doc comment for why). Every score here is a Measure
of Effectiveness on this study (no Objective declared, docs/feasibility-plan.md's own study
definition) -- ``passed``/``passFraction`` are therefore ``null`` throughout, never coerced to
``false``/``0`` (the exact misrepresentation ``run_products_panel.js``'s ``objectiveRows`` doc
comment already warns against for the analogous ``RunProducts.scores`` case).

Everything else in this fixture -- ``sweepId``, each sample's ``runId``/``configHash``/``seeds``/
``productsUri`` -- is a STRUCTURAL placeholder, not a measured value: the manager's brief gave
axis values, score values, and the two top-level hashes, but not individual sample identifiers
(these do not exist yet until worker A's ``POST /api/cdm/sweep`` and the F1/F2 sweep executor
actually run this study end-to-end). Each placeholder is deterministic and clearly synthetic
(e.g. ``configHash`` is a real SHA-256 of a descriptive string, not a copied hash) so nobody
mistakes it for a second measured number -- see each field's own comment below.

Run manually to regenerate the fixture (pure Python, no GMAT/venv needed):

    python3 web/js/fixtures/gen_feasibility_sweep_fixture.py
"""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path

OUT_PATH = Path(__file__).resolve().parent / "feasibility_sweep_fixture.json"

# ----------------------------------------------------------------------- measured inputs
# Copied verbatim from the F3b task brief (2026-09-09), which the feasibility manager
# states was measured last round against the real demo_two_instance_sweep study. Axis keys
# already in the wire's own sorted-union order ("demo_flt.spacecraft.DragArea" <
# "event:burn1.dv_x" lexicographically -- 'd' < 'e').
AXIS_KEYS = ["demo_flt.spacecraft.DragArea", "event:burn1.dv_x"]
SCORE_NAMES = ["demo_flt_rmag_at_end", "demo_mvr_rmag_at_end"]  # already sorted ('f' < 'm')

SWEEP_ID = "demo_two_instance_sweep"
SWEEP_HASH = "e03787532ba919ceb42300fede6a8371c08bcfa81ddb00fe9ffd65f0ba5e0236"
DRM_HASH = "499d426e18272fbb361ede2c6edd906fd9f828ac99da154722d56d4e2914433d"

# point -> (DragArea, dv_x, {score_name: [draw0, draw1]}) -- the manager's own measured table.
_MEASURED_SAMPLES = [
    (5.0, 10.0, {
        "demo_mvr_rmag_at_end": [6895836.508, 6895853.128],
        "demo_flt_rmag_at_end": [6870530.236, 6870530.238],
    }),
    (5.0, 30.0, {
        "demo_mvr_rmag_at_end": [6943487.876, 6945526.236],
        "demo_flt_rmag_at_end": [6870508.334, 6870507.611],
    }),
    (25.0, 10.0, {
        "demo_mvr_rmag_at_end": [6896092.128, 6895472.136],
        "demo_flt_rmag_at_end": [6870482.687, 6870484.136],
    }),
    (25.0, 30.0, {
        "demo_mvr_rmag_at_end": [6944981.977, 6947523.490],
        "demo_flt_rmag_at_end": [6870371.222, 6870367.018],
    }),
]
UNIT = "m"  # rmag ("radius magnitude") scores are a position magnitude -- metres, matching
            # every other position-scored value already in this repo's fixtures (e.g.
            # tests/fixtures/demo_two_instance.runproducts.bin's own demo_flt_rmag_at_end).


def _population_aggregate(values: list[float]) -> dict:
    """crates/av-sweep/src/aggregate.rs's exact formula (read, not edited): plain
    left-to-right mean, POPULATION std dev (divide by n, never n-1)."""
    n = len(values)
    mean = sum(values) / n
    sq_diff_sum = sum((v - mean) ** 2 for v in values)
    std_dev = math.sqrt(sq_diff_sum / n)
    return {"draws": n, "mean": mean, "stdDev": std_dev, "min": min(values), "max": max(values), "passFraction": None}


def _placeholder_config_hash(point_index: int, draw_index: int) -> str:
    """A real SHA-256, but of a plainly-synthetic descriptive string -- distinguishes this
    placeholder from a genuine per-sample config hash (which would hash the actual DRM/SOS/
    SystemDefinition bytes, docs/feasibility-plan.md's own 'config_hash scope' decision) at a
    glance, while still being a well-formed 64-hex-char string like a real one."""
    return hashlib.sha256(f"feasibility-fixture-placeholder-config-hash:p{point_index}:d{draw_index}".encode()).hexdigest()


points = []
samples_by_point = []
for point_index, (drag_area, dv_x, scores_by_name) in enumerate(_MEASURED_SAMPLES):
    axis_values = {"demo_flt.spacecraft.DragArea": drag_area, "event:burn1.dv_x": dv_x}
    samples = []
    for draw_index in range(2):
        samples.append({
            "drawIndex": draw_index,
            # Structural placeholder (not measured) -- see module docstring.
            "runId": f"{SWEEP_ID}-p{point_index}-d{draw_index}",
            "configHash": _placeholder_config_hash(point_index, draw_index),
            # Structural placeholder uint64-as-decimal-string seeds (not measured) -- the
            # real derivation is SHA-256(base_seed, sweep_hash, point, draw, key), truncated
            # to 64 bits (docs/feasibility-plan.md's F1 milestone); this fixture does not
            # have a real base seed to derive from, so it uses a distinguishable placeholder
            # that is still a syntactically valid uint64 decimal string.
            "seeds": {"burn1": str(1_000_000_000_000 + point_index * 1000 + draw_index)},
            "scores": {
                name: {"value": values[draw_index], "unit": UNIT, "passed": None}
                for name, values in scores_by_name.items()
            },
            "productsUri": f"runs/{SWEEP_ID}/p{point_index}/d{draw_index}/products.bin",
            "error": "",
        })
    points.append({"pointIndex": point_index, "axisValues": axis_values, "samples": samples})
    samples_by_point.append((point_index, scores_by_name))

aggregates = []
for point_index, scores_by_name in samples_by_point:
    for name in SCORE_NAMES:
        agg = _population_aggregate(scores_by_name[name])
        aggregates.append({"name": name, "pointIndex": point_index, **agg})
# Sorted by (name, pointIndex) per the wire contract.
aggregates.sort(key=lambda a: (a["name"], a["pointIndex"]))

sweep = {
    "sweepId": SWEEP_ID,
    "sweepHash": SWEEP_HASH,
    "drmHash": DRM_HASH,
    "axisKeys": AXIS_KEYS,
    "scoreNames": SCORE_NAMES,
    "points": points,
    "aggregates": aggregates,
}

scenario = {
    "name": f"sweep:{SWEEP_ID}",
    "meta": {
        "sweepSource": "SweepResults",
        "sweepId": SWEEP_ID,
        "sweepHash": SWEEP_HASH,
        "drmHash": DRM_HASH,
    },
    "sweep": sweep,
}

fixture = {
    "_comment": "Generated by web/js/fixtures/gen_feasibility_sweep_fixture.py -- see that "
                "script's own module docstring for exactly which numbers are measured "
                "(axis values, score values, sweepHash, drmHash) vs. structural placeholders "
                "(runId/configHash/seeds/productsUri).",
    "scenario": scenario,
}

OUT_PATH.write_text(json.dumps(fixture, indent=1) + "\n")
print(f"wrote {OUT_PATH} ({len(points)} points, {sum(len(p['samples']) for p in points)} samples, "
      f"{len(aggregates)} aggregates)")
for a in aggregates:
    print(f"  {a['name']} @ point {a['pointIndex']}: mean={a['mean']:.6f} stdDev={a['stdDev']:.6f} "
          f"min={a['min']:.6f} max={a['max']:.6f}")
