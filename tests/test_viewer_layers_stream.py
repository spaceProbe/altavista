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

Round 6: the five runs above pre-date question 228's real-manifest-byte-cost fix (the
non-zero `evictedCount`/`cancelledCount` shown for each of them was measured against
this file's OWN pre-round-4 byte accounting, not against `MEMORY_BUDGET_BYTES` as it
is used today) -- do not read them as evidence that `MEMORY_BUDGET_BYTES` (still
3,000,000, unchanged) currently evicts anything; see `TIGHT_MEMORY_BUDGET_BYTES`'s own
comment, below, for why it structurally does not, and for the two-run split
(`stream_result`/`stream_result_generous`) this round adds. `FRAME_BUDGET_MS` itself
is untouched by that split -- both runs measured `maxFrameMs` in the same 1.0ms-2.3ms
range this section's own five-run history already established (this task's own report
quotes the exact numbers for both budgets), so the protocol above was not re-run.

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
* ``test_memory_budget_was_respected_and_the_soft_violation_branch_never_fired``
  (round 6: reads the TIGHT run, `stream_result`/`TIGHT_MEMORY_BUDGET_BYTES` -- see
  that constant's own comment for why): a manager with no real eviction
  (`maxResidentBytesObserved` would climb past the budget) fails the first half; an
  implementation whose eviction only APPEARS to respect the budget because it never
  had anything left to evict without soft-violating (see `web/js/layers/layer.js`'s
  own `_evictIfNeeded` doc comment: a budget so tight nothing fits under it makes
  `budgetRespected` true for the wrong reason) fails the second half,
  `softViolationTaken`.
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
* ``test_eviction_and_cancellation_both_actually_happened`` (round 6: reads the TIGHT
  run -- see `TIGHT_MEMORY_BUDGET_BYTES`'s own comment; at the old, generous budget
  this was structurally unreachable, exactly round 5's carried failure): the same
  "only a meaningful test if it had to run" reasoning `tests/test_viewer_globe.py::
  test_tile_budget_is_respected`/`test_tile_loading_is_cancelled_on_camera_move`
  already use for the globe's own scheduler -- `evictedCount == 0` would mean the
  budget was never actually approached (a budget check that passes only because it
  was never exercised proves nothing); `cancelledCount == 0` would mean the harness's
  own deliberate camera jump (`layers_stream_check.mjs`'s `CAMERA_PATH`,
  'near-0-0-close' -> 'jump-antipodal') never actually caught a real in-flight
  `fetch()` still pending, which would mean this test is not exercising cancellation
  against real network I/O at all, only against the stub `web/js/layers_check.mjs`
  already covers.
* ``test_generous_budget_never_evicted_a_wanted_set_that_fits`` (round 6, new; reads
  `stream_result_generous`/`MEMORY_BUDGET_BYTES`): the converse claim -- an
  implementation that evicts on some trigger OTHER than the real budget invariant
  (e.g. a fixed resident-tile-count cap, or a spurious unconditional per-step sweep)
  would show `evictedCount > 0` here even though this run's own real cumulative
  total never approaches its budget; `budgetRespected`/`softViolationTaken` alone
  cannot catch that (both hold trivially whenever nothing is evicted), which is why
  this test asserts `evictedCount == 0` directly, not merely that the budget held.

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
# MEMORY_BUDGET_BYTES / TIGHT_MEMORY_BUDGET_BYTES -- round 6 (question 231's own
# manager review, carried into this task's brief): the comment this replaces sized
# its one budget against ImageryLayerAdapter's IMAGERY_TILE_BYTES ESTIMATE
# (262,144 bytes/tile, web/js/layers/imagery_layer.js) because that was still what
# every request was charged at the time. Round 4 (question 228,
# GatewayImageryLayerAdapter.plan(), see that file's own module docstring) changed
# that: once `fetchManifest()` resolves, every request is charged its tile's own
# REAL manifest-declared `size_bytes` instead -- and this fixture's own synthetic
# tiles (--min-level 0 --max-level 2 --tile-size 16 --synthetic-source 32x16, the
# same whole-globe plate-carree grid as before: 42 distinct tiles, 2 + 8 + 32 across
# levels 0-2) are tiny under that real accounting, not 262,144 bytes each. Measured
# directly against this exact real stack (a temporary per-position instrumented run
# this task's own report describes -- `manager.residentBytes` read back after every
# `CAMERA_PATH` position settled, generous budget, so nothing was ever evicted out
# from under the measurement): every real tile this path's camera ever asks for
# costs EXACTLY 852 bytes (16,188 bytes fetched / 19 tiles fetched over the WHOLE
# path -- see test_stream_report's own generous-run numbers, which reproduce this
# exactly on every run: the total is deterministic, only the timing of individual
# loads is host-load-dependent). Old MEMORY_BUDGET_BYTES = 3,000,000 is therefore
# 184x this path's own real 16,188-byte cumulative total -- structurally unable to
# evict anything, which is exactly round 5's carried failure (docs/heavy-plan.md's
# round-5 status; docs/teamlog/2026-09-02-team-1.md's last section, the lead's own
# words: "round 4's admission budget legitimately means a wanted set that fits the
# budget never evicts; the proof's shape, not the code, is stale").
#
# Two budgets now, not one -- two real runs (`stream_result`/`stream_result_
# generous`, below), because "eviction must actually happen" and "a wanted set that
# fits must never be evicted" are both real claims this module makes and neither
# one can be checked against the other's own run:
#
#   TIGHT_MEMORY_BUDGET_BYTES = 11,000 -- `stream_result`'s own budget, and
#   therefore what every test in this module below reads UNLESS its own comment
#   says otherwise (only `test_generous_budget_never_evicted_a_wanted_set_that_
#   fits`, at the bottom, reads the other run). Chosen the same way the old
#   3,000,000 was ("comfortably above any single LOW-demand position's own working
#   set ... and comfortably below what an UNTHROTTLED run's own cumulative total
#   reaches"), against the REAL numbers above: the path's own smallest position
#   ('far', 2 root tiles, the first entry in CAMERA_PATH below) costs exactly
#   2 * 852 = 1,704 bytes alone (also measured directly, same instrumented run) --
#   11,000 is 6.5x that, so 'far' alone always fits with room to spare, which is
#   what keeps `softViolationCount` at 0 by construction (question 228's own
#   invariant) -- the exact trap `test_memory_budget_was_respected_and_the_soft_
#   violation_branch_never_fired` (below, now reading THIS run) already guards
#   against: a budget so tight nothing fits makes `budgetRespected` true for the
#   wrong reason. 11,000 is BELOW the path's own real cumulative total on the run
#   this arithmetic was first measured against (16,188 bytes, a generous run with
#   nothing evicted) -- but that total is not a fixed number: repeat real generous
#   runs (nothing evicted, so this is a clean read of "how much of the path's own
#   up-to-20-tile-per-position demand settled before the run ended," host-load
#   dependent) ranged 9,372-16,188 bytes. Read that honestly: on the low end, the
#   raw "budget vs. cumulative total" inequality this paragraph opened with does
#   NOT hold -- 11,000 > 9,372. What keeps eviction reliable anyway, measured
#   directly rather than assumed: the TIGHT run's own resident total is not simply
#   "the generous run's cumulative total, capped" -- admitting under a real budget
#   changes which requests get admitted and in what order (`compareAdmission`,
#   web/js/layers/layer.js), and an evicted-then-still-wanted tile is re-fetched,
#   so a tight run's own `tilesFetched` tends to run HIGHER than a generous run's
#   over the identical path, not lower (this task's own repeat measurements at
#   11,000 show `tilesFetched` of 14-19 against the budget's own ~13-tile
#   capacity, `evictedCount` nonzero every time -- see "Two budgets, one number
#   apiece, tuned for real reliability, not just a real single pass" below for the
#   full repeat-run record). The real basis for trusting 11,000 is that empirical
#   record, not this one inequality against one generous run's own total.
#
# Two budgets, one number apiece, tuned for real reliability, not just a real single
# pass (this task's own binding rule, "an exit code is not evidence," extended here
# to "one passing run is not evidence either" -- a budget chosen from a single lucky
# run would be exactly as hollow as the boolean this task replaces). Measured across
# MANY repeat real runs at each candidate tight budget (same real stack, same
# CAMERA_PATH, nothing else changed) before picking 11,000:
#
#   6,000: `evictedCount` reliably nonzero, but `cancelledCount` was 0 on roughly
#   half of ~9 repeat runs. Root cause, confirmed by comparing against matched
#   generous-budget runs (which never showed a single `cancelledCount == 0` across
#   12 repeats): at this budget, admission itself is budget-gated as tightly as the
#   real concurrency cap (`STREAM_MAX_CONCURRENT_LOADS = 2`) already is, so on a
#   host-load-dependent fraction of runs, both concurrent slots' own loads settle
#   before the deliberate camera jump, leaving nothing in flight to cancel --
#   cancellation stops being reliably exercised well before eviction does.
#   9,000: better (~1 zero-cancellation run in 10) but still real, reproduced across
#   two independent batches. 12,000: the opposite failure appeared instead --
#   `evictedCount == 0` on 1 of 8 repeats, because 12,000 sits close enough to this
#   path's own real cumulative total that a run whose close-in positions happen to
#   settle slightly fewer requests (the same host-load variance) never climbs past
#   it at all. 11,000: 5 of 5 repeat runs (in addition to this task's own quoted
#   full-module runs) with both `evictedCount > 0` and `cancelledCount > 0` -- kept
#   as the number that sits in the narrower band between those two failure modes,
#   not because there is a proof no host-load run could ever land on either edge
#   (none of the numbers tried achieves that against a real, shared, over-subscribed
#   host -- see "The frame budget" above for the same caveat applied to
#   FRAME_BUDGET_MS), but because it measured reliably in this task's own repeat
#   testing where the neighbouring candidates did not. If a future run on this host
#   ever shows either assertion flake against this budget, the fix is the same
#   protocol used here -- repeat real runs at nearby candidates, record the real
#   counts, move the number -- never to add a tolerance or make either assertion
#   conditional (this task's own binding rule).
#
#   MEMORY_BUDGET_BYTES = 3,000,000 -- unchanged from round 5 -- `stream_result_
#   generous`'s own budget: still 184x the path's own real cumulative total, so
#   this run is kept specifically BECAUSE nothing it ever wants can be evicted from
#   it -- the other half of the admission-budget claim this module makes (`test_
#   generous_budget_never_evicted_a_wanted_set_that_fits`, below): a wanted set
#   that genuinely fits under the budget is never evicted from it, not merely
#   "budget big enough that this happens to be true," but asserted directly,
#   non-conditionally, against a real run.
TIGHT_MEMORY_BUDGET_BYTES = 11_000
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


def _run_layers_stream_check(node: str, viewer_server, tile_set, tile_set_b, memory_budget_bytes: int) -> dict:
    """Runs `web/js/layers_stream_check.mjs` for real, once, against the real,
    already-running `viewer_server`, at the given `memory_budget_bytes`. Shared by
    both of this module's runs (`stream_result`/`stream_result_generous`, below) --
    one place that builds the argument list, so the two runs can only ever differ in
    the ONE argument this task's own job is about.

    Round 6 (docs/open-questions.md question 231's ruling): `tile_set_b` is new --
    `layers_stream_check.mjs`'s own GlobeLayer probe (module docstring) uses it to
    construct "two real sets, later wins per tile" for real. The four positional
    args between the memory budget and `tile_set_b`'s own manifest hash are passed
    EXPLICITLY, at exactly this script's own pre-round-6 DEFAULT values (maxLevel 2,
    tileBytes 262144 == IMAGERY_TILE_BYTES, maxConcurrentLoads 2 ==
    STREAM_MAX_CONCURRENT_LOADS, dwellRoundTrips 1 == DWELL_ROUND_TRIPS_PER_
    POSITION -- see layers_stream_check.mjs's own module docstring for each), so
    every OTHER measurement either run makes still runs byte-for-byte identically to
    round 5/task 1's own runs; only the memory budget (this task's own job, see
    `TIGHT_MEMORY_BUDGET_BYTES`/`MEMORY_BUDGET_BYTES`'s own comment above) and the
    final, tile_set_b positional slot (task 1's own addition) differ between calls."""
    proc = subprocess.run(
        [
            node, str(LAYERS_STREAM_CHECK), viewer_server.origin, tile_set.manifest_sha256,
            str(FRAME_BUDGET_MS), str(memory_budget_bytes),
            "2", "262144", "2", "1",
            tile_set_b.manifest_sha256,
        ],
        cwd=str(LAYERS_STREAM_CHECK.parent), capture_output=True, text=True, timeout=180,
    )
    assert proc.returncode == 0, (
        f"node {LAYERS_STREAM_CHECK.name} (memoryBudgetBytes={memory_budget_bytes}) exited "
        f"{proc.returncode}\nstdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{LAYERS_STREAM_CHECK.name} did not print valid JSON: {proc.stdout!r}\nstderr: {proc.stderr}")
    assert "error" not in data, f"{LAYERS_STREAM_CHECK.name} reported a setup error: {data.get('error')}"
    return data


@pytest.fixture(scope="module")
def stream_result(viewer_server, tile_set, tile_set_b) -> dict:
    """The TIGHT run -- `TIGHT_MEMORY_BUDGET_BYTES` (see that constant's own comment
    above for the full arithmetic). Every test in this module reads THIS fixture
    unless its own docstring/comment says otherwise -- in particular `test_eviction_
    and_cancellation_both_actually_happened` and `test_memory_budget_was_respected_
    and_the_soft_violation_branch_never_fired`, whose own second half (eviction
    genuinely ran, and never soft-violated) is only a meaningful check against a run
    where eviction had to happen at all."""
    node = _require_node()
    return _run_layers_stream_check(node, viewer_server, tile_set, tile_set_b, TIGHT_MEMORY_BUDGET_BYTES)


@pytest.fixture(scope="module")
def stream_result_generous(viewer_server, tile_set, tile_set_b) -> dict:
    """The GENEROUS run -- `MEMORY_BUDGET_BYTES` (unchanged from round 5, see that
    constant's own comment above). The other half of the admission-budget claim:
    read ONLY by `test_generous_budget_never_evicted_a_wanted_set_that_fits`, below,
    and by `test_stream_report`, which prints both runs' numbers side by side."""
    node = _require_node()
    return _run_layers_stream_check(node, viewer_server, tile_set, tile_set_b, MEMORY_BUDGET_BYTES)


# =================================================================================================
# The tests.
#
# Round 6: which run each test below reads, and why (this task's own brief requires saying so
# explicitly, not leaving it implicit in a fixture name).
#
#   `stream_result` (TIGHT_MEMORY_BUDGET_BYTES) -- every test below UNLESS noted otherwise,
#   including every pre-round-6 test carried over unchanged from round 5/task 1. Frame timing
#   (`test_every_frame_stayed_within_the_chosen_budget`), real-bytes/ETag verification, the HTTP
#   status allowlist, and the GlobeLayer probe/decode-mode/console-clean tests are all
#   budget-agnostic in substance -- they read this run simply because it is the module's own
#   primary run. `test_memory_budget_was_respected_and_the_soft_violation_branch_never_fired` and
#   `test_eviction_and_cancellation_both_actually_happened` specifically NEED this run: both are
#   checks that are only meaningful once eviction has actually happened (see each one's own
#   docstring, added round 6). Round 7 (heavy7 task 4, question 233): the GlobeLayer probe's own
#   `probeManager` (web/js/layers_stream_check.mjs) no longer has a hardcoded 50,000,000-byte
#   budget independent of this run's own -- it now shares THIS run's real `memoryBudgetBytes` too,
#   which is exactly what makes `test_globe_meshes_never_go_textureless_once_the_default_has_
#   loaded`'s own new `defaultEvictedFromUnderLiveMeshCount` assertion (below) a meaningful check
#   against a tight budget specifically, not merely a generous one -- see that test's own
#   docstring. `test_two_real_sets_the_later_one_wins_per_tile` moved to `stream_result_generous`
#   this round for the converse reason -- see that test's own docstring for the measured budget
#   conflict this exact fix (probeManager sharing the tight run's real budget) surfaced.
#
#   `stream_result_generous` (MEMORY_BUDGET_BYTES, unchanged from round 5) -- read by
#   `test_generous_budget_never_evicted_a_wanted_set_that_fits` (the other half of the
#   admission-budget claim: a wanted set that fits is never evicted), by
#   `test_two_real_sets_the_later_one_wins_per_tile` (round 7, see that test's own docstring), and
#   by `test_stream_report` (prints both runs' numbers side by side, per this task's own brief).
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
    """Reads the TIGHT run (round 6 -- see `TIGHT_MEMORY_BUDGET_BYTES`'s own comment
    above): the second half of this test's own name (`softViolationTaken is False`)
    is only a meaningful check against a run where eviction genuinely ran at all --
    on the old, generous budget, nothing was ever resident enough to need evicting,
    so `softViolationTaken` was `False` vacuously, never because real eviction kept
    the byte total under budget while genuinely being tested. The OTHER half of this
    same claim -- a budget so tight NOTHING fits, so `budgetRespected` ends up
    `True` for the WRONG reason -- is exactly what `TIGHT_MEMORY_BUDGET_BYTES`'s own
    arithmetic (6.5x the path's smallest position's own working set) was chosen to
    avoid; this test's own `softViolationTaken is False` assertion is what would
    catch it if that arithmetic were ever wrong."""
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
    move -- see this module's own docstring for exactly what a zero here would mean.

    Reads the TIGHT run (`stream_result`, `TIGHT_MEMORY_BUDGET_BYTES`) -- round 6:
    at the old, generous `MEMORY_BUDGET_BYTES`, `evictedCount` was structurally 0 on
    every run (this path's own real cumulative resident total, 16,188 bytes, never
    gets within two orders of magnitude of a 3,000,000-byte budget) -- see
    `TIGHT_MEMORY_BUDGET_BYTES`'s own comment, above, for the real arithmetic behind
    the budget this run now uses instead."""
    assert stream_result["evictedCount"] > 0, (
        "expected LayerManager to have evicted at least one resident tile over this "
        "real run; evictedCount == 0 would mean TIGHT_MEMORY_BUDGET_BYTES was never "
        "actually approached, so budgetRespected would be true for the wrong reason"
    )
    assert stream_result["cancelledCount"] > 0, (
        "expected the deliberate camera jump (web/js/layers_stream_check.mjs's own "
        "CAMERA_PATH, 'near-0-0-close' -> 'jump-antipodal') to have cancelled at least "
        "one real in-flight fetch(); cancelledCount == 0 would mean this test never "
        "actually exercised cancellation against real network I/O"
    )


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_generous_budget_never_evicted_a_wanted_set_that_fits(stream_result_generous):
    """The other half of the admission-budget claim this module makes (round 6, this
    task's own brief, item 2): `test_eviction_and_cancellation_both_actually_
    happened` (above) proves eviction works, by forcing it to happen against a real
    run; this test proves the converse against a SEPARATE real run -- that
    `MEMORY_BUDGET_BYTES` (3,000,000, 184x this path's own real 16,188-byte
    cumulative total -- see that constant's own comment for the arithmetic) never
    evicts a wanted tile set that genuinely fits under it. Reads `stream_result_
    generous`, never `stream_result` -- the two claims ("eviction happens when the
    budget is exceeded" and "eviction does not happen when it is not") are each only
    provable against the run actually built to test it; a single run could not prove
    both without a budget change fabricating whichever half was not the one that
    just ran.

    What this would catch: an implementation that evicts on some OTHER trigger
    unrelated to the real budget (e.g. a fixed resident-count cap, or a spurious
    per-step eviction sweep unconditioned on `residentBytes + pendingBytes <=
    memoryBudgetBytes`) would show `evictedCount > 0` here even though nothing ever
    approached this run's own generous budget -- this assertion, not merely
    `budgetRespected` (already checked, generously, by construction whenever nothing
    is evicted), is what would catch that."""
    assert stream_result_generous["evictedCount"] == 0, (
        # Manager review, round 6: the budget is interpolated, so the prose must not assert
        # a fixed "fits comfortably" regardless of what it prints -- under a deliberately
        # mis-set budget this message used to claim 16,188 bytes fit under 11,000.
        f"expected LayerManager to have evicted NOTHING over a run whose whole wanted set "
        f"fits its budget -- this run's budget is "
        f"{stream_result_generous['memoryBudgetBytes']} bytes against a measured cumulative "
        f"path total of 9,372-16,188 bytes (see MEMORY_BUDGET_BYTES's own comment) -- but "
        f"evictedCount={stream_result_generous['evictedCount']}. If the budget printed here "
        f"is NOT comfortably above that range, this run was not given the generous budget, "
        f"and the failure is a misconfigured fixture rather than a real eviction defect."
    )
    assert stream_result_generous["softViolationTaken"] is False, (
        f"expected the soft-violation branch to never fire on this generous run: "
        f"{stream_result_generous!r}"
    )
    assert stream_result_generous["budgetRespected"] is True, (
        f"expected the resident byte total to stay under budget on this generous run: "
        f"{stream_result_generous!r}"
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
def test_two_real_sets_the_later_one_wins_per_tile(stream_result_generous):
    """Question 231's ruling, half 2: "two selected sets compose in list order with
    the later on top ... GlobeLayer binds a material map from whichever imagery layer
    is topmost for THAT TILE, not from a fixed id." Constructed for real, against two
    REAL manifests (`tests/heavy_stack.py`-style `tile_set`/this file's own
    `tile_set_b`, the second deliberately shallower): set B (registered after set A)
    must win on every tile its own real manifest covers, and set A must still be what
    is bound on the one tile set B's own real gateway genuinely 404s for -- never
    left blank, never wrongly shown as set B's non-existent tile.

    Round 7 (heavy7 task 4, docs/open-questions.md question 233): reads
    `stream_result_generous`, not `stream_result`, as of this round -- moved here
    deliberately, not a weakened assertion (every check below is unchanged, still
    strict, still non-conditional). Root cause, measured directly against this
    exact real stack once `globeLayerProbe`'s own `probeManager` started sharing
    this run's real budget (task 4's own fix for question 233's disclosed gap,
    replacing a hardcoded 50,000,000-byte budget that made this probe's eviction
    guard provably untested): TWO overlapping real gateway sets' own full residency
    is structurally, not tunably, too large for TIGHT_MEMORY_BUDGET_BYTES. Set A's
    own `ImageryLayerAdapter.plan()` declares demand for every one of this camera's
    PROBE_MAX_TILES tiles unconditionally, for as long as it stays registered --
    never filtered by what a later-registered set already covers -- so its own real
    residency (11 tiles x ~852 bytes/tile, the fixture's own real manifest-declared
    cost, not a fictional estimate) and set B's own real residency for the tiles ITS
    manifest covers (7 tiles x ~852 bytes) must coexist under one budget at the same
    time. Measured directly (`stream_result["globeLayerProbe"]["twoSetProbe"]["snapshotAfterStepD"]`,
    this task's own report has the full run): `residentBytes` reaches 10,576 with
    `deferredCount` in the thousands, and `laterSetWinsWhereCovered` is `False`
    outright -- not flaky, not host-load-dependent, the same every run, because
    18 real tile-residencies x ~852 bytes (~15,336 bytes) exceeds an 11,000-byte
    budget regardless of `GlobeLayer`'s own `imageryTileBytes` (that option only
    changes the DEFAULT adapter's declared cost, never a real gateway set's real,
    manifest-derived one -- not ours to retune, and not the actual constraint here
    either way). `MEMORY_BUDGET_BYTES` (the generous run, unchanged, 3,000,000 --
    184x this path's own cumulative demand) is where this specific claim can
    actually be tested, exactly like `test_generous_budget_never_evicted_a_wanted_
    set_that_fits` already reads the generous run for its own, different reason."""
    g = stream_result_generous["globeLayerProbe"]
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
    previously-textured mesh went back to textureless (never assumed zero).

    Round 6, manager review: this assertion was disclosed as NOT having teeth
    against one specific implementation bug -- clearing `mesh.material.map`
    whenever nothing is resident for that tile THIS tick would still pass it,
    because the ONE thing that would force that branch (the default adapter's own
    resident texture evicted out from under a still-selected mesh) could only
    happen if `probeManager` (this SAME probe's own LayerManager, web/js/layers_
    stream_check.mjs) ever evicted -- and `probeManager` was constructed with its
    own hardcoded `memoryBudgetBytes: 50_000_000`, entirely independent of the CLI
    `memoryBudgetBytes` argument this run was actually invoked with, so that branch
    was structurally unreachable from here no matter which budget this test module
    itself used.

    Round 7 (heavy7 task 4, docs/open-questions.md question 233): fixed.
    `probeManager` now takes this run's own real `memoryBudgetBytes` (see that
    file's own module docstring, "Round 7"), and a SEPARATE, dedicated construction
    (`globe2`/`probeManager2`, also in web/js/layers_stream_check.mjs) forces the
    exact scenario this assertion's own name is about for real: a resident default
    payload evicted by real budget pressure while its own mesh stays selected/live,
    confirmed directly (`g["evictionProbe"]`, and the new, counted
    `defaultEvictedFromUnderLiveMeshCount` field below) -- and
    `texturelessRegressionCount` stays 0 even so, because the real, unmodified
    `GlobeLayer.update()` genuinely never clears a mesh's texture when nothing is
    currently resident for it. Perturbed directly (this task's own report has the
    quoted failure): temporarily making that texture-selection loop clear
    `mesh.material.map` whenever nothing is resident pushed
    `texturelessRegressionCount` to a nonzero count and failed this exact
    assertion, then temporarily reverting `probeManager2`'s own budget back to a
    hardcoded 50,000,000 pushed `defaultEvictedFromUnderLiveMeshCount` back to 0 --
    proof that the new counter is load-bearing on the budget fix itself, not on
    something else. Both perturbations were reverted before this diff was
    finalized."""
    g = stream_result["globeLayerProbe"]
    assert g["defaultHasEverLoaded"] is True, f"the default adapter's texture was never observed loaded at all: {g!r}"
    assert g["neverTexturelessOnceLoadedPerMesh"] is True, (
        f"expected a mesh that had EVER shown a texture to never regress to "
        f"material.map === null on a later tick; it happened "
        f"{g['texturelessRegressionCount']} time(s): {g!r}"
    )
    assert g["texturelessRegressionCount"] == 0
    # Round 7 (heavy7 task 4): the new, counted fact that makes the assertion above
    # meaningful rather than vacuous -- see `evictionProbe` in web/js/layers_stream_
    # check.mjs's own globeLayerProbe for the full construction (a direct
    # LayerManager.update() call that makes one already-resident, currently-selected
    # tile genuinely unwanted for one real step, so LayerManager's own real eviction
    # machinery evicts it while GlobeLayer's own mesh for that tile is left
    # completely untouched -- still live).
    assert g["defaultEvictedFromUnderLiveMeshCount"] > 0, (
        f"expected at least one real default-imagery payload to have been evicted "
        f"while its own mesh was still live -- the exact situation "
        f"neverTexturelessOnceLoadedPerMesh claims to guard against; a 0 here would "
        f"mean this run never actually exercised that branch, so the assertion "
        f"above would be passing for the wrong reason: {g['evictionProbe']!r}"
    )
    ep = g["evictionProbe"]
    assert ep.get("targetMeshLiveAtEviction") is True, (
        f"expected the evicted default payload's own mesh to still be selected/live "
        f"at the moment of eviction (never evicted after its mesh was already "
        f"disposed, which would prove nothing about this assertion): {ep!r}"
    )
    assert ep.get("meshKeptStaleTextureWhileGenuinelyUnsupplied") is True, (
        f"expected the mesh to still show a texture (its own stale, pre-eviction "
        f"one) at the exact real tick where neither the default nor any gateway "
        f"layer had a resident payload for it -- the literal branch "
        f"neverTexturelessOnceLoadedPerMesh is supposed to guard: {ep!r}"
    )


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
def test_stream_report(stream_result, stream_result_generous, capsys):
    """Not a correctness assertion -- prints the measured frame-time/memory/network
    numbers for BOTH real runs (round 6: `stream_result`, the TIGHT run most of this
    module's tests read, and `stream_result_generous`, read only by `test_generous_
    budget_never_evicted_a_wanted_set_that_fits`) so `pytest -q -s` (or any CI log)
    carries the real values this task's own "an exit code is not evidence" rule
    requires -- for both budgets, not just the one most tests happen to use."""
    keys = (
        "frameCount", "maxFrameMs", "p50FrameMs", "p95FrameMs", "frameBudgetMs", "everyFrameWithinBudget",
        "memoryBudgetBytes", "maxResidentBytesObserved", "budgetRespected", "softViolationTaken",
        "tilesFetched", "bytesFetched", "etagVerifiedCount", "etagMismatchCount",
        "cancelledCount", "evictedCount", "failedCount", "failureNames", "commitCount", "httpStatusCounts",
        "settledBeforeMaxFrames", "maxConcurrentLoads", "calibrationRoundTripMs", "dwellMsPerPosition",
        "consoleWarnings", "unhandledRejections", "decodeModeCounts",
    )
    with capsys.disabled():
        print(f"\nreal-gateway frame-time/memory harness (web/js/layers_stream_check.mjs) -- TIGHT run (memoryBudgetBytes={TIGHT_MEMORY_BUDGET_BYTES}):")
        for key in keys:
            print(f"  {key}={stream_result.get(key)}")
        print(f"\nreal-gateway frame-time/memory harness (web/js/layers_stream_check.mjs) -- GENEROUS run (memoryBudgetBytes={MEMORY_BUDGET_BYTES}):")
        for key in keys:
            print(f"  {key}={stream_result_generous.get(key)}")
