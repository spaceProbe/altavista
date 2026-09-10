"""Emitting the YAML ``av-sweep`` consumes (F3, ``docs/feasibility-plan.md``).

Field-for-field ``RawParameterSweep``/``RawSweepAxis``/``RawProvenance``
(``crates/av-sweep/src/schema.rs``, reusing ``crates/av-kernel/src/drm/schema.rs``'s
``RawProvenance``) -- every one of those three structs is
``#[serde(deny_unknown_fields, default)]``, so this module emits *only* their declared field
names (an unknown key is a hard parse failure on the Rust side) and may omit a field whose
value is its Rust zero-default (``""``, ``0``, ``0.0``, ``[]``, ``{}``, ``false``), since
``#[serde(default)]`` fills it back in identically.

**F4b change (previously: always emitted every field explicitly, including zero defaults).**
This module now OMITS a field whose value is its declared proto3/Rust zero-default, matching
the style every hand-written ``drms/*.yaml`` fixture in this repository already uses (e.g.
``drms/demo_two_instance_sweep.sweep.yaml``, whose own event axis has no ``instance:``/
``parameter:``/``min:``/``max:``/``steps:`` keys at all, and whose own top level has no
``dispersed:`` key at all). This is safe *only* because ``#[serde(default)]`` on every one of
the three Raw* structs guarantees an omitted field parses back to the exact same Rust zero
value an explicit zero would -- YAML mapping key ORDER is also irrelevant to the parsed struct,
so neither this omission nor this module's own fixed field order can move
``canonical_sweep_hash`` (a hash over the *parsed message*, not the YAML text) at all. Proved,
not just argued: ``compute_sweep_hash`` was run against this module's own new (field-omitting)
emission of ``drms/drag_sail_vs_burn.sweep.yaml`` (F4's own committed, hash-pinned sweep) and
compared byte-for-byte against the hash already committed in that file --
see ``docs/studies/drag-sail-vs-burn.md``'s emitter-change section for the exact commands and
digests.
"""
from __future__ import annotations

from typing import Any, Dict, Optional

import yaml

from .declare import SweepAxis, SweepDeclaration


def _axis_to_dict(axis: SweepAxis) -> Dict[str, Any]:
    """One ``RawSweepAxis``, in ``crates/av-sweep/src/schema.rs``'s own ``RawSweepAxis`` field
    order (``instance``, ``parameter``, ``values``, ``min``, ``max``, ``steps``, ``event_id``,
    ``value_key``) -- but a field is OMITTED when it equals that field's own proto3/Rust zero
    default (``""`` for the four string fields, ``[]`` for ``values``, ``0.0`` for ``min``/
    ``max``, ``0`` for ``steps``), since ``RawSweepAxis``'s own ``#[serde(default)]`` fills an
    omitted key back in identically -- see this module's own docstring for the hash-parity
    proof this omission rests on. A well-formed axis (exactly one target, per
    ``SweepAxis.validate_target``) therefore emits only the four keys its own target actually
    uses (e.g. ``instance``/``parameter``/``values`` for a parameter axis, ``event_id``/
    ``value_key``/``values`` for an event axis), never the other struct's zero-valued fields.
    """
    d: Dict[str, Any] = {}
    if axis.instance:
        d["instance"] = axis.instance
    if axis.parameter:
        d["parameter"] = axis.parameter
    if axis.values:
        d["values"] = [float(v) for v in axis.values]
    if axis.min != 0.0:
        d["min"] = float(axis.min)
    if axis.max != 0.0:
        d["max"] = float(axis.max)
    if axis.steps != 0:
        d["steps"] = int(axis.steps)
    if axis.event_id:
        d["event_id"] = axis.event_id
    if axis.value_key:
        d["value_key"] = axis.value_key
    return d


