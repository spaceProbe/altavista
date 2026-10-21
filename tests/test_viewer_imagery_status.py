"""The viewer's handling of a Blue Marble basemap that is not installed (question 264, task 3).

``web/js/imagery_status_check.mjs`` drives the real ``imagery_status.js`` and ``panels/map_panel.js``
``render()`` (with a minimal DOM stand-in; no ``document`` exists under plain ``node``): a scenario
whose ``imagery.installed`` is ``false`` gets NO tile images (neither the globe's nor the 2D map's
would be anything but 404s) and the server's notice instead, while a source with no verdict (the
test fixture) and an installed pyramid behave as before. The globe checkbox and RPO-layout wiring in
``web/js/app.js`` needs a real DOM and WebGL, so it is proved in a real browser against a live server
(see the round report) and pinned here at source level: both entry points must consult the verdict
before ``enableGlobe``.
"""
from __future__ import annotations

import json
import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
CHECK = REPO_ROOT / "web" / "js" / "imagery_status_check.mjs"
APP_JS = REPO_ROOT / "web" / "js" / "app.js"

pytestmark = pytest.mark.skipif(shutil.which("node") is None, reason="node is not on PATH; the viewer harness needs it")


@pytest.fixture(scope="module")
def checks() -> list:
    proc = subprocess.run(["node", str(CHECK)], capture_output=True, text=True, cwd=str(REPO_ROOT), timeout=60)
    assert proc.stdout.strip(), f"imagery_status_check.mjs printed nothing (rc={proc.returncode}): {proc.stderr[-800:]}"
    return json.loads(proc.stdout)


def _failed(checks: list, prefix: str = "") -> list:
    return [c for c in checks if c["name"].startswith(prefix) and not c["pass"]]


def test_the_harness_ran_every_check(checks):
    assert len(checks) >= 11
    assert not _failed(checks), _failed(checks)


def test_a_not_installed_basemap_draws_no_tiles_and_shows_the_notice_on_the_2d_map(checks):
    mine = [c for c in checks if c["name"].startswith("map: not installed")]
    assert len(mine) == 3 and all(c["pass"] for c in mine), mine


def test_an_installed_or_unjudged_source_still_draws_its_mosaic(checks):
    mine = [c for c in checks if c["name"].startswith("map: installed") or "no verdict" in c["name"]]
    assert len(mine) == 3 and all(c["pass"] for c in mine), mine


def test_the_globe_info_lines(checks):
    mine = [c for c in checks if c["name"].startswith("globe-info")]
    assert len(mine) == 2 and all(c["pass"] for c in mine), mine


def test_both_globe_entry_points_in_app_js_consult_the_verdict_before_enabling_the_globe():
    """The checkbox handler and `setupRpoViewports` (the RPO layout turns the globe on by itself)
    must each test `basemapNotInstalled(...)` before their `viewer.enableGlobe(...)` call."""
    src = APP_JS.read_text()
    calls = [m.start() for m in re.finditer(r"viewer\.enableGlobe\('Earth'", src)]
    assert len(calls) == 2, "expected exactly the checkbox and the RPO layout to call enableGlobe"
    for at in calls:
        before = src[max(0, at - 700):at]
        assert "basemapNotInstalled(imagery)" in before, "an enableGlobe call is not preceded by the verdict check"
        assert "globeNotInstalledInfo(imagery)" in before
    assert "from './imagery_status.js'" in src
