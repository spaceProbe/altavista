"""Pure-Python tests (no GMAT needed) for script preparation and frame parsing."""
import re
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from altavista.model import Footprint, Frame, ScenarioData, Trajectory  # noqa: E402
from altavista.scenario import (  # noqa: E402
    decimate, frame_from_script, parse_frame, prepare_script, script_spacecraft, strip_gui_subscribers,
)

SAMPLE = """
Create Spacecraft SatA, SatB;
Create Spacecraft SatC;
Create OrbitView View1;
View1.Add = {SatA, Earth};
View1.ShowPlot = true;
Create OpenFramesInterface OFI;
OFI.Add = {SatA};
Create ReportFile UserReport;
UserReport.Add = {SatA.X};
Create CoordinateSystem MarsInertial;
MarsInertial.Origin = Mars;
MarsInertial.Axes = BodyInertial;
BeginMissionSequence;
Toggle View1 Off;
Propagate Prop(SatA, SatB, SatC) {SatA.ElapsedDays = 1};
"""


def test_strip_gui_subscribers_comments_out_views_and_their_settings():
    out = strip_gui_subscribers(SAMPLE)
    assert "% [altavista stripped] Create OrbitView View1;" in out
    assert "% [altavista stripped] View1.Add" in out
    assert "% [altavista stripped] OFI.Add" in out
    assert "% [altavista stripped] Toggle View1 Off;" in out
    # user report files are kept
    assert "\nCreate ReportFile UserReport;" in out
    assert "\nUserReport.Add" in out


def test_script_spacecraft_handles_comma_lists():
    assert script_spacecraft(SAMPLE) == ["SatA", "SatB", "SatC"]