def _provenance_to_dict(p) -> Dict[str, Any]:
    """One ``RawProvenance``, field-for-field -- see this module's own docstring for why
    ``crates/av-sweep/src/schema.rs`` reuses this exact shape from ``crates/av-kernel/src/
    drm/schema.rs`` rather than declaring a second one. Each field is OMITTED when it equals
    its own proto3/Rust zero default, for the identical reason :func:`_axis_to_dict` omits
    one -- ``RawProvenance`` is itself ``#[serde(default)]``, field-by-field, not only as a
    whole struct, so this is a per-field elision, not an all-or-nothing one."""
    d: Dict[str, Any] = {}
    if p.author_kind:
        d["author_kind"] = p.author_kind
    if p.principal:
        d["principal"] = p.principal
    if p.tool:
        d["tool"] = p.tool
    if p.config_hash:
        d["config_hash"] = p.config_hash
    if p.data_pack_hash:
        d["data_pack_hash"] = p.data_pack_hash
    if p.dataset_hash:
        d["dataset_hash"] = p.dataset_hash
    if p.created_tai_ns:
        d["created_tai_ns"] = int(p.created_tai_ns)
    if p.run_id:
        d["run_id"] = p.run_id
    if p.attributes:
        d["attributes"] = dict(p.attributes)
    return d


def to_yaml_dict(sweep: SweepDeclaration, *, hash: Optional[str] = None) -> Dict[str, Any]:
    """``sweep`` as the plain dict :func:`to_yaml` dumps -- one ``RawParameterSweep``,
    field-for-field: ``id``, ``drm_id``, ``axes``, ``monte_carlo_draws``, ``provenance``,
    ``hash``, ``dispersed``.

    ``hash`` overrides ``sweep.hash`` when given (not ``None``) -- the two-pass hashing dance
    in :func:`altavista.feasibility.hashing.emit_sweep_yaml` needs to emit the identical
    document with only the ``hash`` field differing (once empty, to compute the canonical
    hash over; once with that computed digest), without mutating ``sweep`` between the two
    calls. ``provenance:`` is present only when ``sweep.provenance`` is not ``None`` --
    ``RawParameterSweep.provenance`` is ``Option<RawProvenance>``, and an absent YAML key
    parses to ``None`` there exactly like an explicit key would if this module invented one
    for "no provenance", so omitting it is the honest, unambiguous choice, not a shortcut. Note
    this is a DIFFERENT omission than the zero-default elision below: ``provenance:`` is kept
    (even as ``provenance: {}``) whenever ``sweep.provenance`` is not ``None``, precisely
    because ``Some(RawProvenance::default())`` and ``None`` are different parsed values, unlike
    every other field here (where an omitted key and an explicit zero-default value parse
    identically).

    ``id``/``drm_id``/``axes`` are always emitted (never legitimately absent from a real
    sweep); ``monte_carlo_draws``/``hash``/``dispersed`` are omitted when they equal their own
    proto3/Rust zero default (``0``, ``""``, ``false`` respectively) for the same reason
    :func:`_axis_to_dict`/:func:`_provenance_to_dict` omit theirs.
    """
    sweep.validate()
    doc: Dict[str, Any] = {
        "id": sweep.id,
        "drm_id": sweep.drm_id,
        "axes": [_axis_to_dict(a) for a in sweep.axes],
    }
    if sweep.monte_carlo_draws:
        doc["monte_carlo_draws"] = int(sweep.monte_carlo_draws)
    if sweep.provenance is not None:
        doc["provenance"] = _provenance_to_dict(sweep.provenance)
    computed_hash = sweep.hash if hash is None else hash
    if computed_hash:
        doc["hash"] = computed_hash
    if sweep.dispersed:
        doc["dispersed"] = bool(sweep.dispersed)
    return doc


def to_yaml(sweep: SweepDeclaration, *, hash: Optional[str] = None) -> str:
    """``sweep`` as YAML text ``av-sweep``'s ``parse_sweep_yaml`` (``crates/av-sweep/src/
    schema.rs``) loads. ``default_flow_style=False`` for the same one-key-per-line style
    every ``drms/*.yaml`` fixture in this repository already uses (e.g.
    ``drms/demo_two_instance_sweep.sweep.yaml``) -- readable in a diff, not a functional
    requirement (``serde_yaml`` parses flow style identically)."""
    return yaml.safe_dump(to_yaml_dict(sweep, hash=hash), default_flow_style=False, sort_keys=False)
