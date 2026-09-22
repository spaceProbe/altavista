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


def test_byte_cost_comes_from_the_manifest_when_one_is_available(check_data):
    """Round 4 (question 228's own round-3-defect-5 follow-up): before
    `fetchManifest()` resolves, `byteCost` must be the constructor's own declared
    fallback estimate, tagged `byteCostSource: 'fallback-estimate'`. After it
    resolves against a real (hand-encoded, byte-verified against Python's own
    `google.protobuf` encoder -- see gateway_imagery_layer_check.mjs's own module
    docstring) `TileSetManifest`, a request for a tile the manifest DOES list must
    carry that tile's own real `size_bytes`, tagged `byteCostSource: 'manifest'` -- an
    implementation that kept charging the fixed estimate after a manifest was fetched
    would fail `afterFetchUsesManifestByteCost`. A request for a tile the manifest
    does NOT list must still fall back to the estimate rather than throw or charge
    `undefined`/`0` -- `unlistedTileFallsBackToEstimate`.
    """
    p = check_data["manifestByteCostProbe"]
    assert p["beforeFetchIsFallback"] is True
    assert p["manifestFetchedExpectedUrl"] is True, (
        "expected fetchManifest() to GET the same-origin relative "
        "/api/tiles/<manifestSha256>/manifest route (question 51: never a second "
        "origin) -- see gateway_imagery_layer.js's own fetchManifest()"
    )
    assert p["manifestTileCount"] == 2
    assert p["afterFetchUsesManifestByteCost"] is True, (
        "expected byteCost to become the manifest's own real per-tile size_bytes "
        "once fetchManifest() resolved, not the constructor's fixed estimate"
    )
    assert p["unlistedTileFallsBackToEstimate"] is True, (
        "expected a request for a tile the manifest does not list to fall back to "
        "the declared estimate, explicitly tagged, never to throw or silently charge "
        "an undefined/zero byte cost"
    )
    assert p["ok"] is True


def test_load_resolves_with_a_texture_shaped_payload(check_data):
    """Round 6 (docs/open-questions.md question 231's ruling, "replace or
    composite"): `load()` used to resolve with `{kind, tile, url, sha256, bytes}` --
    raw, verified bytes a real `THREE.Material.map`/`THREE.WebGLRenderer` would
    reject outright. It must now resolve with something texture-shaped
    (`isTexture === true`, a real `.dispose()`) -- see
    gateway_imagery_layer_check.mjs's own module docstring, `textureShapedPayload`,
    for exactly what an implementation that still returned the old raw object, or
    that threw instead of using the documented fallback when `createImageBitmap` is
    simply absent (node has none -- an environment limitation, not a data problem),
    would fail here."""
    assert check_data["textureShapedPayload"] is True


def test_the_resolved_texture_is_tagged_with_its_own_source_layer_id(check_data):
    """Question 231's ruling: "GlobeLayer binds a material map from whichever
    imagery layer is topmost for that tile" needs a REAL, per-texture property
    tracing a bound texture back to the layer that produced it -- tagged AT THE
    SOURCE (this adapter's own `load()`), never inferred from which
    `LayerManager` slot a payload happened to be stored under."""
    assert check_data["provenanceTagged"] is True


def test_the_verified_wire_bytes_are_still_reachable_off_the_texture(check_data):
    """web/js/layers_stream_check.mjs still needs the real, ETag-verified wire bytes
    for its own second, synchronous SHA-256 check and real PNG-header parse -- the
    SHA-256/ETag verification this adapter performs must not be weakened by this
    task's own texture-conversion seam, only relocated (`payload.bytes` ->
    `payload.userData.bytes`, byte-for-byte identical)."""
    assert check_data["verifiedBytesStillReachable"] is True


def test_release_disposes_the_exact_texture_load_created(check_data):
    """`release(key)` -- the Layer interface's own eviction/unregistration hook
    (web/js/layers/layer.js's module docstring) -- must free the real GPU resource
    this adapter's own `load()` now creates (round 6), and must do so exactly once
    per load, not on every subsequent call for the same, already-released key (a
    `release()` that looked up the wrong key, or that never disposed anything at
    all, would fail `releaseDisposesTheRealTexture`; one that disposed again on a
    repeat call for an already-released key would fail
    `releaseIsNoopOnceAlreadyReleased`)."""
    assert check_data["releaseDisposesTheRealTexture"] is True
    assert check_data["releaseIsNoopOnceAlreadyReleased"] is True


def test_the_real_decode_branch_and_a_genuine_decode_failure_are_both_reachable(check_data):
    """node has no `createImageBitmap` (measured directly in
    gateway_imagery_layer_check.mjs's own module docstring), so every OTHER case in
    this file only ever exercises `decodeTileBytesToTexture`'s "API absent" fallback
    branch. This proves the OTHER TWO branches' own logic under node by temporarily
    stubbing the global function node does not have: a resolving stub must produce a
    real `THREE.Texture` tagged `decodeMode: 'createImageBitmap'`; a throwing stub
    (bytes that pass SHA-256/ETag verification but are not a valid image) must NOT
    reject `load()` -- it must still resolve, with the same placeholder texture,
    tagged `decodeMode: 'createImageBitmap-failed'` and carrying the real decode
    error on `userData.decodeError`, distinct from the "API absent" tag, and still
    carrying this adapter's own provenance tag. An earlier version of this adapter
    rejected on a genuine decode failure instead, which regressed
    tests/test_viewer_layers_panel.py's own real-browser proof (that fixture's tile
    bytes are exactly this case: tagged `image/png` but not a real one) -- this case
    is what pins the fix. This does NOT prove a real browser's `createImageBitmap`
    correctly decodes a real PNG this codebase's own gateway serves -- that piece is
    proved separately, in a real headless-Chrome check (see this task's own report
    for exactly where)."""
    d = check_data["decodeModeSwitch"]
    assert d["realDecodePathProducesRealTexture"] is True
    assert d["realDecodeFailureStillResolvesWithTaggedFallback"] is True
    assert d["globalRestoredAfterStubbing"] is True, (
        "the temporary createImageBitmap stub must be restored (or removed, if node "
        "never had one) after this probe -- a real global left mutated would leak "
        "into whatever this node process runs next"
    )
    assert d["ok"] is True


def test_gateway_imagery_layer_report(check_data, capsys):
    with capsys.disabled():
        print("\ngateway imagery layer (web/js/gateway_imagery_layer_check.mjs):")
        for key, value in check_data.items():
            print(f"  {key}={value}")
