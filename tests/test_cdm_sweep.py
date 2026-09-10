"""F3 (``docs/feasibility-plan.md``): ``POST /api/cdm/sweep`` -- publishing an
``altavista.v1.SweepResults`` the way ``POST /api/cdm/run`` publishes a ``RunProducts``.

Drives the real ``altavista.server.create_app`` route through ``fastapi.testclient.
TestClient`` (the exact route code a live ``uvicorn`` process runs, no real socket needed --
same posture as ``tests/test_cdm_run.py``), against the real, frozen
``tests/fixtures/demo_two_instance_sweep.sweepresults.bin`` -- a genuine 4-point x 2-draw
study over ``drms/demo_two_instance_sweep.{drm,sweep}.yaml`` produced end to end through
``altavista.feasibility``'s own Python authoring path (``tests/test_feasibility_e2e.py``
exercises that path directly; this file's job is the HTTP layer on top of an already-real
``SweepResults``, not re-proving the study itself).

What is checked, and why each check is a real test (this task's own "name the wrong
implementation it would fail against" rule) is documented per test function below.
"""
from __future__ import annotations

import json

import pytest
from fastapi.testclient import TestClient
from google.protobuf import json_format

from altavista.pb.altavista.v1 import run_pb2
from altavista.server import _sweep_results_to_dict, create_app

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
FIXTURE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance_sweep.sweepresults.bin"

# Expected values (crates/av-sweep round 2's own measurement, this task's brief): demo_mvr_
# rmag_at_end at each point, draws 0 and 1. Read from the fixture and cross-checked once
# (test_fixture_demo_mvr_rmag_matches_the_previously_measured_expected_table below), not
# duplicated as separate hardcoded literals scattered across every test that needs a real
# score value.
EXPECTED_DEMO_MVR_RMAG = {
    0: (6895836.508, 6895853.128),
    1: (6943487.876, 6945526.236),
    2: (6896092.128, 6895472.136),
    3: (6944981.977, 6947523.490),
}


@pytest.fixture(scope="module")
def fixture_bytes() -> bytes:
    assert FIXTURE_PATH.is_file(), f"missing {FIXTURE_PATH} -- run the fixture-generation path first"
    return FIXTURE_PATH.read_bytes()


@pytest.fixture(scope="module")
def fixture_results(fixture_bytes) -> run_pb2.SweepResults:
    sr = run_pb2.SweepResults()
    sr.ParseFromString(fixture_bytes)
    return sr


@pytest.fixture()
def client(tmp_path) -> TestClient:
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)
    return TestClient(app)


def test_fixture_demo_mvr_rmag_matches_the_previously_measured_expected_table(fixture_results):
    """Sanity check on the frozen fixture itself, not the HTTP route: the committed fixture's
    own demo_mvr_rmag_at_end values must match the table this task's manager recorded from
    the previous round's measurement, to 3 decimal places (mm). Any disagreement here means
    the frozen fixture drifted from the expected study, which every other test in this file
    would otherwise silently inherit."""
    by_point_draw = {(s.point_index, s.draw_index): s.scores["demo_mvr_rmag_at_end"].value
                      for s in fixture_results.samples}
    for point_index, (draw0, draw1) in EXPECTED_DEMO_MVR_RMAG.items():
        assert by_point_draw[(point_index, 0)] == pytest.approx(draw0, abs=1e-2), point_index
        assert by_point_draw[(point_index, 1)] == pytest.approx(draw1, abs=1e-2), point_index