def test_prepare_script_injects_reports_before_mission_sequence(tmp_path):
    text, files = prepare_script(SAMPLE, Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"), str(tmp_path))
    assert set(files) == {"SatA", "SatB", "SatC"}
    for sat in files:
        assert f"Create ReportFile altavista_{sat};" in text
        assert f"{sat}.EarthMJ2000Eq.VZ" in text
        assert f"altavista_{sat}.SolverIterations = Current;" in text
        assert files[sat].endswith(f"{sat}.rpt")
    # every injected line precedes BeginMissionSequence
    assert text.index("altavista_SatC.Add") < text.index("BeginMissionSequence")
    # only one BeginMissionSequence remains
    assert len(re.findall(r"^\s*BeginMissionSequence", text, re.M)) == 1


def test_prepare_script_adds_frame_cs_when_missing(tmp_path):
    text, _ = prepare_script(SAMPLE, Frame("LunaMJ2000Eq", "Luna", "MJ2000Eq"), str(tmp_path), spacecraft=["SatA"])
    assert "Create CoordinateSystem LunaMJ2000Eq;" in text
    assert "LunaMJ2000Eq.Origin = Luna;" in text
    # a frame the script already defines is not redefined
    text2, _ = prepare_script(SAMPLE, Frame("MarsInertial", "Mars", "BodyInertial"), str(tmp_path), spacecraft=["SatA"])
    assert text2.count("Create CoordinateSystem MarsInertial") == 1


def test_prepare_script_requires_spacecraft(tmp_path):
    with pytest.raises(ValueError):
        prepare_script("Create ForceModel FM;\nBeginMissionSequence;\n", Frame(), str(tmp_path))


@pytest.mark.parametrize("spec, origin, axes", [
    ("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
    ("EarthFixed", "Earth", "BodyFixed"),
    ("SunMJ2000Ec", "Sun", "MJ2000Ec"),
    ("MarsInertial", "Mars", "BodyInertial"),
    ("LunaFixed", "Luna", "BodyFixed"),
    (("Jupiter", "MJ2000Eq"), "Jupiter", "MJ2000Eq"),
])
def test_parse_frame(spec, origin, axes):
    fr = parse_frame(spec)
    assert (fr.origin, fr.axes) == (origin, axes)


def test_parse_frame_rejects_unknown():
    with pytest.raises(ValueError):
        parse_frame("Nonsense")


def test_frame_from_script():
    fr = frame_from_script(SAMPLE, "MarsInertial")
    assert fr == Frame("MarsInertial", "Mars", "BodyInertial")
    assert frame_from_script(SAMPLE, "Missing") is None


def test_parse_report_drops_backward_rows(tmp_path):
    from altavista.scenario import parse_report
    p = tmp_path / "Sat.rpt"
    rows = ["1.0,1,0,0,0,1,0", "1.5,2,0,0,0,1,0", "1.2,9,9,9,9,9,9", "1.5,3,0,0,0,2,0", "2.0,4,0,0,0,1,0", "garbage"]
    p.write_text("\n".join(rows) + "\n")
    tr = parse_report(str(p))
    assert tr.t == [1.0, 1.5, 1.5, 2.0]          # backward row dropped, equal epoch kept
    assert tr.pos[2] == [3.0, 0.0, 0.0]


def test_decimate_keeps_endpoints():
    tr = Trajectory("x")
    for i in range(100):
        tr.append(float(i), [i, 0, 0, 1, 0, 0])
    decimate(tr, 10)
    assert len(tr.t) <= 12
    assert tr.t[0] == 0.0 and tr.t[-1] == 99.0
    assert len(tr.pos) == len(tr.t) == len(tr.vel)


def test_scenario_data_json_shape():
    tr = Trajectory("Sat", color="#fff")
    tr.append(100.0, [1, 2, 3, 4, 5, 6])
    tr.append(100.5, [2, 3, 4, 5, 6, 7])
    data = ScenarioData(name="t", spacecraft=[tr])
    d = data.to_dict()
    assert d["t0"] == 100.0 and d["t1"] == 100.5
    assert d["spacecraft"][0]["pos"] == [1, 2, 3, 2, 3, 4]
    assert d["spacecraft"][0]["vel"] == [4, 5, 6, 5, 6, 7]
    assert d["frame"]["name"] == "EarthMJ2000Eq"
    assert "published" in d["meta"]
    # M4.1: `frames` is additive -- present, empty by default, existing keys/shape above
    # unchanged (docs/open-questions.md question 78).
    assert d["frames"] == []
    # M7.1: `stateSpaces` is additive too -- present, empty by default (question 88).
    assert d["stateSpaces"] == []
    # ...and every recorded trajectory now declares which state space it's in.
    assert d["spacecraft"][0]["stateSpaceId"] == "altavista.cartesian_pos_vel_6"


def test_scenario_data_frames_key_carries_supplied_entries():
    """`ScenarioData.frames` is a plain pass-through list (altavista/scenario.py's
    `_build_frames` populates it with protobuf-JSON-transcoded dicts; this module has
    no protobuf dependency of its own -- see altavista/model.py's docstring)."""
    data = ScenarioData(name="t", frames=[{"id": "EarthMJ2000Eq", "axes": "AXES_KIND_MJ2000_EQ", "body": "Earth"}])
    d = data.to_dict()
    assert d["frames"] == [{"id": "EarthMJ2000Eq", "axes": "AXES_KIND_MJ2000_EQ", "body": "Earth"}]


def test_trajectory_attitude_is_additive_and_empty_by_default():
    """M5.2: `Trajectory.attitude` (a per-sample [x,y,z,w] quaternion stream, parallel
    to `t` -- groundwork for sensor footprints, web/js/scene.js's per-entity
    body-frame node) is additive: unset by default, and its wire form is a flat empty
    list like `pos`/`vel` would be for an empty trajectory -- not present-but-null,
    not omitted. This is what web/js/scene.js's `hasAttitude` check
    (`s.attitude.length === s.t.length * 4`) relies on to correctly choose the
    nadir-pointing VVLH fallback when no real attitude data was recorded.
    """
    tr = Trajectory("Sat")
    tr.append(100.0, [1, 2, 3, 4, 5, 6])
    d = tr.to_dict()
    assert d["attitude"] == []


def test_trajectory_attitude_flattens_like_pos_and_vel():
    tr = Trajectory("Sat", attitude=[[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.70710678, 0.70710678]])
    tr.append(100.0, [1, 2, 3, 4, 5, 6])
    tr.append(100.5, [2, 3, 4, 5, 6, 7])
    d = tr.to_dict()
    assert d["attitude"] == [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.70710678, 0.70710678]


def test_trajectory_to_dict_state_space_id_upgrades_with_attitude():
    """M7.1 (question 88): `Trajectory.to_dict()["stateSpaceId"]` names the plain
    6-component id when `attitude` is empty, and the 10-component attitude id when it is
    populated -- the same rule altavista/cdm.py's trajectory_to_cdm() upgrade uses, so a
    scenario built here and a CDM bundle built from the same data never disagree about
    which shape a trajectory is."""
    plain = Trajectory("Sat")
    plain.append(100.0, [1, 2, 3, 4, 5, 6])
    assert plain.to_dict()["stateSpaceId"] == "altavista.cartesian_pos_vel_6"

    with_att = Trajectory("Sat", attitude=[[0.0, 0.0, 0.0, 1.0]])
    with_att.append(100.0, [1, 2, 3, 4, 5, 6])
    assert with_att.to_dict()["stateSpaceId"] == "altavista.cartesian_pos_vel_6_attitude_quat_4"


def test_scenario_data_state_spaces_key_carries_supplied_entries():
    """`ScenarioData.state_spaces` is a plain pass-through list, like `frames` (M7.1;
    altavista/scenario.py's `_build_state_spaces` populates it with protobuf-JSON-transcoded
    StateSpace dicts -- this module has no protobuf dependency of its own)."""
    data = ScenarioData(name="t", state_spaces=[{"id": "altavista.cartesian_pos_vel_6", "components": []}])
    d = data.to_dict()
    assert d["stateSpaces"] == [{"id": "altavista.cartesian_pos_vel_6", "components": []}]


def test_scenario_data_footprints_key_is_additive_and_empty_by_default():
    """M6.3: `ScenarioData.footprints` (the sensor-footprint consumer of
    `Trajectory.attitude`) is additive, like `frames` before it -- unset by default,
    present as an empty list, so a scenario/test built before M6.3 still round-trips
    unchanged (test_scenario_data_json_shape above)."""
    data = ScenarioData(name="t")
    assert data.to_dict()["footprints"] == []


def test_footprint_to_dict_shape():
    fp = Footprint(
        name="ISS_footprint", spacecraft="ISS", half_angle_deg=10.0, axis=[1.0, 0.0, 0.0],
        color="#00e5ff", t=[100.0, 100.1],
        center=[[7000.0, 0.0, 0.0], None],
        ring=[[7000.0, 10.0, 0.0, 7000.0, -10.0, 0.0], []],
    )
    d = fp.to_dict()
    assert d == {
        "name": "ISS_footprint", "spacecraft": "ISS", "halfAngleDeg": 10.0, "axis": [1.0, 0.0, 0.0],
        "color": "#00e5ff", "t": [100.0, 100.1],
        "center": [[7000.0, 0.0, 0.0], None],
        "ring": [[7000.0, 10.0, 0.0, 7000.0, -10.0, 0.0], []],
    }


def test_scenario_data_footprints_key_carries_supplied_entries():
    fp = Footprint(name="f", spacecraft="Sat", half_angle_deg=5.0, axis=[0.0, 0.0, 1.0])
    data = ScenarioData(name="t", footprints=[fp])
    d = data.to_dict()
    assert d["footprints"] == [fp.to_dict()]
    assert d["footprints"][0]["ring"] == []
    assert d["footprints"][0]["center"] == []
