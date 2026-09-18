"""H5b-2 deliverable 2 (docs/heavy-plan.md H5, round 3): `web/js/layers/
gateway_imagery_layer.js`'s `GatewayImageryLayerAdapter` -- a `Layer` (web/js/layers/
layer.js) whose `plan()` reuses `ImageryLayerAdapter`'s own real tile
selection/screen-space-error arithmetic (never a second copy of it) and whose
`load()` performs a real `fetch` of the viewer's own same-origin `/api/tiles/*`
route, verifying the response bytes' SHA-256 against the gateway's own `ETag`.

Same "run the real code, don't port it" discipline as tests/test_viewer_layers.py:
this file shells out to a real `node` process running `web/js/
gateway_imagery_layer_check.mjs` (which drives the real adapter with an injected,
network-free `fetchImpl` stub -- see that file's own module docstring for exactly
what each case proves and what a wrong implementation would fail against) and
asserts on the JSON it prints to stdout. No docker gate, no real network of any
kind -- this file always runs. The OTHER half of deliverable 2's own proof -- a real
fetch across a real loopback socket to a real `av-tiles` gateway, with a real
gateway-issued ETag -- is `tests/test_viewer_layers_stream.py`, which IS docker-gated
and asserts `etagVerifiedCount == tilesFetched` and `etagMismatchCount == 0` against
that real traffic.

Question 154 / 199: this file starts no docker container and mutates no process
environment; `node web/js/gateway_imagery_layer_check.mjs` touches no network socket
at all (every `fetch` call in that script is answered by an in-memory stub, never a
real `fetch`).
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "gateway_imagery_layer_check.mjs"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; see test_viewer_layers.py's identical skip reason")
    return NODE


@pytest.fixture(scope="module")
def check_data() -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0, f"node {CHECK.name} exited {proc.returncode}\nstdout: {proc.stdout}\nstderr: {proc.stderr}"
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{CHECK.name} did not print valid JSON: {proc.stdout!r}")


def test_plan_never_fetches(check_data):
    """plan() must only declare demand (design constraint a) -- see
    gateway_imagery_layer_check.mjs's own module docstring for what a `plan()` that
    eagerly fetched would do to `planNeverFetches`."""
    assert check_data["planNeverFetches"] is True


def test_url_is_same_origin_relative_by_default_and_an_explicit_origin_is_honoured(check_data):
    """Question 51: the viewer never fetches any other origin -- with `origin` left
    at its own default, the URL this adapter asks to fetch must be a same-origin
    relative path, never an absolute host; an explicitly-injected origin (this
    task's own headless harness's use case) must still be honoured verbatim."""
    assert check_data["urlIsSameOriginRelativeByDefault"] is True
    assert check_data["expectedRelativeUrl"].startswith("/api/tiles/")
    assert check_data["originIsHonoured"] is True


def test_matching_etag_resolves(check_data):
    assert check_data["matchingEtagResolves"] is True


def test_mismatched_etag_rejects_with_a_typed_named_error(check_data):
    """The deliverable's own named requirement: a stub response whose ETag does not
    match the bytes it returns must reject load() with TileEtagMismatchError, by
    name -- see gateway_imagery_layer_check.mjs's own module docstring for what an
    adapter that trusted the gateway's ETag without recomputing it would do here."""
    assert check_data["mismatchedEtagRejectsWithTypedError"] is True


def test_missing_etag_also_rejects_with_the_same_typed_error(check_data):
    assert check_data["missingEtagRejectsWithTypedError"] is True


def test_a_non_2xx_status_rejects_with_a_typed_named_http_error(check_data):
    assert check_data["nonOkStatusRejectsWithTypedHttpError"] is True


def test_abort_signal_reaches_the_injected_fetch_implementation(check_data):
    """load() adds no bespoke abort bookkeeping of its own (unlike
    ImageryLayerAdapter.load()'s wrapped THREE.TextureLoader stub, whose underlying
    XHR has no abort hook) -- it must simply pass `signal` through to `fetchImpl`,
    which real `fetch` then honours natively. A load() that forgot to forward
    `signal` at all would leave `abortStub.calls[0].signal` unset, failing this."""
    assert check_data["abortRejectsAndSignalWasPassedThrough"] is True


def test_gateway_imagery_layer_report(check_data, capsys):
    with capsys.disabled():
        print("\ngateway imagery layer (web/js/gateway_imagery_layer_check.mjs):")
        for key, value in check_data.items():
            print(f"  {key}={value}")
