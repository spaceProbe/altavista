"""F3 (``docs/feasibility-plan.md``): Python-side axis-target validation for
``altavista.feasibility.declare`` -- a mirror of ``crates/av-sweep/src/grid.rs::
validate_axis_target``, run on a Python-authored :class:`~altavista.feasibility.declare.
SweepAxis` before it is ever written to YAML or shelled out to the real Rust loader (which
enforces the identical rule again -- see ``tests/test_feasibility_yaml.py`` for proof that a
YAML file this package emits is genuinely accepted or refused by ``av-sweep`` itself, not
merely by this Python-side mirror).

Every test here names the exact wrong implementation it would fail against, per this task's
own "every test must fail against a nameable wrong implementation" rule.
"""
from __future__ import annotations

import pytest

from altavista.feasibility import (
    AmbiguousAxisDeclarationError,
    AxisBothTargetsError,
    AxisMissingTargetError,
    AxisStepsBelowMinimumError,
    EVENT_AXIS_KEY_PREFIX,
    ReservedAxisKeyPrefixError,
    SweepAxis,
    SweepDeclaration,
)


def test_a_parameter_axis_with_explicit_values_validates_and_keys_correctly():
    """A well-formed parameter axis passes and its key is "{instance}.{parameter}" --
    fails against an implementation that swaps the two fields or uses the wrong
    separator."""
    axis = SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 25.0])
    axis.validate_target()  # must not raise
    assert axis.key() == "demo_flt.spacecraft.DragArea"


def test_an_event_axis_with_explicit_values_validates_and_keys_correctly():
    """A well-formed event axis passes and its key carries the reserved "event:" prefix --
    fails against an implementation that keys an event axis the same way as a parameter
    axis (dropping the prefix), which is exactly what would let it collide."""
    axis = SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 30.0])
    axis.validate_target()  # must not raise
    assert axis.key() == "event:burn1.dv_x"
    assert axis.key().startswith(EVENT_AXIS_KEY_PREFIX)


def test_refuses_an_axis_with_no_target_at_all():
    """Fails against an implementation that only checks "is anything at all set" instead of
    refusing the fully-empty axis -- the base case of AxisMissingTarget."""
    axis = SweepAxis()
    with pytest.raises(AxisMissingTargetError):
        axis.validate_target()


def test_refuses_a_partially_declared_parameter_target_instance_only():
    """instance set, parameter left empty: still "missing", not a usable axis. Fails against
    an implementation that treats "instance is non-empty" alone as sufficient (would let a
    typo -- a forgotten `parameter` -- silently sweep nothing, or crash later with a
    confusing KeyError instead of a clear refusal here)."""
    axis = SweepAxis(instance="demo_flt", values=[1.0])
    with pytest.raises(AxisMissingTargetError):
        axis.validate_target()


def test_refuses_a_partially_declared_event_target_value_key_only():
    """value_key set, event_id left empty: mirrors the parameter-side partial-declaration
    check on the event side. Fails against an implementation that only checks the
    parameter target's completeness and lets a half-declared event target through."""
    axis = SweepAxis(value_key="dv_x", values=[1.0])
    with pytest.raises(AxisMissingTargetError):
        axis.validate_target()


def test_refuses_an_axis_declaring_both_a_complete_parameter_and_a_complete_event_target():
    """Both targets fully declared: ambiguous, refused as AxisBothTargets, not silently
    resolved by preferring one. Fails against an implementation that just checks `if
    instance: ... elif event_id: ...` (an if/elif would silently pick the parameter branch
    and drop the event target on the floor)."""
    axis = SweepAxis(instance="demo_flt", parameter="spacecraft.DragArea",
                      event_id="burn1", value_key="dv_x", values=[1.0])
    with pytest.raises(AxisBothTargetsError):
        axis.validate_target()


