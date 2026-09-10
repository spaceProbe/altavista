"""F1b (crates/av-sweep's own task brief, §4): an independent oracle for
``crates/av-sweep/src/bin/av-sweep/json.rs``'s hand-written proto3 canonical JSON encoder.

The encoder is Rust code this repository writes by hand (no protobuf-json-mapping crate, per
this task's "no new dependency" rule). Rust-side tests can only prove the encoder agrees with
itself; they cannot prove it agrees with the *real* proto3 JSON mapping. This test is that
external check: it parses ``goldens/sweep_results_json/sweep_results.json`` with Python's real
``google.protobuf.json_format.Parse`` (a completely independent implementation of the same
mapping, not this crate's own code in any form) and separately parses the companion
``sweep_results.pb`` with plain ``ParseFromString``, then asserts the two decode to the
byte-identical message. If the JSON encoder ever mis-renames a field, forgets to quote a
``uint64``, or drops something a genuine proto3 reader still expects, this test -- not a Rust
unit test pinned against the same code under test -- is what catches it.

Both golden files are produced by ``crates/av-sweep/examples/gen_sweep_results_json_golden.rs``,
a SYNTHETIC (no GMAT, no real study run) fixture, from a generator that refuses to run without an
explicit ``--reason`` (standing rule: "Goldens are regenerated only through a generator that
takes an explicit --reason") -- see ``goldens/sweep_results_json/REGENERATED.txt`` for the reason
and command the currently-committed pair was produced with. This file does NOT regenerate them;
it only reads what is already committed.
"""

from __future__ import annotations

from pathlib import Path

from google.protobuf import json_format

from altavista.pb import core_pb2
from altavista.pb.altavista.v1 import run_pb2

GOLDEN_DIR = Path(__file__).resolve().parent.parent / "goldens" / "sweep_results_json"


def test_sweep_results_json_round_trips_through_the_real_protobuf_library_to_the_same_message():
    json_text = (GOLDEN_DIR / "sweep_results.json").read_text()
    pb_bytes = (GOLDEN_DIR / "sweep_results.pb").read_bytes()

    from_json = run_pb2.SweepResults()
    json_format.Parse(json_text, from_json)

    from_binary = run_pb2.SweepResults()
    from_binary.ParseFromString(pb_bytes)

    assert from_json == from_binary, "the hand-written JSON encoder's output must decode (via the real protobuf library) to the identical message the .pb file itself carries"

    # A few field-level sanity checks, so a bug that happened to make BOTH messages equally
    # wrong (e.g. an empty golden on both sides) cannot slip through unnoticed.
    assert from_binary.sweep_id == "demo_two_instance_sweep"
    assert len(from_binary.samples) == 2
    ok_sample = next(s for s in from_binary.samples if s.point_index == 0)
    # Question 192(b): SweepSample.seed (one projected value) -> seeds (a map), field 10 --
    # the golden's own generator now writes TWO keys, so map ordering is actually exercised.
    assert dict(ok_sample.seeds) == {"burn_seed": 15505883566766354181, "fault_seed": 42}
    assert ok_sample.error == ""
    assert ok_sample.scores["demo_flt_cd_at_end"].HasField("passed")
    assert ok_sample.scores["demo_flt_cd_at_end"].passed is True
    assert not ok_sample.scores["demo_flt_rmag_at_end"].HasField("passed")

    failed_sample = next(s for s in from_binary.samples if s.point_index == 1)
    assert failed_sample.error != ""
    assert len(failed_sample.seeds) == 0, "a failed sample never got far enough to derive any seeds"
    assert len(failed_sample.scores) == 0

    assert len(from_binary.aggregates) == 1
    assert from_binary.aggregates[0].HasField("pass_fraction")
    assert from_binary.aggregates[0].pass_fraction == 1.0

    assert from_binary.provenance.created_tai_ns == 0
    assert from_binary.provenance.author_kind == core_pb2.AUTHOR_KIND_AGENT


def test_sweep_results_json_encodes_the_uint64_seed_as_a_json_string():
    json_text = (GOLDEN_DIR / "sweep_results.json").read_text()
    # A raw textual check, independent of any parser: proto3 canonical JSON must never emit a
    # bare (unquoted) 64-bit integer -- most JSON parsers (including JavaScript's) cannot
    # represent the full uint64 range as a native number without precision loss. Question
    # 192(b): the seeds MAP's own values get this treatment now, not a single top-level field.
    assert '"seeds":{"burn_seed":"15505883566766354181","fault_seed":"42"}' in json_text, json_text
    assert '"burn_seed":15505883566766354181' not in json_text, "a uint64 map value must never appear as a bare JSON number"
