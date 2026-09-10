"""``altavista.feasibility``: declare a parameter sweep in Python, emit its YAML, launch
``av-sweep``, and load the resulting ``altavista.v1.SweepResults`` (F3,
``docs/feasibility-plan.md``).

Quick start::

    from altavista.feasibility import SweepAxis, SweepDeclaration, emit_sweep_yaml, run_study

    sweep = SweepDeclaration(
        id="demo_two_instance_sweep",
        drm_id="demo_two_instance_sweep_drm",
        axes=[
            SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 25.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 30.0]),
        ],
        monte_carlo_draws=2,
    )
    emit_sweep_yaml(sweep, "my_sweep.yaml")   # fills sweep.hash in too
    results = run_study("my_sweep.yaml", drm_path, sos_path, [system_path], "out/", workers=2)

See ``altavista/feasibility/declare.py`` for why ``SweepDeclaration`` has no ``seeds`` field,
and ``altavista/feasibility/hashing.py`` for how the canonical hash is obtained and what that
choice does and does not prove.
"""
from .declare import EVENT_AXIS_KEY_PREFIX, Provenance, SweepAxis, SweepDeclaration
from .errors import (
    AmbiguousAxisDeclarationError,
    AxisBothTargetsError,
    AxisMissingTargetError,
    AxisStepsBelowMinimumError,
    AxisTargetError,
    FeasibilityBinaryNotFoundError,
    FeasibilityError,
    FeasibilityHashError,
    FeasibilityRunError,
    ReservedAxisKeyPrefixError,
)
from .hashing import compute_sweep_hash, emit_sweep_yaml
from .paths import discover_repo_root
from .runner import find_av_sweep_binary, load_sweep_results, run_study
from .yaml_io import to_yaml, to_yaml_dict

__all__ = [
    "SweepAxis", "SweepDeclaration", "Provenance", "EVENT_AXIS_KEY_PREFIX",
    "to_yaml", "to_yaml_dict",
    "compute_sweep_hash", "emit_sweep_yaml",
    "find_av_sweep_binary", "run_study", "load_sweep_results",
    "discover_repo_root",
    "FeasibilityError", "AxisTargetError", "AxisMissingTargetError", "AxisBothTargetsError",
    "ReservedAxisKeyPrefixError", "AmbiguousAxisDeclarationError", "AxisStepsBelowMinimumError",
    "FeasibilityHashError", "FeasibilityBinaryNotFoundError", "FeasibilityRunError",
]
