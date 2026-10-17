"""Question 245: the feasibility panel's data path serves a study that lives in ClickHouse.

The panel reads a ``SweepResults`` posted to ``POST /api/cdm/sweep`` (``tests/test_cdm_sweep.py``
tests that route on a frozen real study). This file proves the step before it: a study written
into the real ClickHouse study store comes back out through ``av-sweep``'s new read-out mode as
the same ``SweepResults``, and the published scenario is **equal to the one published from the
file store's copy** (and to the one published from the frozen fixture itself).

The path, all through the real binary and a real ClickHouse server:

1. ``av-sweep --import-study <frozen fixture> --store-dir D --store-clickhouse URL ...`` writes the
   frozen 4-point x 2-draw study (a genuine GMAT study) into the file store and into ClickHouse in
   one run (the password reaches the binary only through an environment variable);
2. ``av-sweep --read-study demo_two_instance_sweep --out-pb ...`` reads it back out of each store;
3. each file is POSTed to ``/api/cdm/sweep`` using the ``client`` fixture of
   ``tests/test_cdm_sweep.py`` (imported, not rewritten), and the scenarios are compared.

Gating (visible, never silent): skipped with a reason when Docker is down or the ClickHouse image
is absent or differs from ``services/clickhouse/IMAGE_DIGEST.md``'s record (the test never pulls).
The container is started under ``altavista.docker_test_lock.lock_docker_tests`` taken by THIS
process, so run it unwrapped; as a Python run that launches workspace binaries it holds a cargo
slot for its whole run::

    scripts/dev/cargo-slot --hold -- .venv/bin/python -m pytest -q -rs tests/test_clickhouse_sweep_panel.py
"""
from __future__ import annotations

import os
import subprocess
import sys
import time
import uuid
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest

from altavista.container_hardening import label_args, prune_stale_labelled_resources
from altavista.docker_test_lock import lock_docker_tests
from altavista.pb.altavista.v1 import run_pb2
from altavista.test_env import resolve_gmat_root

# The panel fixture and helper of tests/test_cdm_sweep.py, reused rather than written a second time.
from test_cdm_sweep import FIXTURE_PATH, client, fixture_bytes  # noqa: F401  (fixtures are used by name)
from heavy_stack import _docker_daemon_unavailable_reason, _fenced_block_after, _local_image_id

REPO_ROOT = Path(__file__).resolve().parents[1]
CARGO_SLOT = REPO_ROOT / "scripts" / "dev" / "cargo-slot"
AV_SWEEP = REPO_ROOT / "target" / "debug" / "av-sweep"
IMAGE_DIGEST_MD = REPO_ROOT / "services" / "clickhouse" / "IMAGE_DIGEST.md"
RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
SWEEP_ID = "demo_two_instance_sweep"
DATABASE = "avstudies"


def _skip_reason() -> str | None:
    daemon = _docker_daemon_unavailable_reason()
    if daemon is not None:
        return f"Docker not available: {daemon}"
    text = IMAGE_DIGEST_MD.read_text()
    image_ref = _fenced_block_after(text, "Registry reference")
    recorded_id = _fenced_block_after(text, "docker image inspect")
    if not image_ref or not recorded_id:
        return f"could not find the image reference / recorded id fenced blocks in {IMAGE_DIGEST_MD}"
    if "@sha256:" not in image_ref:
        return f"the recorded image reference {image_ref!r} is not by digest"
    actual = _local_image_id(image_ref)
    if actual is None:
        return f"image {image_ref!r} is not present locally (this test never pulls; see {IMAGE_DIGEST_MD} for the one-time pull)"
    if actual != recorded_id:
        return f"image {image_ref!r} is present but its id {actual!r} differs from the recorded {recorded_id!r}"
    return None


SKIP_REASON = _skip_reason()


def _env() -> dict:
    gmat_root, reason = resolve_gmat_root()
    if gmat_root is None:
        pytest.fail(f"GMAT_ROOT does not resolve ({reason}); the av-sweep binary links GMAT")
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    env["GMAT_ROOT"] = gmat_root
    env.setdefault("CARGO_BUILD_JOBS", "4")
    return env


