"""F3 (``docs/feasibility-plan.md``): the hash-agreement test named in the package's own
task brief section 1 -- proof that :func:`altavista.feasibility.hashing.compute_sweep_hash`
agrees with ``crates/av-sweep/src/hash.rs::canonical_sweep_hash`` on
``drms/demo_two_instance_sweep.sweep.yaml``'s own committed hash, and that a Python-authored
:class:`~altavista.feasibility.declare.SweepDeclaration` reproducing that same fixture's
content hashes to the exact same value even though the emitted YAML *text* differs
completely in formatting from the hand-authored file.

See ``altavista/feasibility/hashing.py``'s own module docstring for the honest disclosure of
what "shelling out to the real Rust tool" does and does not prove.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from altavista.feasibility import (
    FeasibilityHashError,
    Provenance,
    SweepAxis,
    SweepDeclaration,
    compute_sweep_hash,
    emit_sweep_yaml,
)

REPO_ROOT = Path(__file__).resolve().parents[1]
FIXTURE_SWEEP_YAML = REPO_ROOT / "drms" / "demo_two_instance_sweep.sweep.yaml"
# Read straight from the committed fixture's own text (not hardcoded twice) so this test
# tracks the fixture rather than silently desyncing from it if it is ever regenerated --
# same posture as tests/test_cdm_run.py's own _declared_drm_hash() helper.
_FIXTURE_HASH_LINE = next(l for l in FIXTURE_SWEEP_YAML.read_text().splitlines() if l.startswith("hash:"))
FIXTURE_HASH = _FIXTURE_HASH_LINE.split('"')[1]


def test_fixture_hash_line_itself_looks_like_a_sha256_hex_digest():
    """Sanity check on the parsing above, not the code under test -- if this fails, every
    other test in this file is comparing against garbage."""
    assert len(FIXTURE_HASH) == 64
    assert all(c in "0123456789abcdef" for c in FIXTURE_HASH)


def test_compute_sweep_hash_agrees_with_the_committed_fixture_hash():
    """The hash-agreement test: computing the hash of the already-committed, already
    hash-verified fixture file must reproduce its own declared ``hash:`` field exactly.
    Fails against a compute_sweep_hash that calls the wrong cargo target, mis-parses stdout
    (e.g. keeps a trailing newline or stray log line), or silently returns a placeholder on
    subprocess failure instead of raising."""
    computed = compute_sweep_hash(FIXTURE_SWEEP_YAML)
    assert computed == FIXTURE_HASH


def test_a_python_authored_sweep_reproducing_the_fixture_hashes_identically():
    """The stronger form: a SweepDeclaration built from scratch in Python, describing
    exactly the same ParameterSweep the fixture's own YAML hand-authors (same id, drm_id,
    two axes, draws, provenance), emitted through altavista.feasibility.yaml_io and hashed
    through emit_sweep_yaml, must hash to the IDENTICAL value -- even though the emitted
    YAML's text differs completely from the hand-authored file (field order, provenance
    field ordering, quoting style). This is only possible if canonical_sweep_hash truly
    hashes the parsed protobuf message, not the YAML text, and if this package's emission
    populates every field prost's canonical encoding covers with the identical values.
    Fails against a yaml_io.to_yaml that drops a field (the hash would then differ, or
    parse_sweep_yaml would reject an unknown/missing field), or against a compute_sweep_hash
    that returns a cached/stale value rather than genuinely re-invoking the Rust tool.
    """
    sweep = SweepDeclaration(
        id="demo_two_instance_sweep",
        drm_id="demo_two_instance_sweep_drm",
        axes=[
            SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 25.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 30.0]),
        ],
        monte_carlo_draws=2,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT", tool="av-sweep F1b fixture authoring"),
    )

    import tempfile
    with tempfile.TemporaryDirectory() as td:
        out = Path(td) / "reproduced.sweep.yaml"
        digest = emit_sweep_yaml(sweep, out)

    assert digest == FIXTURE_HASH
    assert sweep.hash == FIXTURE_HASH, "emit_sweep_yaml must stamp the computed hash back onto sweep.hash"


def test_a_different_sweep_hashes_differently():
    """Negative control: a SweepDeclaration that genuinely differs from the fixture (one
    axis value changed) must NOT hash to the fixture's value -- fails against a
    compute_sweep_hash/emit_sweep_yaml that always returns the same digest regardless of
    content (e.g. a hardcoded return, or a hash computed over the wrong/empty message)."""
    sweep = SweepDeclaration(
        id="demo_two_instance_sweep",
        drm_id="demo_two_instance_sweep_drm",
        axes=[
            # 26.0, not 25.0 -- the one deliberate difference from the fixture.
            SweepAxis.parameter_axis("demo_flt", "spacecraft.DragArea", values=[5.0, 26.0]),
            SweepAxis.event_axis("burn1", "dv_x", values=[10.0, 30.0]),
        ],
        monte_carlo_draws=2,
        provenance=Provenance(author_kind="AUTHOR_KIND_AGENT", tool="av-sweep F1b fixture authoring"),
    )
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        out = Path(td) / "different.sweep.yaml"
        digest = emit_sweep_yaml(sweep, out)
    assert digest != FIXTURE_HASH


def test_compute_sweep_hash_raises_on_a_nonexistent_file():
    """Fails against an implementation that lets the underlying cargo/file-not-found error
    escape as some other exception type, or worse, silently returns a bogus digest."""
    with pytest.raises(FeasibilityHashError):
        compute_sweep_hash(Path("/nonexistent/path/does_not_exist.sweep.yaml"))


def test_compute_sweep_hash_raises_on_malformed_yaml():
    """A YAML file with an unknown field (deny_unknown_fields) must be refused by the real
    Rust loader, surfaced as FeasibilityHashError -- fails against an implementation that
    swallows the parse error and returns some default/placeholder digest instead."""
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        bad = Path(td) / "bad.sweep.yaml"
        bad.write_text("id: x\nnot_a_real_field: true\nhash: \"\"\n")
        with pytest.raises(FeasibilityHashError):
            compute_sweep_hash(bad)
