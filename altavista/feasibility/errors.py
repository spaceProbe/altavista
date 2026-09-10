"""Exception types for :mod:`altavista.feasibility` (F3, ``docs/feasibility-plan.md``).

One base class (:class:`FeasibilityError`), plus a handful of narrow subclasses so a caller
(or a test) can catch precisely the failure it cares about instead of string-matching a
message. Mirrors this repository's existing convention (``altavista/cdm.py``'s
``CdmAdapterError``, ``altavista/frames.py``'s frame errors): a typed refusal, never a bare
``ValueError`` or ``Exception``, and never silently swallowed.
"""
from __future__ import annotations


class FeasibilityError(Exception):
    """Base class for every error this package raises."""


class AxisTargetError(FeasibilityError):
    """An axis's declared target (instance+parameter vs. event_id+value_key) is invalid.

    Mirrors ``crates/av-sweep/src/grid.rs::validate_axis_target`` -- see
    :meth:`altavista.feasibility.declare.SweepAxis.validate_target` for the exact rules this
    reproduces and why. **The Rust loader is the authority**: this check exists so a typo is
    caught before ever shelling out to ``av-sweep``, not as a replacement for
    ``validate_axis_target`` -- a future change to that function's rules is not automatically
    reflected here and must be mirrored by hand.
    """


class AxisMissingTargetError(AxisTargetError):
    """Neither a complete parameter target nor a complete event target is declared."""


class AxisBothTargetsError(AxisTargetError):
    """Some field of a parameter target AND some field of an event target are both set."""


class ReservedAxisKeyPrefixError(AxisTargetError):
    """A parameter axis's own ``"{instance}.{parameter}"`` key starts with the reserved
    ``"event:"`` prefix (``crates/av-sweep/src/grid.rs::EVENT_AXIS_KEY_PREFIX``), which would
    let it collide with the event-axis key namespace in ``SweepSample.axis_values``."""


class AmbiguousAxisDeclarationError(FeasibilityError):
    """An axis declares both explicit ``values`` and a non-zero ``min``/``max``/``steps``
    range. Mirrors ``crates/av-sweep/src/grid.rs::expand_grid``'s
    ``SweepError::AmbiguousAxisDeclaration`` check -- **not** ``validate_axis_target`` -- see
    that function's own doc comment. A light, optional catch: the Rust loader remains
    authoritative for the full grid-expansion semantics (the exact interpolation formula,
    duplicate-axis detection across the whole sweep, etc.), which this package does not
    reproduce.
    """


class AxisStepsBelowMinimumError(FeasibilityError):
    """A range axis (no explicit ``values``) declares ``steps < 2``. Mirrors
    ``crates/av-sweep/src/grid.rs::expand_grid``'s ``SweepError::AxisStepsBelowMinimum``."""


class FeasibilityHashError(FeasibilityError):
    """The canonical sweep hash could not be computed or did not look like a SHA-256 hex
    digest -- see ``altavista.feasibility.hashing``."""


class FeasibilityBinaryNotFoundError(FeasibilityError):
    """No ``av-sweep`` binary could be found -- see
    ``altavista.feasibility.runner.find_av_sweep_binary``."""


class FeasibilityRunError(FeasibilityError):
    """``av-sweep`` exited non-zero, or exited zero but did not write the expected
    ``sweep_results.pb`` -- see ``altavista.feasibility.runner.run_study``."""
