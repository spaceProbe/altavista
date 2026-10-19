"""The Python twin of ``av_cdm::path_id`` (question 254 item 1) against the shared vectors.

``data/path_component_id_vectors.json`` is read by this file and by the unit tests in
``crates/av-cdm/src/path_id.rs``; neither side has a private list of ids.
"""
import json
from pathlib import Path

import pytest

from altavista import path_id

VECTORS = json.loads((Path(__file__).resolve().parent.parent / "data" / "path_component_id_vectors.json").read_text())


def test_the_fixture_states_the_modules_limit():
    assert VECTORS["max_len"] == path_id.MAX_LEN == 128


@pytest.mark.parametrize("value", VECTORS["accepted"])
def test_every_accepted_vector_passes_unchanged(value):
    assert path_id.validate_path_component_id(value) == value


@pytest.mark.parametrize("entry", VECTORS["refused"], ids=lambda e: repr(e["id"])[:40])
def test_every_refused_vector_is_refused_for_its_recorded_reason(entry):
    with pytest.raises(path_id.PathIdError) as err:
        path_id.validate_path_component_id(entry["id"])
    assert err.value.reason == entry["reason"]
    assert isinstance(err.value, ValueError)


def test_the_fixture_exercises_every_reason():
    assert {e["reason"] for e in VECTORS["refused"]} == {"empty", "too_long", "disallowed_byte", "bad_first_byte"}


def test_a_non_string_and_a_lone_surrogate_are_refused():
    with pytest.raises(path_id.PathIdError):
        path_id.validate_path_component_id(None)  # type: ignore[arg-type]
    with pytest.raises(path_id.PathIdError):
        path_id.validate_path_component_id("a\ud800b")