def test_refuses_a_complete_parameter_target_plus_a_half_declared_event_target():
    """A complete parameter target PLUS only `event_id` (no `value_key`) set: this is the
    F2b manager-review defect crates/av-sweep/src/grid.rs's own doc comment records --
    checking "both targets COMPLETE" would let this slip through as a plain parameter axis
    with `event_id` silently dropped. Fails against an implementation using `has_param and
    has_event` (both fully declared) instead of `param_any and event_any` (any field of
    either target)."""
    axis = SweepAxis(instance="demo_flt", parameter="spacecraft.DragArea",
                      event_id="burn1", values=[1.0])
    with pytest.raises(AxisBothTargetsError):
        axis.validate_target()


def test_refuses_a_parameter_axis_whose_key_collides_with_the_reserved_event_prefix():
    """An adversarially-named instance ("event") whose "{instance}.{parameter}" key would
    start with "event:" is refused, not silently accepted to produce an axis_values key
    indistinguishable from an event axis's own key space. Fails against an implementation
    that never checks this (crates/av-sweep/src/grid.rs's own module doc comment names this
    exact adversarial case: instance literally named "event:demo_flt")."""
    axis = SweepAxis.parameter_axis("event:demo_flt", "spacecraft.DragArea", values=[1.0])
    with pytest.raises(ReservedAxisKeyPrefixError):
        axis.validate_target()
    # The key computed BY THE COLLIDING PARAMETER AXIS really would start with the prefix,
    # proving the refusal above is not a false positive on an unrelated condition.
    assert f"{axis.instance}.{axis.parameter}".startswith(EVENT_AXIS_KEY_PREFIX)


def test_a_non_colliding_parameter_axis_named_close_to_the_prefix_is_still_accepted():
    """An instance name that merely CONTAINS "event" but whose key does not START WITH
    "event:" must not be refused -- fails against an overzealous implementation that
    substring-matches the prefix anywhere in the key instead of anchoring at the start."""
    axis = SweepAxis.parameter_axis("my_event_sensor", "spacecraft.DragArea", values=[1.0])
    axis.validate_target()  # must not raise
    assert axis.key() == "my_event_sensor.spacecraft.DragArea"


def test_refuses_an_axis_declaring_both_explicit_values_and_a_range():
    """Fails against an implementation that silently prefers `values` (or `min`/`max`) when
    both are given instead of refusing the ambiguous declaration."""
    axis = SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 25.0], min=1.0, max=2.0, steps=3)
    with pytest.raises(AmbiguousAxisDeclarationError):
        axis.validate_value_declaration()


def test_refuses_a_range_axis_with_fewer_than_two_steps():
    """Fails against an implementation that accepts steps=1 or steps=0 for a declared range
    (a 1-step "range" is not a range, and 0 steps with non-zero min/max is a clear typo, not
    "zero points")."""
    axis = SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", min=1.0, max=2.0, steps=1)
    with pytest.raises(AxisStepsBelowMinimumError):
        axis.validate_value_declaration()


def test_sweep_declaration_validate_reports_the_first_bad_axis():
    """SweepDeclaration.validate() runs axis validation over every declared axis, axes in
    order -- fails against an implementation that only validates axes[0] or silently
    catches/ignores a later axis's error."""
    good = SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0])
    bad = SweepAxis()  # no target at all
    sweep = SweepDeclaration(id="s", drm_id="d", axes=[good, bad])
    with pytest.raises(AxisMissingTargetError):
        sweep.validate()


def test_sweep_declaration_has_no_seeds_field():
    """Question 191/192: Scenario.seeds lives on the DRM, not the sweep -- this dataclass
    must not silently accept (and drop) a `seeds=` keyword, which would misrepresent this
    package as owning something it does not. Fails against a future edit that adds a
    ``seeds`` field back without updating the module's own documented rationale."""
    import dataclasses
    field_names = {f.name for f in dataclasses.fields(SweepDeclaration)}
    assert "seeds" not in field_names
