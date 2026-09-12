"""E5 (docs/edge-plan.md milestone E5): "the tracks are compared against the run's truth
trajectory ... and published to the viewer as an entity beside the truth."

The existing path is reused verbatim, per this milestone's own instruction ("do not build a
new panel unless the existing ones genuinely cannot show it, and say which you found"):
``POST /api/cdm/run`` already accepts an ``altavista.v1.RunProducts`` carrying a
``trajectories`` map keyed by instance id (``tests/test_cdm_run.py``'s own, unmodified
route) and ``web/js/panels/run_products_panel.js`` / ``altavista/cdm.py`` already render
every trajectory that map contains as its own spacecraft entity -- nothing about either
consumer assumes exactly one trajectory per bundle, so a second, engine-produced
trajectory keyed ``"flight_track"`` beside the DRM's own truth trajectory (keyed
``"flight"``) needs no new endpoint, no new panel, and no viewer-side code change at all.
This test proves exactly that: both entities are published and both are readable back
through the real route, in-process, headlessly (``fastapi.testclient.TestClient``, the
same pattern ``tests/test_cdm_run.py`` already uses -- no real socket, no network at test
time, question 154).

``crates/av-track/src/bin/av-track-demo.rs`` builds the augmented ``RunProducts`` entirely
offline (the committed ``crates/av-edge/tests/fixtures/ground_segment/*.pb`` fixture, an
in-process, no-socket ``av_ingest::ingest::Ingest``, this track's own consumer and engine
bridge -- see that binary's own module doc) and writes it to ``--out``; this test builds
that binary once (module-scoped, "cargo build, fail loudly on a build error" -- ``tests/
test_cdm_run.py::av_run_bin``'s own precedent) and reads back its exact output bytes,
exactly like ``tests/test_cdm_run.py::run_bundle_bytes`` does for ``av-run``.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from altavista.pb.altavista.v1 import run_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parents[1]
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


@pytest.fixture(scope="module")
def av_track_demo_bin():
    """Builds ``crates/av-track``'s ``av-track-demo`` binary once for the module -- a build
    failure is a real failure of this task (``tests/test_cdm_run.py::av_run_bin``'s own
    posture), never silently skipped."""
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-track", "--bin", "av-track-demo"],
        cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-track --bin av-track-demo failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "av-track-demo"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def track_run_bundle_bytes(av_track_demo_bin, tmp_path_factory) -> bytes:
    """Runs the real ``av-track-demo`` binary (real engine bridge, real chain-verified
    in-process ingest, real comparison against the fixture's own truth trajectory -- no
    network, question 154) and returns the exact ``RunProducts`` wire bytes it wrote to
    ``--out``."""
    out = tmp_path_factory.mktemp("av_track_demo") / "bundle.pb"
    proc = subprocess.run(
        [str(av_track_demo_bin), "--out", str(out)],
        cwd=str(REPO_ROOT), capture_output=True, text=True, timeout=180)
    if proc.returncode != 0:
        pytest.fail(f"av-track-demo failed (rc={proc.returncode}):\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return out.read_bytes()


@pytest.fixture()
def client(tmp_path) -> TestClient:
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)
    return TestClient(app)


def test_track_bundle_bytes_are_a_real_run_products_with_both_trajectories(track_run_bundle_bytes: bytes):
    """Direct decode of the real ``av-track-demo`` output, independent of the server route
    below -- proves the binary itself produced both trajectories before the HTTP layer is
    even involved (``tests/test_cdm_run.py::
    test_run_bundle_bytes_are_a_real_run_products_message_with_frames_and_scores``'s own
    precedent)."""
    rp = run_pb2.RunProducts()
    rp.ParseFromString(track_run_bundle_bytes)
    assert "flight" in rp.trajectories, "the fixture's own truth trajectory must still be present"
    assert "flight_track" in rp.trajectories, "the engine-produced track trajectory must be present beside the truth"
    truth = rp.trajectories["flight"]
    track = rp.trajectories["flight_track"]
    assert len(truth.samples) > 0
    assert len(track.samples) > 0
    # Every track sample's mean is a 6-vector (air_3d: pos_x,pos_y,pos_z,vel_x,vel_y,vel_z).
    assert all(len(s.mean) == 6 for s in track.samples)


def test_publishing_the_track_bundle_shows_the_track_entity_beside_the_truth_entity(client: TestClient, track_run_bundle_bytes: bytes):
    """The actual viewer deliverable: ``POST /api/cdm/run`` (unmodified) followed by
    ``GET /api/scenario/{name}`` (unmodified) -- the exact JSON shape a browser's
    ``loadScenario()`` receives -- must show a spacecraft entity named ``"flight_track"``
    alongside the truth entity, through the existing route and the existing conversion
    (``altavista.cdm.cdm_trajectory_to_viewer_json``), with no code in either changed for
    this test."""
    resp = client.post("/api/cdm/run", content=track_run_bundle_bytes, headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]

    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    scenario = resp.json()

    names = [sc["name"] for sc in scenario["spacecraft"]]
    assert "flight_track" in names, f"expected a 'flight_track' spacecraft entity beside the truth; got {names}"
    assert len(scenario["spacecraft"]) >= 2, "the truth entity and the track entity must both be published, not one replacing the other"

    track_entity = next(sc for sc in scenario["spacecraft"] if sc["name"] == "flight_track")
    assert len(track_entity["t"]) > 0, "the track entity must carry real sample epochs, not an empty trajectory"