@pytest.fixture(scope="module")
def av_sweep_bin() -> Path:
    proc = subprocess.run([sys.executable, str(CARGO_SLOT), "build", "-p", "av-sweep", "--bin", "av-sweep"],
                          cwd=str(REPO_ROOT), env=_env(), capture_output=True, text=True, timeout=3600)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-sweep failed (rc={proc.returncode}):\n{proc.stdout}\n{proc.stderr}")
    assert AV_SWEEP.is_file(), f"expected {AV_SWEEP} after a successful build"
    return AV_SWEEP


@pytest.fixture(scope="module")
def clickhouse():
    if SKIP_REASON is not None:
        pytest.skip(SKIP_REASON)
    text = IMAGE_DIGEST_MD.read_text()
    image_ref = _fenced_block_after(text, "Registry reference")
    recorded_id = _fenced_block_after(text, "docker image inspect")
    run_id = f"{__name__}-{uuid.uuid4()}"
    tag = "".join(c for c in run_id if c.isalnum())[-24:]
    user, password = f"avuser{tag}", f"avpw{tag}x"
    with lock_docker_tests():
        prune_stale_labelled_resources()
        cmd = ["docker", "run", "-d", *label_args(run_id), "--tmpfs", "/var/lib/clickhouse",
               "-p", "127.0.0.1::8123",
               "-e", f"CLICKHOUSE_USER={user}", "-e", f"CLICKHOUSE_PASSWORD={password}", "-e", f"CLICKHOUSE_DB={DATABASE}",
               image_ref]
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        if result.returncode != 0:
            pytest.fail(f"`docker run` (ClickHouse) failed: {result.stderr}")
        container_id = result.stdout.strip()
        try:
            running = subprocess.run(["docker", "inspect", "--format", "{{.Image}}", container_id], capture_output=True, text=True, timeout=30).stdout.strip()
            assert running == recorded_id, f"the RUNNING container's image {running!r} must equal the recorded digest {recorded_id!r}"
            port_out = subprocess.run(["docker", "port", container_id, "8123/tcp"], capture_output=True, text=True, timeout=30).stdout
            port = int(port_out.strip().splitlines()[0].rsplit(":", 1)[-1])
            deadline = time.monotonic() + 120
            last = "never attempted"
            while time.monotonic() < deadline:
                try:
                    r = httpx.post(f"http://127.0.0.1:{port}/", content="SELECT 1", timeout=3.0,
                                   headers={"X-ClickHouse-User": user, "X-ClickHouse-Key": password})
                    if r.status_code == 200 and r.text.strip() == "1":
                        break
                    last = f"HTTP {r.status_code}: {r.text[:200]}"
                except httpx.HTTPError as e:
                    last = str(e)
                time.sleep(0.25)
            else:
                logs = subprocess.run(["docker", "logs", "--tail", "40", container_id], capture_output=True, text=True).stdout
                pytest.fail(f"ClickHouse (container {container_id}) not ready: {last}\n{logs}")
            yield SimpleNamespace(url=f"http://127.0.0.1:{port}", user=user, password=password, container_id=container_id)
        finally:
            subprocess.run(["docker", "rm", "-fv", container_id], capture_output=True, timeout=60)


def _store_args(ch, store_dir: Path | None, *, clickhouse: bool) -> list:
    args = []
    if store_dir is not None:
        args += ["--store-dir", str(store_dir)]
    if clickhouse:
        args += ["--store-clickhouse", ch.url, "--clickhouse-database", DATABASE, "--clickhouse-user", ch.user,
                 "--clickhouse-password-env", "AV_CH_TEST_PASSWORD"]
    return args