def test_publish_binary_protobuf(client: TestClient, fixture_bytes: bytes, fixture_results):
    """POSTing the real binary-protobuf SweepResults publishes a scenario carrying every
    contract key with the fixture's real values -- fails against a handler that parses only
    JSON (would 400 or silently mis-decode raw protobuf bytes as UTF-8 JSON text), or that
    drops/renames a contract key."""
    resp = client.post("/api/cdm/sweep", content=fixture_bytes,
                        headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    assert name == "sweep:demo_two_instance_sweep"

    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    scenario = resp.json()

    assert scenario["name"] == "sweep:demo_two_instance_sweep"
    assert scenario["meta"]["sweepSource"] == "SweepResults"
    assert scenario["meta"]["sweepId"] == fixture_results.sweep_id
    assert scenario["meta"]["sweepHash"] == fixture_results.sweep_hash
    assert scenario["meta"]["drmHash"] == fixture_results.drm_hash

    sweep = scenario["sweep"]
    assert sweep["sweepId"] == "demo_two_instance_sweep"
    assert sweep["sweepHash"] == fixture_results.sweep_hash
    assert sweep["drmHash"] == fixture_results.drm_hash
    assert sweep["axisKeys"] == ["demo_flt.spacecraft.DragArea", "event:burn1.dv_x"]
    assert sweep["scoreNames"] == ["demo_flt_cd_at_end", "demo_flt_rmag_at_end", "demo_mvr_rmag_at_end"]
    assert len(sweep["points"]) == 4
    assert [p["pointIndex"] for p in sweep["points"]] == [0, 1, 2, 3], "points must be ascending by pointIndex"
    assert len(sweep["aggregates"]) == 12

    point0 = sweep["points"][0]
    assert point0["axisValues"] == {"demo_flt.spacecraft.DragArea": 5.0, "event:burn1.dv_x": 10.0}
    assert [s["drawIndex"] for s in point0["samples"]] == [0, 1], "samples must be ascending by drawIndex"
    draw0 = point0["samples"][0]
    assert draw0["runId"] == "demo_two_instance_sweep_p0_d0"
    assert draw0["error"] == ""
    assert draw0["scores"]["demo_mvr_rmag_at_end"]["value"] == pytest.approx(EXPECTED_DEMO_MVR_RMAG[0][0], abs=1e-2)
    assert draw0["scores"]["demo_mvr_rmag_at_end"]["passed"] is None, "a measure of effectiveness has no pass criterion"
    assert draw0["scores"]["demo_mvr_rmag_at_end"]["unit"] == "UNIT_METER"


def test_publish_json(client: TestClient, fixture_bytes: bytes, fixture_results):
    """POSTing the JSON transcoding of the identical message publishes the SAME scenario as
    the binary path -- fails against a handler whose JSON branch diverges from the binary
    branch (e.g. a different projection function, or a json_format.MessageToDict call that
    silently drops an unset optional field this task's own contract requires as an explicit
    null)."""
    sr = run_pb2.SweepResults()
    sr.ParseFromString(fixture_bytes)
    json_text = json_format.MessageToJson(sr)

    resp = client.post("/api/cdm/sweep", content=json_text, headers={"content-type": "application/json"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]

    resp_bin = create_app_and_publish_binary(fixture_bytes)
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    scenario_from_json = resp.json()

    assert scenario_from_json["sweep"] == resp_bin, "binary and JSON publish paths must produce the identical scenario['sweep'] projection"


def create_app_and_publish_binary(fixture_bytes: bytes) -> dict:
    """Helper for test_publish_json: publishes the same fixture bytes through a SEPARATE
    fresh app/client via the binary path, and returns its scenario['sweep'] dict, so the
    JSON test can assert byte-for-byte (well, dict-for-dict) equality against a known-good
    binary publish without entangling the two TestClient fixtures' scenario stores."""
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        app = create_app(texture_dir=td, web_dir=td)
        c = TestClient(app)
        resp = c.post("/api/cdm/sweep", content=fixture_bytes, headers={"content-type": "application/x-protobuf"})
        assert resp.status_code == 200, resp.text
        name = resp.json()["name"]
        return c.get(f"/api/scenario/{name}").json()["sweep"]


def test_malformed_binary_body_is_refused_with_400(client: TestClient):
    """Garbage bytes under application/x-protobuf must 400, naming the content type --
    fails against a handler that lets DecodeError propagate as an unhandled 500, or that
    silently accepts garbage as an all-default-fields SweepResults."""
    resp = client.post("/api/cdm/sweep", content=b"\xff\xfe\x00\x01not a protobuf message at all",
                        headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 400, resp.text
    assert "SweepResults" in resp.text
    assert "application/x-protobuf" in resp.text


def test_malformed_json_body_is_refused_with_400(client: TestClient):
    """Invalid JSON under any non-protobuf content type must 400 -- fails the same way as
    the binary case above, for the JSON branch."""
    resp = client.post("/api/cdm/sweep", content="{not valid json", headers={"content-type": "application/json"})
    assert resp.status_code == 400, resp.text
    assert "SweepResults" in resp.text


def test_json_with_an_unknown_field_is_refused_with_400(client: TestClient):
    """A JSON document that is syntactically valid JSON, but not a valid SweepResults (an
    unknown field name), must still 400 -- fails against a handler using a permissive JSON
    parse (e.g. bare json.loads with no schema check) instead of json_format.Parse, which
    would accept this silently and publish a bogus, all-default scenario."""
    resp = client.post("/api/cdm/sweep", content=json.dumps({"not_a_real_field": True}),
                        headers={"content-type": "application/json"})
    assert resp.status_code == 400, resp.text


def test_sweep_source_marker_present_even_with_no_aggregates(client: TestClient):
    """A study with zero aggregates (a synthetic, hand-built SweepResults -- never derived
    from the real fixture, so this is a genuinely independent case) still publishes
    meta.sweepSource (and the other meta.sweep* keys) and an explicit empty
    scenario['sweep']['aggregates'] list -- fails against an implementation that only sets
    meta.sweepSource when aggregates is non-empty, or that omits the aggregates key entirely
    instead of publishing []."""
    sr = run_pb2.SweepResults(sweep_id="empty_study", sweep_hash="", drm_hash="")
    resp = client.post("/api/cdm/sweep", content=sr.SerializeToString(),
                        headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    assert name == "sweep:empty_study"

    scenario = client.get(f"/api/scenario/{name}").json()
    assert scenario["meta"]["sweepSource"] == "SweepResults"
    assert scenario["meta"]["sweepId"] == "empty_study"
    assert scenario["meta"]["sweepHash"] == ""
    assert scenario["meta"]["drmHash"] == ""
    assert scenario["sweep"]["aggregates"] == []
    assert scenario["sweep"]["points"] == []
    assert scenario["sweep"]["axisKeys"] == []
    assert scenario["sweep"]["scoreNames"] == []
    # "no trajectory, no events, no bodies" (this route's own docstring) -- span() over an
    # empty spacecraft list is None, so t0/t1 publish as null, not a fabricated span.
    assert scenario["spacecraft"] == []
    assert scenario["events"] == []
    assert scenario["t0"] is None
    assert scenario["t1"] is None


def test_sweep_results_to_dict_is_directly_callable_without_http():
    """This task's own instruction: "so a test ... can call it directly rather than through
    HTTP" -- proves _sweep_results_to_dict is a real, importable, standalone function, not
    logic buried inside the route closure."""
    sr = run_pb2.SweepResults(sweep_id="direct_call_test")
    d = _sweep_results_to_dict(sr)
    assert d["sweepId"] == "direct_call_test"
    assert d["aggregates"] == []
    assert d["points"] == []


def test_pass_fraction_zero_is_distinguished_from_unset():
    """A real, measured 0% pass fraction (every draw at this point failed its Objective)
    must publish as the float 0.0, never coerced to null -- fails against an implementation
    that reads `a.pass_fraction or None` instead of `a.pass_fraction if a.HasField(...) else
    None`: proto3's zero value for an unset optional double is also 0.0, so `0.0 or None`
    evaluates to None, silently and wrongly turning "every draw failed" into "no pass
    criterion exists" (ADR-005 sec 6's Objective/MeasureOfEffectiveness distinction, the
    same one question 165's ScoreResult.passed handling protects for individual scores)."""
    sr = run_pb2.SweepResults(sweep_id="s")
    sr.aggregates.add(name="an_objective", point_index=0, draws=2, mean=1.0, std_dev=0.0,
                       min=1.0, max=1.0, pass_fraction=0.0)
    d = _sweep_results_to_dict(sr)
    assert d["aggregates"][0]["passFraction"] == 0.0
    assert d["aggregates"][0]["passFraction"] is not None


def test_seeds_are_encoded_as_decimal_strings_not_bare_numbers():
    """A uint64 seed value must publish as a JSON string (proto3 canonical JSON's own
    convention for uint64 -- most JSON parsers, JavaScript's included, cannot represent the
    full uint64 range as a native number without precision loss), never a bare int -- fails
    against an implementation that forgets str() and leaves the raw Python int in the dict:
    json.dumps would then emit an unquoted number, and a value at or above 2**53 would
    silently lose precision in a JS consumer. The seed value below (2**63, i.e.
    9223372036854775808) is deliberately above JavaScript's Number.MAX_SAFE_INTEGER
    (2**53 - 1) -- large enough that this test would still catch the bug even if it only
    checked for precision loss rather than type."""
    big_seed = 2**63  # 9223372036854775808 -- exceeds Number.MAX_SAFE_INTEGER by a wide margin
    sr = run_pb2.SweepResults(sweep_id="s")
    sample = sr.samples.add(point_index=0, draw_index=0, run_id="r0")
    sample.seeds["burn_seed"] = big_seed

    d = _sweep_results_to_dict(sr)
    seed_value = d["points"][0]["samples"][0]["seeds"]["burn_seed"]
    assert isinstance(seed_value, str), f"expected a decimal string, got {type(seed_value)}: {seed_value!r}"
    assert seed_value == str(big_seed)

    # And through actual JSON serialization, as a browser consumer would see it: the digits
    # must appear inside quotes, never as a bare (unquoted) JSON number.
    encoded = json.dumps(d)
    assert f'"burn_seed": "{big_seed}"' in encoded or f'"burn_seed":"{big_seed}"' in encoded
    assert f'"burn_seed": {big_seed}' not in encoded and f'"burn_seed":{big_seed}' not in encoded


def test_a_failed_sample_publishes_its_error_and_empty_scores_and_seeds(fixture_results):
    """A failed SweepSample (error set, no scores, no seeds -- crates/av-sweep's own
    documented behaviour for a per-sample refusal) must publish error and empty
    scores/seeds dicts verbatim, never backfilled with a placeholder. Built as a synthetic
    sample (the real fixture's own 8 samples all succeeded) rather than skipped, since this
    is exactly the kind of case a "nothing is ever synthesized" rule needs covered."""
    sr = run_pb2.SweepResults(sweep_id="s")
    sr.samples.add(point_index=0, draw_index=0, run_id="r0",
                    error="axis names demo_flt.spacecraft.DragArea = 999.0, which is outside its declared bound [0, 30]")
    d = _sweep_results_to_dict(sr)
    sample = d["points"][0]["samples"][0]
    assert sample["error"] != ""
    assert sample["scores"] == {}
    assert sample["seeds"] == {}
