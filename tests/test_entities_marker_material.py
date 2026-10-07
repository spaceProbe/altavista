"""The screen-space entity marker's material, under node (web/js/entities_marker_material_check.mjs):
the vertex-shader rewrite against the vendored three's real `basic` shader, the viewport size it is
given, the residency scene's mesh, and the frustum-culling hazard `frustumCulled = false` answers.
The marker's pixel size is measured in real Chrome by tests/test_viewer_entity_framing_browser.py.
`node` is required (skipped, not faked, if absent)."""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_marker_material_check.mjs"
NODE = shutil.which("node")


@pytest.fixture(scope="module")
def data() -> dict:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/entities/ is ES modules, run for real under node.")
    proc = subprocess.run([NODE, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=30)
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{CHECK.name} did not print valid JSON (exit {proc.returncode}): {proc.stdout!r}\nstderr: {proc.stderr}")


def test_all_marker_material_checks_pass(data):
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert not failed, f"marker material checks failed: {failed}"
    assert data["allPass"] is True


def test_check_names_are_distinct(data):
    names = [c["name"] for c in data["checks"]]
    assert len(names) == len(set(names)) >= 20, names
