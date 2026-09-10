"""Declaring a ``ParameterSweep`` from Python (F3, ``docs/feasibility-plan.md``).

These are plain dataclasses, not protobuf messages -- the same posture
``crates/av-sweep/src/schema.rs``'s ``RawParameterSweep``/``RawSweepAxis`` take relative to
``proto/altavista/v1/system.proto``'s real ``ParameterSweep``/``SweepAxis``: an honest,
field-for-field transcription of the YAML authoring shape, not a bespoke vocabulary.
:mod:`altavista.feasibility.yaml_io` turns one of these into the YAML text ``av-sweep``
itself parses (``RawParameterSweep``/``RawSweepAxis``, deny-unknown-fields, field-for-field);
this module only declares the shape and its Python-side validation.

**Seeds are deliberately not a field here.** ``proto/altavista/v1/system.proto``'s real
``ParameterSweep`` message has no ``seeds`` field (confirmed against the generated bindings:
``ParameterSweep``'s fields are exactly ``id, drm_id, axes, monte_carlo_draws, provenance,
hash, dispersed``) -- ``Scenario.seeds`` lives on the **DRM**, declared in the DRM's own YAML
(``drms/demo_two_instance_sweep.drm.yaml``'s ``scenario.seeds``), not authored through this
sweep-declaration API at all. A sweep can therefore say "this study runs every sample with
draw-to-draw dispersion" (:attr:`SweepDeclaration.monte_carlo_draws` > 1, or
:attr:`SweepDeclaration.dispersed`) but it cannot *name* which base seeds get dispersed, nor
their base values -- those are wholly the DRM's business (``crates/av-sweep``'s own seed
derivation reads ``Scenario.seeds`` off the DRM the sweep names via ``drm_id``, one derived
value per declared key, per sample). Pretending this class owned seeds by giving it a field
that silently did nothing (or worse, that this package invented an out-of-band way to smuggle
into the DRM) would misrepresent that ownership; saying so here, in the one place a Python
author would look for it, is the honest alternative.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence

from .errors import (
    AmbiguousAxisDeclarationError,
    AxisBothTargetsError,
    AxisMissingTargetError,
    AxisStepsBelowMinimumError,
    ReservedAxisKeyPrefixError,
)

# Mirrors crates/av-sweep/src/grid.rs::EVENT_AXIS_KEY_PREFIX verbatim -- see that constant's
# own doc comment (and this module's SweepAxis.validate_target) for why a parameter axis
# whose own key would start with this is refused rather than merely assumed not to happen.
EVENT_AXIS_KEY_PREFIX = "event:"


@dataclass
class Provenance:
    """``altavista.v1.Provenance``, the subset ``crates/av-kernel/src/drm/schema.py``'s
    ``RawProvenance`` actually declares (field-for-field, same names) -- see
    ``crates/av-sweep/src/schema.rs``'s own module doc comment for why ``ParameterSweep``
    reuses that exact shape rather than a second one. Every field defaults to its proto3
    zero value, matching ``RawProvenance``'s own ``#[serde(default)]``.
    """
    author_kind: str = ""
    principal: str = ""
    tool: str = ""
    config_hash: str = ""
    data_pack_hash: str = ""
    dataset_hash: str = ""
    created_tai_ns: int = 0
    run_id: str = ""
    attributes: Dict[str, str] = field(default_factory=dict)


@dataclass
class SweepAxis:
    """One axis of a ``ParameterSweep`` -- field-for-field ``RawSweepAxis``
    (``crates/av-sweep/src/schema.rs``): ``instance``, ``parameter``, ``values``, ``min``,
    ``max``, ``steps``, ``event_id``, ``value_key``. Exactly one target: an instance
    parameter (``instance`` + ``parameter``) or a scenario event value (``event_id`` +
    ``value_key``, e.g. a maneuver's ``dv_x``) -- never both, never neither
    (:meth:`validate_target`, question 192(c)).

    Construct with :meth:`parameter_axis` or :meth:`event_axis` rather than the bare
    constructor when possible -- they fill in only the fields for the target you mean and
    leave the other four at their proto3 zero value, the shape :meth:`validate_target`
    expects to see for a well-formed axis.
    """
    instance: str = ""
    parameter: str = ""
    values: List[float] = field(default_factory=list)
    min: float = 0.0
    max: float = 0.0
    steps: int = 0
    event_id: str = ""
    value_key: str = ""

    @classmethod
    def parameter_axis(cls, instance: str, parameter: str, *, values: Optional[Sequence[float]] = None,
                        min: Optional[float] = None, max: Optional[float] = None,
                        steps: Optional[int] = None) -> "SweepAxis":
        """An axis targeting ``SystemInstance`` ``instance``'s parameter ``parameter``."""
        return cls(instance=instance, parameter=parameter, values=list(values or []),
                    min=min if min is not None else 0.0, max=max if max is not None else 0.0,
                    steps=steps if steps is not None else 0)

    @classmethod
    def event_axis(cls, event_id: str, value_key: str, *, values: Optional[Sequence[float]] = None,
                    min: Optional[float] = None, max: Optional[float] = None,
                    steps: Optional[int] = None) -> "SweepAxis":
        """An axis targeting scenario event ``event_id``'s ``values[value_key]`` (question
        192(c)) -- e.g. a maneuver's ``dv_x``."""
        return cls(event_id=event_id, value_key=value_key, values=list(values or []),
                    min=min if min is not None else 0.0, max=max if max is not None else 0.0,
                    steps=steps if steps is not None else 0)

    # -- target validation (question 192(c)) -----------------------------------------------
    def validate_target(self) -> None:
        """Refuse an axis whose target is missing, ambiguous, or would collide with the
        reserved event-axis key namespace -- a line-for-line mirror of
        ``crates/av-sweep/src/grid.rs::validate_axis_target``, including its ordering (the
        "both targets" check runs first, on ANY field of each target being set, not only a
        fully-declared one -- an axis with a complete parameter target plus a half-declared
        event target, e.g. ``event_id`` set but ``value_key`` forgotten, is ambiguous, not a
        parameter axis with a silently dropped stray field).

        **The Rust loader (``crates/av-sweep/src/grid.rs::validate_axis_target``) is the
        authority.** This function exists so a typo in a Python-authored sweep is caught
        before ever shelling out to ``av-sweep``, not so this package can skip the real
        check -- ``av-sweep`` itself runs the identical rule again (``crate::schema::
        parse_sweep_yaml`` and ``crate::grid::expand_grid`` both call it) on every YAML file
        this package emits, Python-side validation or not.
        """
        param_any = bool(self.instance) or bool(self.parameter)
        event_any = bool(self.event_id) or bool(self.value_key)
        if param_any and event_any:
            raise AxisBothTargetsError(
                f"axis declares both a parameter target (instance={self.instance!r}, "
                f"parameter={self.parameter!r}) and an event target (event_id={self.event_id!r}, "
                f"value_key={self.value_key!r}) -- exactly one target per axis")

        has_param = bool(self.instance) and bool(self.parameter)
        has_event = bool(self.event_id) and bool(self.value_key)
        if has_param:
            key = f"{self.instance}.{self.parameter}"
            if key.startswith(EVENT_AXIS_KEY_PREFIX):
                raise ReservedAxisKeyPrefixError(
                    f"parameter axis key {key!r} (instance={self.instance!r}, "
                    f"parameter={self.parameter!r}) starts with the reserved "
                    f"{EVENT_AXIS_KEY_PREFIX!r} prefix, which is reserved for event-axis keys")
            return
        if has_event:
            return
        raise AxisMissingTargetError(
            f"axis declares no complete target (instance={self.instance!r}, "
            f"parameter={self.parameter!r}, event_id={self.event_id!r}, "
            f"value_key={self.value_key!r}) -- need instance+parameter or event_id+value_key")

    def validate_value_declaration(self) -> None:
        """Bonus, optional checks mirroring ``crates/av-sweep/src/grid.rs::expand_grid``'s
        own pre-expansion checks (**not** ``validate_axis_target`` -- see
        :class:`altavista.feasibility.errors.AmbiguousAxisDeclarationError`'s own docstring).
        Catches the two cheapest, most common authoring typos before a YAML file is even
        written; the Rust loader remains authoritative for the rest of ``expand_grid``'s
        semantics (the exact interpolation formula, whole-sweep duplicate-axis detection),
        which this function does not attempt to reproduce.
        """
        if self.values and (self.min != 0.0 or self.max != 0.0 or self.steps != 0):
            raise AmbiguousAxisDeclarationError(
                f"axis declares both explicit values ({self.values!r}) and a min/max/steps "
                f"range (min={self.min!r}, max={self.max!r}, steps={self.steps!r}) -- exactly "
                f"one of the two, never both")
        if not self.values and self.steps < 2 and (self.min != 0.0 or self.max != 0.0 or self.steps != 0):
            raise AxisStepsBelowMinimumError(
                f"axis declares a min/max/steps range with steps={self.steps!r} (< 2, and no "
                f"explicit values) -- a range needs at least 2 steps")

    def key(self) -> str:
        """This axis's own ``SweepSample.axis_values`` key -- delegates to
        :meth:`validate_target` first (raises if the target is invalid), then mirrors
        ``crates/av-sweep/src/grid.rs::AxisTarget::key`` exactly."""
        self.validate_target()
        if self.instance:
            return f"{self.instance}.{self.parameter}"
        return f"{EVENT_AXIS_KEY_PREFIX}{self.event_id}.{self.value_key}"


@dataclass
class SweepDeclaration:
    """A ``ParameterSweep`` authored from Python -- field-for-field ``RawParameterSweep``
    (``crates/av-sweep/src/schema.rs``): ``id``, ``drm_id``, ``axes``, ``monte_carlo_draws``,
    ``provenance``, ``hash``, ``dispersed``. ``hash`` normally starts (and stays, until
    :func:`altavista.feasibility.hashing.emit_sweep_yaml` fills it in) empty -- see that
    module for how the real canonical hash is obtained.

    See this module's own docstring for why there is deliberately no ``seeds`` field.
    """
    id: str
    drm_id: str
    axes: List[SweepAxis] = field(default_factory=list)
    monte_carlo_draws: int = 1
    provenance: Optional[Provenance] = None
    dispersed: bool = False
    hash: str = ""

    def validate(self) -> None:
        """Runs :meth:`SweepAxis.validate_target` (question 192(c)) and
        :meth:`SweepAxis.validate_value_declaration` (the bonus checks) over every declared
        axis. Raises the first violation found, axes in declaration order. Does **not**
        check for duplicate axis targets across the sweep (``crates/av-sweep/src/grid.rs``'s
        ``SweepError::DuplicateAxis``) -- that is whole-sweep grid-expansion semantics this
        package leaves to the real Rust loader, not a per-axis check.
        """
        for axis in self.axes:
            axis.validate_target()
            axis.validate_value_declaration()
