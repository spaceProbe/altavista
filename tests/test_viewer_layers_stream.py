"""H5b-2 deliverable 4 (docs/heavy-plan.md H5, round 3): the test that stands the REAL
stack up end to end -- H5's own remaining headless claim, made concrete: "a headless
harness measures frame time while a tile set streams from a real av-tiles gateway and
asserts no frame exceeds the budget and the memory budget is never crossed."

# What this stands up, for real

Every piece of infrastructure this file needs -- the digest gate, the host-wide-locked
labelled MinIO container, the `cargo build`s, `av-tile-fixture`, the `LocalTestIssuer`
and its token, and the real `av-tiles` subprocess -- comes from `tests/heavy_stack.py`
(deliverable 4's own extraction out of `tests/test_viewer_tiles_route.py`, see that
module's own docstring for the full reasoning; not repeated here). This file adds
exactly one more real thing on top: the viewer server itself, started as a REAL OS
process (`_start_viewer_server`, below) -- `python -m altavista serve`, configured
with `--tiles-endpoint`/`--tiles-token-path` pointed at the real `av-tiles` subprocess
`heavy_stack.av_tiles_service` already started, listening on an OS-assigned ephemeral
loopback port, polled for readiness against its own real `/api/health` route (never a
fixed sleep -- the identical pattern `tests/test_viewer_net.py::_start_altavista_
server` already uses for the same server, duplicated in shape rather than imported per
this repo's "each viewer test file stays self-contained" convention that module's own
docstring states). `node` cannot reach a `fastapi.testclient.TestClient` (it is not a
real socket) -- this is exactly why this test needs a real subprocess where `tests/
test_viewer_tiles_route.py` could get away with an in-process `TestClient`.

Once that real server is listening, `web/js/layers_stream_check.mjs` (H5b-2 deliverable
3 -- see that file's own module docstring for exactly what a "frame" is and why real
network time is deliberately NOT inside it) is run as a real `node` subprocess against
that server's own real origin, with the real tile set's real manifest hash. Every tile
byte this test's own assertions depend on crossed a real loopback socket, through the
real FastAPI proxy route (`altavista/server.py`'s `tiles_tile`), to the real `av-tiles`
gateway, to the real MinIO object store `av-tile-fixture` populated.

# The frame budget: FRAME_BUDGET_MS, and why

Chosen and recorded here, not in the harness (`layers_stream_check.mjs` takes it as a
CLI argument specifically so the CALLER commits to a number and a reason -- see that
file's own module docstring). Measured directly, twice:

  - Against a local, network-free smoke-test HTTP server (5ms artificial per-tile
    delay, no docker, no real gateway -- used only to develop and tune this harness's
    own dwell/concurrency logic before ever touching the real stack): `maxFrameMs`
    across several runs stayed under 2ms, with `p95FrameMs` under 0.03ms -- the one
    outlier was the very first frame (a few hundred microseconds to ~1ms), consistent
    with V8/Node module and `node:crypto` first-call warm-up, not per-tile cost.
  - Against THIS test's own real stack (real MinIO, real av-tiles, real viewer server,
    real loopback `fetch`, real cargo-built binaries, on this exact host): four
    consecutive full runs measured `maxFrameMs` of 1.79ms, 1.86ms, 1.86ms and 1.81ms
    (`p95FrameMs` under 0.021ms every time -- the max is a single-frame outlier, the
    same first-call-warm-up shape as the smoke-test measurement above, not a
    per-tile-scaling cost). See `test_stream_report`'s own printed `maxFrameMs` for
    what THIS specific run measured.

FRAME_BUDGET_MS was originally set to 250 -- two orders of magnitude above the real
stack's own measured maximum, so generous that H5's actual claim ("never stalls a
frame") could not fail against it for any implementation anyone would write: 250ms is
itself more than a dozen dropped frames at 60Hz, so a 130x margin was proving nothing
about frame timing, only that the harness ran at all. Corrective round 3 tightened it
to FRAME_BUDGET_MS = 16.7 -- one frame at 60Hz, the number H5's claim is actually
about -- and re-measured five CONSECUTIVE full runs against this exact real stack
(same real MinIO, real av-tiles, real viewer server, real loopback `fetch`, on this
exact host) to confirm the new budget holds under real, current host load rather than
assuming the four historical numbers above still apply:

    run 1: maxFrameMs=1.7563ms  (evictedCount=7, cancelledCount=4)
    run 2: maxFrameMs=1.7548ms  (evictedCount=7, cancelledCount=4)
    run 3: maxFrameMs=1.9488ms  (evictedCount=6, cancelledCount=4)
    run 4: maxFrameMs=1.7863ms  (evictedCount=7, cancelledCount=4)
    run 5: maxFrameMs=1.8330ms  (evictedCount=9, cancelledCount=4)

All five cleared 16.7ms with an 8.5x-9.5x margin (worst case: run 3's 1.9488ms is
16.7 / 1.9488 ~= 8.6x under budget), consistent with the four historical runs above,
so the tighter budget was kept rather than loosened -- this task's own binding rule:
"a budget that was widened must say what widened it" implies the converse duty when a
budget is tightened instead, which is exactly this paragraph. This host is still
over-subscribed and shared with another track (this task's own binding instructions;
confirmed directly while developing the ORIGINAL 250ms version of this test: `docker
events`, captured around a real run of this file, showed containers from an entirely
unrelated `cohort_backend`/Supabase stack cycling concurrently on this same host) --
16.7ms is not a "one frame, no room at all" assertion, it still carries roughly an
order of magnitude of margin over every measurement taken on this host so far, while
being tight enough that a genuinely broken implementation -- one that, say,
accidentally moved the real `fetch()` await, or a real cryptographic digest of the
WHOLE tile set, inside the timed body -- would still fail it (such a regression would
land in the tens-to-hundreds-of-milliseconds range per frame, given this fixture's own
real per-tile round trip measured 42-53ms across every run in this file's history).
If a future run on a more heavily loaded host ever fails this budget, the fix is to
re-run this same five-consecutive-runs protocol, record the real numbers here, and
move to the next multiple of a 60Hz frame (33.4ms for two) only if genuinely needed --
never to loosen it on a single flaky observation.

# What each test would catch (this task's own standing review requirement -- "for each
test, be able to name the wrong implementation it would fail against")

* ``test_every_frame_stayed_within_the_chosen_budget``: an implementation that measures
  a frame's synchronous cost incorrectly -- e.g. one that accidentally `await`s the
  real `fetch()` INSIDE the timed body (this file's own "what a frame is" contract,
  restated from ``layers_stream_check.mjs``'s own module docstring) -- would report a
  `maxFrameMs` on the order of the real gateway round-trip time (milliseconds to tens
  of milliseconds under load), not microseconds; FRAME_BUDGET_MS = 16.7 (one 60Hz
  frame, see "The frame budget" above) is tight enough to actually catch that, unlike
  the original 250ms, which this test also prints the real `maxFrameMs` for
  (``test_stream_report``) so a reviewer can see the real number, not just a boolean.
* ``test_memory_budget_was_respected_and_the_soft_violation_branch_never_fired``: a
  manager with no real eviction (`maxResidentBytesObserved` would climb past
  `MEMORY_BUDGET_BYTES`) fails the first half; an implementation whose eviction only
  APPEARS to respect the budget because it never had anything left to evict without
  soft-violating (see `web/js/layers/layer.js`'s own `_evictIfNeeded` doc comment: a
  budget so tight nothing fits under it makes `budgetRespected` true for the wrong
  reason) fails the second half, `softViolationTaken`.
* ``test_real_bytes_really_crossed_the_gateway_and_every_one_was_etag_verified``: a
  harness (or an adapter) that never actually reaches the real gateway at all (e.g. a
  broken `origin`, a manifest hash mismatch, or a URL-building bug) would report
  `tilesFetched == 0`, failing outright; an adapter that trusts the gateway's `ETag`
  without recomputing the SHA-256 over the bytes it actually received (see
  `web/js/layers/gateway_imagery_layer.js`'s own module docstring) could still report
  tiles "fetched" while `etagVerifiedCount < tilesFetched` -- this test's own equality
  assertion (not merely `> 0`) is what would catch that.
* ``test_every_http_status_seen_is_one_the_design_expects``: a URL-building bug (wrong
  level/x/y addressing, a manifest hash typo, a route path typo) would 404 against the
  real gateway -- this test's own closed allowlist (`{"200"}` only, for this run's
  camera path, which touches only real, existing tile addresses -- see this task's own
  investigation into `crates/av-jobs/src/scheme.rs`'s `tiles_covering` confirming every
  `(level,x,y)` in `[0, tileCountX(level)) x [0, tileCountY(level))` at levels 0-2
  genuinely exists in this fixture) catches it, where a looser "some status came back"
  check would not.
* ``test_eviction_and_cancellation_both_actually_happened``: the same "only a
  meaningful test if it had to run" reasoning `tests/test_viewer_globe.py::test_tile_
  budget_is_respected`/`test_tile_loading_is_cancelled_on_camera_move` already use for
  the globe's own scheduler -- `evictedCount == 0` would mean `MEMORY_BUDGET_BYTES` was
  never actually approached (a budget check that passes only because it was never
  exercised proves nothing); `cancelledCount == 0` would mean the harness's own
  deliberate camera jump (`layers_stream_check.mjs`'s `CAMERA_PATH`, 'near-0-0-close' ->
  'jump-antipodal') never actually caught a real in-flight `fetch()` still pending,
  which would mean this test is not exercising cancellation against real network I/O
  at all, only against the stub `web/js/layers_check.mjs` already covers.

# No network at test time beyond loopback (question 154) / no environment mutation
(question 199)

Restated identically from `tests/test_command_console_routes.py`'s own module doc
(the first place in this suite to establish both points): binding/connecting to
`127.0.0.1` never leaves the host's own kernel network stack -- it is not "the
network" for question 154's purposes. Nothing in this file calls
`os.environ[...]`/`monkeypatch.setenv`; every setting reaches a subprocess as an
explicit CLI argument or a file this test itself wrote and named explicitly.
"""
from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest

