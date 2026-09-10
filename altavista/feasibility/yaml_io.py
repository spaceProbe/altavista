"""Emitting the YAML ``av-sweep`` consumes (F3, ``docs/feasibility-plan.md``).

Field-for-field ``RawParameterSweep``/``RawSweepAxis``/``RawProvenance``
(``crates/av-sweep/src/schema.rs``, reusing ``crates/av-kernel/src/drm/schema.rs``'s
``RawProvenance``) -- every one of those three structs is
``#[serde(deny_unknown_fields, default)]``, so this module emits *only* their declared field
names (an unknown key is a hard parse failure on the Rust side) and may omit a field whose
value is its Rust zero-default (``""``, ``0``, ``0.0``, ``[]``, ``{}``, ``false``), since
``#[serde(default)]`` fills it back in identically. This module always emits every field
explicitly instead (including zero defaults) -- simpler to audit against the three Raw*
structs field list than a "which fields are worth omitting" judgment call, and no less
correct: an explicit zero and an omitted field parse to the exact same Rust value either way.
"""
from __future__ import annotations

from typing import Any, Dict, Optional

import yaml

from .declare import SweepAxis, SweepDeclaration


def _axis_to_dict(axis: SweepAxis) -> Dict[str, Any]:
    """One ``RawSweepAxis``, field-for-field: ``instance``, ``parameter``, ``values``,
    ``min``, ``max``, ``steps``, ``event_id``, ``value_key`` -- exactly ``crates/av-sweep/src/
    schema.rs``'s ``RawSweepAxis`` field list and order, no more, no fewer (``#[serde(deny_
    unknown_fields)]`` refuses an extra key; this dict never has one)."""
    return {
        "instance": axis.instance,
        "parameter": axis.parameter,
        "values": [float(v) for v in axis.values],
        "min": float(axis.min),
        "max": float(axis.max),
        "steps": int(axis.steps),
        "event_id": axis.event_id,
        "value_key": axis.value_key,
    }


def _provenance_to_dict(p) -> Dict[str, Any]:
    """One ``RawProvenance``, field-for-field -- see this module's own docstring for why
    ``crates/av-sweep/src/schema.rs`` reuses this exact shape from ``crates/av-kernel/src/
    drm/schema.rs`` rather than declaring a second one."""
    return {
        "author_kind": p.author_kind,
        "principal": p.principal,
        "tool": p.tool,
        "config_hash": p.config_hash,
        "data_pack_hash": p.data_pack_hash,
        "dataset_hash": p.dataset_hash,
        "created_tai_ns": int(p.created_tai_ns),
        "run_id": p.run_id,
        "attributes": dict(p.attributes),
    }


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
    for "no provenance", so omitting it is the honest, unambiguous choice, not a shortcut.
    """
    sweep.validate()
    doc: Dict[str, Any] = {
        "id": sweep.id,
        "drm_id": sweep.drm_id,
        "axes": [_axis_to_dict(a) for a in sweep.axes],
        "monte_carlo_draws": int(sweep.monte_carlo_draws),
    }
    if sweep.provenance is not None:
        doc["provenance"] = _provenance_to_dict(sweep.provenance)
    doc["hash"] = sweep.hash if hash is None else hash
    doc["dispersed"] = bool(sweep.dispersed)
    return doc


def to_yaml(sweep: SweepDeclaration, *, hash: Optional[str] = None) -> str:
    """``sweep`` as YAML text ``av-sweep``'s ``parse_sweep_yaml`` (``crates/av-sweep/src/
    schema.rs``) loads. ``default_flow_style=False`` for the same one-key-per-line style
    every ``drms/*.yaml`` fixture in this repository already uses (e.g.
    ``drms/demo_two_instance_sweep.sweep.yaml``) -- readable in a diff, not a functional
    requirement (``serde_yaml`` parses flow style identically)."""
    return yaml.safe_dump(to_yaml_dict(sweep, hash=hash), default_flow_style=False, sort_keys=False)
