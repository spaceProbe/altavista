"""Heavy cleanup round, task 2 (docs/open-questions.md questions 235 and 237): a producer for
`Trajectory.model`.

`Trajectory.model` ("the URL of a glTF the viewer draws for this spacecraft") existed on the
dataclass but `Trajectory.to_dict()` never emitted it and nothing authored it, so the viewer's
glTF entity class (`web/js/scene.js::_buildEntities`) drew wherever a trajectory declared a
model and nothing declared one. This file pins the producer:

  - `to_dict()` emits `"model"` when set and OMITS the key (not `null`) when unset;
  - the model-less JSON is byte-identical to what it was before the field had a producer
    (pinned literal, see `MODEL_LESS_JSON` below for why a literal rather than `git show HEAD`);
  - a non-`str` is a `TypeError`, an empty/whitespace-only `str` a `ValueError`, on every
    authoring surface and on emit;
  - the authoring surface that already carried appearance carries it: `Scenario.spacecraft(
    ..., model=)`, `Scenario.adopt(..., model=)`, and the `models` mapping beside `colors` on
    `Scenario.run_script` / `from_script_text`;
  - nothing here fetches the URL (a model URL that does not exist is accepted verbatim).

The browser half (a real GMAT run authored with `model=`, published, drawn by real Chrome) is
`tests/test_viewer_entities_browser.py`.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from altavista.model import Trajectory, check_model_ref  # noqa: E402
from altavista.scenario import Scenario  # noqa: E402

MODEL_URL = "/js/fixtures/entity_model_fixture.gltf"
KEPLERIAN = dict(SMA=6878.0, ECC=0.0005, INC=51.6, RAAN=45.0, AOP=0.0, TA=0.0)
EPOCH = "01 Jan 2026 00:00:00.000"

# The exact `json.dumps(Trajectory.to_dict(), sort_keys=True, separators=(",", ":"))` that the
# model.py at commit f91e156 (before this change) produced for `_fixed_trajectory()` below,
# captured by running that file's own code. A pinned literal rather than a `git show HEAD:...`
# subprocess comparison because HEAD stops being "before" the moment this change is committed:
# the comparison would then hold vacuously, while a literal keeps failing if the model-less
# shape ever drifts.
MODEL_LESS_JSON = (
    '{"attitude":[],"color":"#54a0ff","cov":[],"covDim":0,"label":"Sat","name":"Sat",'
    '"pos":[7000.0,0.0,0.0,6999.0,100.0,1.0],"stateSpaceId":"altavista.cartesian_pos_vel_6",'
    '"t":[100.0,100.5],"vel":[0.0,7.5,0.0,-0.1,7.4,0.01]}'
)


def _fixed_trajectory(**kw) -> Trajectory:
    tr = Trajectory("Sat", color="#54a0ff", **kw)
    tr.append(100.0, [7000.0, 0.0, 0.0, 0.0, 7.5, 0.0])
    tr.append(100.5, [6999.0, 100.0, 1.0, -0.1, 7.4, 0.01])
    return tr


def _dump(d: dict) -> str:
    return json.dumps(d, sort_keys=True, separators=(",", ":"))


# ------------------------------------------------------------------------- the dataclass
def test_to_dict_emits_model_when_set():
    d = _fixed_trajectory(model=MODEL_URL).to_dict()
    assert d["model"] == MODEL_URL


def test_to_dict_omits_the_key_when_unset_not_null():
    d = _fixed_trajectory().to_dict()
    assert "model" not in d


def test_model_less_json_is_byte_identical_to_before_the_producer():
    assert _dump(_fixed_trajectory().to_dict()) == MODEL_LESS_JSON


def test_model_adds_exactly_one_key_and_changes_nothing_else():
    with_model = _fixed_trajectory(model=MODEL_URL).to_dict()
    without = _fixed_trajectory().to_dict()
    assert set(with_model) - set(without) == {"model"}
    assert {k: v for k, v in with_model.items() if k != "model"} == without


def test_the_url_is_stored_verbatim_and_never_fetched():
    # A URL nothing serves, with an unreachable host: accepted unchanged, no network touched
    # (this would raise or hang if Python tried to resolve it).
    url = "http://does-not-exist.invalid/never/fetched.glb"
    assert _fixed_trajectory(model=url).to_dict()["model"] == url


@pytest.mark.parametrize("bad", [5, 1.5, b"/m.gltf", ["/m.gltf"], {"u": "/m.gltf"}, True, Path("/m.gltf")])
def test_non_string_model_is_a_type_error(bad):
    with pytest.raises(TypeError, match="non-empty str"):
        Trajectory("Sat", model=bad)
    with pytest.raises(TypeError, match="non-empty str"):
        check_model_ref(bad)


@pytest.mark.parametrize("bad", ["", " ", "\t\n"])
def test_empty_model_is_a_value_error(bad):
    with pytest.raises(ValueError, match="non-empty str"):
        Trajectory("Sat", model=bad)
    with pytest.raises(ValueError, match="non-empty str"):
        check_model_ref(bad)


def test_a_bad_model_assigned_after_construction_is_refused_on_emit():
    tr = _fixed_trajectory()
    tr.model = 7
    with pytest.raises(TypeError, match="non-empty str"):
        tr.to_dict()
    tr.model = ""
    with pytest.raises(ValueError, match="non-empty str"):
        tr.to_dict()


# ------------------------------------------------------------------- the authoring surface
def test_spacecraft_model_keyword_lands_on_the_trajectory_and_the_json():
    sc = Scenario("test_tmp_spacecraft_kw", frame="EarthMJ2000Eq")
    chaser = sc.spacecraft("gv_tmp_chaser", epoch=EPOCH, keplerian=KEPLERIAN, color="#ff6b6b", model=MODEL_URL)
    plain = sc.spacecraft("gv_tmp_plain", epoch=EPOCH, keplerian=KEPLERIAN)
    assert chaser.trajectory.model == MODEL_URL
    assert chaser.trajectory.color == "#ff6b6b"  # beside color=, not instead of it
    assert plain.trajectory.model is None
    sc.propagate([chaser, plain], seconds=120, step=60)
    by_name = {s["name"]: s for s in sc.to_dict()["spacecraft"]}
    assert by_name["gv_tmp_chaser"]["model"] == MODEL_URL
    assert "model" not in by_name["gv_tmp_plain"]


def test_adopt_model_keyword():
    sc = Scenario("test_tmp_adopt_kw", frame="EarthMJ2000Eq")
    obj = sc.gmat.Construct("Spacecraft", "gv_tmp_adopted")
    adopted = sc.adopt(obj, model=MODEL_URL)
    assert adopted.trajectory.model == MODEL_URL
    # Re-adopting returns the same Spacecraft (as it always did) and a model passed then is
    # recorded rather than silently dropped.
    again = sc.adopt(obj, model="/js/other.glb")
    assert again is adopted and adopted.trajectory.model == "/js/other.glb"
    # ...while omitting it leaves the declared one alone.
    assert sc.adopt(obj) is adopted and adopted.trajectory.model == "/js/other.glb"


def test_spacecraft_and_adopt_refuse_a_bad_model_before_creating_anything():
    sc = Scenario("test_tmp_bad_kw", frame="EarthMJ2000Eq")
    with pytest.raises(TypeError, match="non-empty str"):
        sc.spacecraft("gv_tmp_bad_type", epoch=EPOCH, keplerian=KEPLERIAN, model=3)
    with pytest.raises(ValueError, match="non-empty str"):
        sc.spacecraft("gv_tmp_bad_empty", epoch=EPOCH, keplerian=KEPLERIAN, model="")
    assert sc.spacecraft_list == []
    assert not sc.gmat.Exists("gv_tmp_bad_type") and not sc.gmat.Exists("gv_tmp_bad_empty")
    obj = sc.gmat.Construct("Spacecraft", "gv_tmp_bad_adopt")
    with pytest.raises(TypeError, match="non-empty str"):
        sc.adopt(obj, model=["/m.gltf"])
    assert sc.spacecraft_list == []


_SCRIPT = """
Create Spacecraft gv_tmp_scr_a, gv_tmp_scr_b;
gv_tmp_scr_a.DateFormat = UTCGregorian;
gv_tmp_scr_a.Epoch = '01 Jan 2026 00:00:00.000';
gv_tmp_scr_b.DateFormat = UTCGregorian;
gv_tmp_scr_b.Epoch = '01 Jan 2026 00:00:00.000';
Create ForceModel gv_tmp_fm;
gv_tmp_fm.CentralBody = Earth;
gv_tmp_fm.PointMasses = {Earth};
Create Propagator gv_tmp_prop;
gv_tmp_prop.FM = gv_tmp_fm;
gv_tmp_prop.Type = RungeKutta89;
BeginMissionSequence;
Propagate gv_tmp_prop(gv_tmp_scr_a, gv_tmp_scr_b) {gv_tmp_scr_a.ElapsedSecs = 600};
"""


def test_run_script_models_mapping_beside_colors():
    sc = Scenario("test_tmp_script_models", frame="EarthMJ2000Eq")
    sc.run_script(_SCRIPT, colors={"gv_tmp_scr_a": "#123456"}, models={"gv_tmp_scr_a": MODEL_URL})
    by_name = {s["name"]: s for s in sc.to_dict()["spacecraft"]}
    assert by_name["gv_tmp_scr_a"]["model"] == MODEL_URL
    assert by_name["gv_tmp_scr_a"]["color"] == "#123456"
    assert "model" not in by_name["gv_tmp_scr_b"]


def test_run_script_refuses_bad_or_unknown_models_before_running_gmat():
    sc = Scenario("test_tmp_script_models_bad", frame="EarthMJ2000Eq")
    with pytest.raises(TypeError, match="non-empty str"):
        sc.run_script(_SCRIPT, models={"gv_tmp_scr_a": 1})
    with pytest.raises(ValueError, match="non-empty str"):
        sc.run_script(_SCRIPT, models={"gv_tmp_scr_a": ""})
    with pytest.raises(ValueError, match="is None"):
        sc.run_script(_SCRIPT, models={"gv_tmp_scr_a": None})
    with pytest.raises(ValueError, match="does not report"):
        sc.run_script(_SCRIPT, models={"gv_tmp_scr_typo": MODEL_URL})
    assert sc.spacecraft_list == []