from heavy_stack import (  # noqa: F401 -- rust_bins/minio/issuer/minio_bucket/tile_set_label/key_prefix are transitive fixture deps
    LADDER,
    MINIO_REGION,
    SKIP_REASON,
    av_tiles_service,
    issuer,
    key_prefix,
    minio,
    minio_bucket,
    rust_bins,
    tile_set,
    tile_set_label,
    token_at_clearance,
    _free_port,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
LAYERS_STREAM_CHECK = REPO_ROOT / "web" / "js" / "layers_stream_check.mjs"

NODE = shutil.which("node")

# See this module's own docstring, "The frame budget", for the full measurement and
# reasoning behind both constants below.
FRAME_BUDGET_MS = 16.7
# MEMORY_BUDGET_BYTES: sized the same way web/js/layers_check.mjs's own
# MEMORY_BUDGET_BYTES was (measured, not estimated) -- against this exact fixture
# shape (--min-level 0 --max-level 2 --tile-size 16 --synthetic-source 32x16, the
# whole-globe plate-carree grid this task's own investigation confirmed: 42 distinct
# tiles, 2 + 8 + 32 across levels 0-2), IMAGERY_TILE_BYTES's own 262,144-byte
# per-tile estimate (web/js/layers/imagery_layer.js) puts the path's full cumulative
# distinct-byte ceiling at 42 * 262,144 = 11,010,048 bytes if every distinct tile
# this harness's camera path ever requests became resident at once. A first measured
# run against this real stack, with a generous 4,500,000-byte budget, reached
# 3,932,160 resident bytes (15 tiles) without ever crossing it -- so 3,000,000 was
# picked instead: comfortably above any single LOW-demand position's own working set
# (the 'far' root-tile view: 2 tiles, 524,288 bytes), and comfortably below what an
# UNTHROTTLED run's own cumulative total reaches, so eviction must run partway
# through this real path -- see test_stream_report's own printed numbers for what
# THIS run actually measured.
MEMORY_BUDGET_BYTES = 3_000_000

READY_TIMEOUT_S = 60.0

# Round 6 (docs/open-questions.md question 231's ruling, "replace or composite"): a
# SECOND real tile set, built the SAME way `tests/heavy_stack.py`'s own `tile_set`
# fixture builds the first (same `av-tile-fixture` binary, same `minio`, same
# `key_prefix`/`tile_set_label` -- av-tiles' own `--key-prefix` flag scopes where it
# looks for objects in the bucket, so a second tile set must share it with whatever
# `av_tiles_service` was actually started with; only the JOB ID and the PYRAMID DEPTH
# differ) -- deliberately SHALLOWER (`--max-level 1` vs `tile_set`'s own 2) so the
# GlobeLayer proof (`layers_stream_check.mjs`'s own `twoSetProbe`) can construct "a
# tile the later set does not cover" against a REAL gateway, never assumed: this
# set's own real manifest genuinely lists only levels 0-1, so a real request for one
# of this camera's own level-2 tiles against THIS set's real `/api/tiles/<sha>/...`
# route genuinely 404s.
GLOBE_PROBE_TILE_SET_B_MAX_LEVEL = 1


@pytest.fixture(scope="module")
def tile_set_b(rust_bins, minio, key_prefix, tile_set_label):
    cmd = [
        str(rust_bins.tile_fixture),
        "--key-prefix", key_prefix,
        "--ladder", LADDER,
        "--label-marking", tile_set_label,
        "--job-id", f"{key_prefix}-job-globe-probe-b",
        "--min-level", "0",
        "--max-level", str(GLOBE_PROBE_TILE_SET_B_MAX_LEVEL),
        "--tile-size", "16",
        "--synthetic-source", "32x16",
        "--store-endpoint", f"http://127.0.0.1:{minio.host_port}",
        "--store-region", MINIO_REGION,
        "--store-access-key-id", minio.access_key,
        "--store-secret-access-key", minio.secret_key,
        "--store-bucket", minio.bucket,
        "--store-path-style",
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        pytest.fail(f"av-tile-fixture (tile_set_b) failed (returncode={proc.returncode}):\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    stdout_lines = [line for line in proc.stdout.splitlines() if line.strip()]
    assert stdout_lines, f"av-tile-fixture (tile_set_b) printed nothing on stdout; stderr:\n{proc.stderr}"
    result = json.loads(stdout_lines[-1])
    assert re.fullmatch(r"[0-9a-f]{64}", result["manifest_sha256"]), result
    return SimpleNamespace(**result)


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; web/js/layers_stream_check.mjs is an "
            "ES module and this test intentionally runs it for real (see module docstring) "
            "rather than porting its logic to Python, so it cannot proceed without node."
        )
    return NODE


# =================================================================================================
# The real viewer server -- a real OS subprocess, polled for readiness against its own real
# /api/health route. Duplicated in shape (not imported) from tests/test_viewer_net.py's own
# `_start_altavista_server`, per that module's own "each viewer test file stays self-contained"
# convention.
# =================================================================================================


@pytest.fixture(scope="module")
def tiles_token_path(tmp_path_factory, token_at_clearance) -> Path:
    path = tmp_path_factory.mktemp("layers_stream_token") / "tiles_token.txt"
    path.write_text(token_at_clearance)
    return path


@pytest.fixture(scope="module")
def viewer_server(av_tiles_service, tiles_token_path):
    """A real `python -m altavista serve` subprocess, on an OS-assigned ephemeral
    loopback port, configured with `--tiles-endpoint`/`--tiles-token-path` pointed at
    the real, already-running `av_tiles_service`. Polls `GET /api/health` (a real
    route, already proven in `tests/test_viewer_tiles_route.py`) via a bounded retry
    loop -- never a fixed sleep-then-assume."""
    port = _free_port()
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "altavista", "serve",
            "--host", "127.0.0.1", "--port", str(port),
            "--tiles-endpoint", av_tiles_service.endpoint,
            "--tiles-token-path", str(tiles_token_path),
            "--log-level", "warning",
        ],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    origin = f"http://127.0.0.1:{port}"
    try:
        deadline = time.monotonic() + READY_TIMEOUT_S
        ready = False
        last_err = "never attempted"
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                pytest.fail(f"altavista serve exited early (returncode={proc.returncode}):\n{proc.stdout.read()}")
            try:
                resp = httpx.get(f"{origin}/api/health", timeout=1.0)
                if resp.status_code == 200:
                    ready = True
                    break
                last_err = f"status {resp.status_code}"
            except httpx.HTTPError as e:
                last_err = str(e)
            time.sleep(0.05)
        if not ready:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            pytest.fail(f"altavista serve did not answer GET /api/health with 200 within {READY_TIMEOUT_S}s; last: {last_err}")
        yield SimpleNamespace(origin=origin, proc=proc)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


# =================================================================================================
# Running web/js/layers_stream_check.mjs for real, once per module, against that real server.
# =================================================================================================


@pytest.fixture(scope="module")
def stream_result(viewer_server, tile_set, tile_set_b) -> dict:
    node = _require_node()
    # Round 6 (docs/open-questions.md question 231's ruling): `tile_set_b` is new --
    # `layers_stream_check.mjs`'s own GlobeLayer probe (module docstring) uses it to
    # construct "two real sets, later wins per tile" for real. The four positional
    # args between `MEMORY_BUDGET_BYTES` and `tile_set_b`'s own manifest hash are
    # passed EXPLICITLY, at exactly this script's own pre-round-6 DEFAULT values
    # (maxLevel 2, tileBytes 262144 == IMAGERY_TILE_BYTES, maxConcurrentLoads 2 ==
    # STREAM_MAX_CONCURRENT_LOADS, dwellRoundTrips 1 == DWELL_ROUND_TRIPS_PER_
    # POSITION -- see layers_stream_check.mjs's own module docstring for each), so
    # every OTHER measurement this fixture's own consumers (the eviction/cancellation
    # test explicitly among them -- this task's brief says to leave it and
    # MEMORY_BUDGET_BYTES exactly as they are) still runs byte-for-byte identically;
    # only the NEW, final positional slot is actually new.
    proc = subprocess.run(
        [
            node, str(LAYERS_STREAM_CHECK), viewer_server.origin, tile_set.manifest_sha256,
            str(FRAME_BUDGET_MS), str(MEMORY_BUDGET_BYTES),
            "2", "262144", "2", "1",
            tile_set_b.manifest_sha256,
        ],
        cwd=str(LAYERS_STREAM_CHECK.parent), capture_output=True, text=True, timeout=180,
    )
    assert proc.returncode == 0, (
        f"node {LAYERS_STREAM_CHECK.name} exited {proc.returncode}\nstdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{LAYERS_STREAM_CHECK.name} did not print valid JSON: {proc.stdout!r}\nstderr: {proc.stderr}")
    assert "error" not in data, f"{LAYERS_STREAM_CHECK.name} reported a setup error: {data.get('error')}"
    return data


# =================================================================================================
# The tests.
# =================================================================================================


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_every_frame_stayed_within_the_chosen_budget(stream_result):
    assert stream_result["everyFrameWithinBudget"] is True, (
        f"expected every one of {stream_result['frameCount']} frames to stay within "
        f"{stream_result['frameBudgetMs']}ms; the slowest was {stream_result['maxFrameMs']}ms"
    )
    assert isinstance(stream_result["maxFrameMs"], (int, float))
    assert stream_result["frameCount"] > 0


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_memory_budget_was_respected_and_the_soft_violation_branch_never_fired(stream_result):
    assert stream_result["budgetRespected"] is True, (
        f"resident byte total exceeded the budget: max observed "
        f"{stream_result['maxResidentBytesObserved']} > budget {stream_result['memoryBudgetBytes']}"
    )
    assert stream_result["maxResidentBytesObserved"] <= stream_result["memoryBudgetBytes"]
    assert stream_result["softViolationTaken"] is False, (
        "the soft-violation branch of _evictIfNeeded fired at least once (see "
        "web/js/layers/layer.js's own doc comment) -- budgetRespected would be true "
        "only because nothing fit under the budget even after evicting everything "
        "possible, not because real eviction kept the byte total under budget"
    )


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_real_bytes_really_crossed_the_gateway_and_every_one_was_etag_verified(stream_result):
    assert stream_result["tilesFetched"] > 0, "expected at least one real tile fetch across the real gateway"
    assert stream_result["etagVerifiedCount"] == stream_result["tilesFetched"], (
        f"expected every fetched tile to have its SHA-256 verified against the gateway's own ETag "
        f"(etagVerifiedCount={stream_result['etagVerifiedCount']}, tilesFetched={stream_result['tilesFetched']})"
    )
    assert stream_result["etagMismatchCount"] == 0, (
        f"expected zero ETag mismatches against the real gateway, got {stream_result['etagMismatchCount']}"
    )


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_every_http_status_seen_is_one_the_design_expects(stream_result):
    """This run's camera path only ever addresses real, existing tiles (levels 0-2,
    the whole-globe grid this task's own investigation into crates/av-jobs/src/
    scheme.rs confirmed `av-tile-fixture` populates completely) with a token this
    module minted at the tile set's own clearance -- so `200` is the only status this
    run should ever see; anything else is a real, unexpected refusal or error."""
    seen = set(stream_result["httpStatusCounts"].keys())
    assert seen <= {"200"}, f"expected only HTTP 200 in this run, saw {stream_result['httpStatusCounts']}"
    assert sum(stream_result["httpStatusCounts"].values()) == stream_result["tilesFetched"]


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_eviction_and_cancellation_both_actually_happened(stream_result):
    """Same "only a meaningful test if it had to run" reasoning as tests/test_viewer_
    globe.py::test_tile_budget_is_respected / test_tile_loading_is_cancelled_on_camera_
    move -- see this module's own docstring for exactly what a zero here would mean."""
    assert stream_result["evictedCount"] > 0, (
        "expected LayerManager to have evicted at least one resident tile over this "
        "real run; evictedCount == 0 would mean MEMORY_BUDGET_BYTES was never actually "
        "approached, so budgetRespected would be true for the wrong reason"
    )
    assert stream_result["cancelledCount"] > 0, (
        "expected the deliberate camera jump (web/js/layers_stream_check.mjs's own "
        "CAMERA_PATH, 'near-0-0-close' -> 'jump-antipodal') to have cancelled at least "
        "one real in-flight fetch(); cancelledCount == 0 would mean this test never "
        "actually exercised cancellation against real network I/O"
    )


## ============================================================================
## Round 6 (docs/open-questions.md question 231's ruling, "replace or composite" --
## docs/heavy-plan.md's round-5 status, "the one thing round 5 does NOT deliver: a
## selected tile set is streamed, not drawn"). `layers_stream_check.mjs`'s own
## `globeLayerProbe` (see that file's own module docstring for the full mechanics)
## drives a REAL `GlobeLayer` against the SAME real gateway this whole file already
## proves real bytes cross a real loopback socket for.
## ============================================================================


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_globe_layer_binds_the_selected_sets_own_texture_while_it_is_on(stream_result):
    """Question 231's ruling, half 1: "a selected tile set is drawn as an imagery
    layer that REPLACES the default imagery on the globe's tile meshes while it is
    on". `everyMeshBoundToSetAWhileOn` is asserted from the real scene graph
    (`globe.group.traverse`, mirroring tests/test_viewer_globe_layer_manager.py's own
    browser probe), reading each mesh's OWN `material.map.userData.sourceLayerId` --
    a real, per-texture property, never a global counter. An implementation that left
    `GlobeLayer` hard-wired to its own fixed `imageryLayerId` (round 5's own
    documented gap -- see docs/heavy-plan.md's round-5 status) would fail this: every
    mesh would stay bound to `'imagery'` even with the real gateway set's own tiles
    genuinely resident on the SAME manager."""
    g = stream_result["globeLayerProbe"]
    assert g["meshCountProbe"] > 0, f"the globe built no tile meshes at all: {g!r}"
    assert g["allDefaultBeforeAnyGatewaySet"] is True, (
        f"expected every mesh bound to the default 'imagery' adapter BEFORE any "
        f"gateway set was registered: {g!r}"
    )
    assert g["stepASettled"] is True and g["stepBSettled"] is True, f"a step did not settle within its tick budget: {g!r}"
    assert g["someMeshChangedProvenanceFromDefaultToA"] is True, (
        f"expected at least one mesh's own provenance to move from 'imagery' to "
        f"'gateway-a' once the real set was toggled on: {g!r}"
    )
    assert g["everyMeshBoundToSetAWhileOn"] is True, (
        f"expected EVERY currently-selected mesh to be bound to the real gateway "
        f"set's own texture while it is the only one on: {g!r}"
    )


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_toggling_the_set_off_restores_the_default_on_the_next_tick(stream_result):
    """Question 231's ruling, half 1 continued: "with the default restored when it is
    turned off". `web/js/app.js`'s own toggle handler calls `viewer.layerManager.
    removeLayer(layerId)`; this asserts the SAME real `LayerManager.removeLayer` call,
    against a real, previously-resident gateway texture, genuinely restores the
    default on the very next `GlobeLayer.update()` tick -- no page reload, no extra
    settling wait (the default's own resident payload never moved)."""
    g = stream_result["globeLayerProbe"]
    assert g["restoredToDefaultAfterToggleOff"] is True, (
        f"expected every mesh to show the default adapter's own texture again, on "
        f"the very next update() tick after removeLayer('gateway-a'): {g!r}"
    )


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_two_real_sets_the_later_one_wins_per_tile(stream_result):
    """Question 231's ruling, half 2: "two selected sets compose in list order with
    the later on top ... GlobeLayer binds a material map from whichever imagery layer
    is topmost for THAT TILE, not from a fixed id." Constructed for real, against two
    REAL manifests (`tests/heavy_stack.py`-style `tile_set`/this file's own
    `tile_set_b`, the second deliberately shallower): set B (registered after set A)
    must win on every tile its own real manifest covers, and set A must still be what
    is bound on the one tile set B's own real gateway genuinely 404s for -- never
    left blank, never wrongly shown as set B's non-existent tile."""
    g = stream_result["globeLayerProbe"]
    p = g["twoSetProbe"]
    assert "skipped" not in p, f"the two-set probe was skipped: {p!r} -- expected tile_set_b's manifest to have been passed"
    assert p["exercisedBothCases"] is True, (
        f"expected this camera position to select tiles at BOTH a level set B covers "
        f"and a level it does not (a meaningful test only if it had to run both "
        f"cases): {p!r}"
    )
    assert p["laterSetWinsWhereCovered"] is True, (
        f"expected every mesh at a level set B's own real manifest covers to be bound "
        f"to gateway-b (the later-registered set): {p!r}"
    )
    assert p["earlierSetWinsWhereLaterDoesNotCover"] is True, (
        f"expected every mesh at the one level set B's own real manifest does NOT "
        f"cover to fall back to gateway-a: {p!r}"
    )
    assert p["realHttpErrorsRecordedForUncoveredTiles"] is True, (
        f"expected a REAL TileHttpError (a genuine 404 against the real gateway) for "
        f"the level set B does not cover, not an assumption from the manifest tile "
        f"count alone: {p!r}"
    )
    assert p["restoredToSetAAfterRemovingB"] is True, (
        f"expected removeLayer('gateway-b') to restore set A (not the default) on "
        f"the tiles set B had been winning: {p!r}"
    )
    assert p["ok"] is True, f"two-set probe: {p!r}"


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_globe_meshes_never_go_textureless_once_the_default_has_loaded(stream_result):
    """This task's own required proof 4: "the globe's own two meshes keep a texture
    at all times (never a frame with material.map === null once the default has
    loaded) -- assert it across the whole run, not just at the end." `layers_stream_
    check.mjs`'s own `checkTextureInvariant` runs after EVERY tick of the entire
    GlobeLayer probe (steps A through E, including the two-set half when it runs),
    not merely before/after. The invariant is checked PER MESH KEY, monotonically --
    once a given tile's own mesh has EVER shown a texture, it must show one on every
    subsequent tick, but a mesh whose own first load has simply not resolved yet
    (`probeManager`'s own `maxConcurrentLoads: 6` admits at most 6 of
    `meshCountProbe` tiles' worth of default-imagery requests per tick, so several
    meshes legitimately still show `material.map === null` for their first few
    ticks, even after the FIRST few meshes' own texture has already resolved) is not
    a regression -- this is a real, measured distinction this task's own report
    explains, not the original global-across-all-meshes version of this check, which
    initially and wrongly flagged that ordinary startup staggering as a violation.
    `texturelessRegressionCount` is the real, counted number of times a
    previously-textured mesh went back to textureless (never assumed zero)."""
    g = stream_result["globeLayerProbe"]
    assert g["defaultHasEverLoaded"] is True, f"the default adapter's texture was never observed loaded at all: {g!r}"
    assert g["neverTexturelessOnceLoadedPerMesh"] is True, (
        f"expected a mesh that had EVER shown a texture to never regress to "
        f"material.map === null on a later tick; it happened "
        f"{g['texturelessRegressionCount']} time(s): {g!r}"
    )
    assert g["texturelessRegressionCount"] == 0


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_globe_layer_probe_harness_itself_was_console_clean(stream_result):
    """This task's own required proof 5: "Console-clean: zero errors/warnings from
    the harness." A node CLI harness has no page/DOM, so this is the direct analogue
    of tests/test_viewer_globe_layer_manager.py's own zero-page-exceptions gate:
    zero `console.warn`/`console.error` calls anywhere in this file's own run (nothing
    in it calls either today -- a call appearing at all is the signal), and zero
    unhandled promise rejections."""
    assert stream_result.get("consoleWarnings") == [], f"expected zero console.warn/error calls: {stream_result.get('consoleWarnings')}"
    assert stream_result.get("unhandledRejections") == [], f"expected zero unhandled promise rejections: {stream_result.get('unhandledRejections')}"


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_decode_modes_are_disclosed_and_this_harness_is_honestly_scoped(stream_result):
    """Manager review of this task's own report: a provenance tag alone
    ("this mesh's texture came from gateway-a") is not proof that gateway-a's own
    real pixels reached the screen -- it is equally true when every one of gateway-a's
    tiles silently decoded to the same placeholder. `decodeModeCounts` (both the
    frame-time section's own real gateway fetches and the GlobeLayer probe's own
    `globeLayerProbe.decodeModeCounts`) must be present and account for every real
    load this harness made, and -- because this whole file runs under node, which has
    no `createImageBitmap` at all (web/js/layers/gateway_imagery_layer.js's own
    module docstring measured this directly) -- every one of them is structurally
    expected to be the `'placeholder-no-createImageBitmap'` fallback, regardless of
    how real the underlying PNG bytes are (they ARE real: `av-tile-fixture`'s own
    tiler, crates/av-jobs/src/tiler.rs, encodes genuine PNGs). This is disclosed here,
    explicitly, rather than left for a reader to assume from the provenance
    assertions alone; the real-decode branch against a real PNG is proved separately,
    in a real browser -- tests/test_viewer_globe_layer_manager.py's own
    test_decode_tile_bytes_to_texture_really_decodes_a_real_png_in_a_real_browser."""
    decode_counts = stream_result.get("decodeModeCounts") or {}
    assert sum(decode_counts.values()) == stream_result["tilesFetched"], (
        f"expected every real fetch this section made to be tallied by its own decode "
        f"mode: {decode_counts!r} vs tilesFetched={stream_result['tilesFetched']}"
    )
    assert set(decode_counts.keys()) <= {"placeholder-no-createImageBitmap"}, (
        f"expected every real load in this NODE-based harness to have taken the "
        f"documented 'no createImageBitmap at all' fallback -- any OTHER mode here "
        f"would mean this assumption about node's own capabilities is stale: {decode_counts!r}"
    )

    probe_decode_counts = stream_result["globeLayerProbe"].get("decodeModeCounts") or {}
    assert sum(probe_decode_counts.values()) > 0, (
        f"expected the GlobeLayer probe's own real gateway loads to be tallied too: {probe_decode_counts!r}"
    )
    assert set(probe_decode_counts.keys()) <= {"placeholder-no-createImageBitmap"}, (
        f"same expectation, for the GlobeLayer probe's own gateway-a/gateway-b loads: {probe_decode_counts!r}"
    )


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_globe_layer_probe_report(stream_result, capsys):
    """Not a correctness assertion -- prints the GlobeLayer probe's own measured
    numbers, same "an exit code is not evidence" rule as test_stream_report below."""
    with capsys.disabled():
        print("\nGlobeLayer real-draw probe (web/js/layers_stream_check.mjs's own globeLayerProbe):")
        g = stream_result["globeLayerProbe"]
        for key, value in g.items():
            print(f"  {key}={value}")


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_stream_report(stream_result, capsys):
    """Not a correctness assertion -- prints the measured frame-time/memory/network
    numbers so `pytest -q -s` (or any CI log) carries the real values this task's own
    "an exit code is not evidence" rule requires."""
    with capsys.disabled():
        print("\nreal-gateway frame-time/memory harness (web/js/layers_stream_check.mjs):")
        for key in (
            "frameCount", "maxFrameMs", "p50FrameMs", "p95FrameMs", "frameBudgetMs", "everyFrameWithinBudget",
            "memoryBudgetBytes", "maxResidentBytesObserved", "budgetRespected", "softViolationTaken",
            "tilesFetched", "bytesFetched", "etagVerifiedCount", "etagMismatchCount",
            "cancelledCount", "evictedCount", "failedCount", "failureNames", "commitCount", "httpStatusCounts",
            "settledBeforeMaxFrames", "maxConcurrentLoads", "calibrationRoundTripMs", "dwellMsPerPosition",
            "consoleWarnings", "unhandledRejections", "decodeModeCounts",
        ):
            print(f"  {key}={stream_result.get(key)}")