def _run(bin_path: Path, ch, args: list) -> subprocess.CompletedProcess:
    env = _env()
    env["AV_CH_TEST_PASSWORD"] = ch.password  # the only way the password reaches the binary
    proc = subprocess.run([str(bin_path), *args], cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=300)
    assert ch.password not in proc.stdout + proc.stderr, "the password must never be printed by av-sweep"
    assert ch.password not in " ".join(args), "the password must never be on a command line"
    return proc


def test_a_clickhouse_backed_study_publishes_the_same_scenario_as_the_file_stores_copy(
        av_sweep_bin, clickhouse, client, fixture_bytes, tmp_path):
    store_dir = tmp_path / "store"
    # 1. The frozen real study into BOTH stores, in one run.
    proc = _run(av_sweep_bin, clickhouse, ["--import-study", str(FIXTURE_PATH), *_store_args(clickhouse, store_dir, clickhouse=True)])
    assert proc.returncode == 0, proc.stderr
    assert proc.stderr.count("written to") == 2, proc.stderr

    # Importing the identical study again is an idempotent no-op (studies are immutable in ClickHouse).
    again = _run(av_sweep_bin, clickhouse, ["--import-study", str(FIXTURE_PATH), *_store_args(clickhouse, None, clickhouse=True)])
    assert again.returncode == 0, again.stderr

    # 2. Read it back out of each store.
    from_ch = tmp_path / "from_clickhouse.pb"
    from_file = tmp_path / "from_file_store.pb"
    proc = _run(av_sweep_bin, clickhouse, ["--read-study", SWEEP_ID, "--out-pb", str(from_ch), "--out-json", str(tmp_path / "from_clickhouse.json"),
                                          *_store_args(clickhouse, None, clickhouse=True)])
    assert proc.returncode == 0, proc.stderr
    proc = _run(av_sweep_bin, clickhouse, ["--read-study", SWEEP_ID, "--out-pb", str(from_file), *_store_args(clickhouse, store_dir, clickhouse=False)])
    assert proc.returncode == 0, proc.stderr

    frozen = run_pb2.SweepResults.FromString(fixture_bytes)
    ch_msg = run_pb2.SweepResults.FromString(from_ch.read_bytes())
    file_msg = run_pb2.SweepResults.FromString(from_file.read_bytes())
    assert ch_msg == file_msg == frozen, "the study read out of ClickHouse is the study that went in"
    assert len(ch_msg.samples) == 8 and len(ch_msg.aggregates) == 12
    assert from_ch.read_bytes() == from_file.read_bytes(), "the same canonical protobuf bytes from either store"

    # 3. Publish each through the panel's route and compare the scenarios.
    def publish(body: bytes) -> dict:
        resp = client.post("/api/cdm/sweep", content=body, headers={"content-type": "application/x-protobuf"})
        assert resp.status_code == 200, resp.text
        name = resp.json()["name"]
        assert name == f"sweep:{SWEEP_ID}"
        got = client.get(f"/api/scenario/{name}")
        assert got.status_code == 200, got.text
        return got.json()

    scenario_ch = publish(from_ch.read_bytes())
    scenario_file = publish(from_file.read_bytes())
    scenario_frozen = publish(fixture_bytes)
    assert scenario_ch == scenario_file, "the scenario published from ClickHouse equals the one from the file store's copy"
    assert scenario_ch == scenario_frozen, "...and the one published from the frozen fixture itself"
    assert scenario_ch["sweep"]["sweepId"] == SWEEP_ID
    assert len(scenario_ch["sweep"]["points"]) == 4 and len(scenario_ch["sweep"]["aggregates"]) == 12


def test_reading_a_study_that_is_not_in_the_store_exits_nonzero_with_a_clear_message(av_sweep_bin, clickhouse, tmp_path):
    proc = _run(av_sweep_bin, clickhouse, ["--read-study", "no_such_study", "--out-pb", str(tmp_path / "x.pb"),
                                          *_store_args(clickhouse, None, clickhouse=True)])
    assert proc.returncode != 0
    assert "holds no study" in proc.stderr, proc.stderr
    assert not (tmp_path / "x.pb").exists()
