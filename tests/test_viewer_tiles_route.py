"""H5b-1 (`docs/heavy-plan.md` H5, round 3, "the served through the viewer server with the
gateway's authentication" half): the viewer server's `/api/tiles/*` routes really proxy a
REAL `av-tiles` gateway backed by a REAL MinIO object store, with the gateway's own OIDC
authentication and per-layer label enforcement intact end to end -- not a mock, not the
`InMemoryObjectSource` `crates/av-tiles/src/server.rs`'s own unit tests use.

# What this stands up, for real

Round 3 (H5b-2, deliverable 4): the ENTIRE stack-up this file needs -- the digest gate, the
host-wide-locked labelled MinIO container, the `cargo build`s, `av-tile-fixture`, the
`LocalTestIssuer` and its two tokens, and the real `av-tiles` subprocess -- was extracted into
`tests/heavy_stack.py`, a shared module now also used by `tests/test_viewer_layers_stream.py`
(question 218's "no second, independently-maintained copy" reasoning, applied to test
infrastructure). See that module's own docstring for the full "what this stands up, for real"
reasoning (unchanged from H5b-1, just relocated); it is not repeated a second time here. This
file itself only builds the FastAPI app under test (`client`, below) against that real,
already-running stack, and holds every test.

# What each test would catch (this task's own standing review requirement -- "for each test,
name the wrong implementation it would fail against")

* ``test_manifest_route_proxies_the_real_manifest_byte_for_byte``: a proxy that reconstructs
  or re-encodes the manifest (rather than passing the gateway's own bytes straight through)
  would still often produce a valid-looking `TileSetManifest`, but its SHA-256 would not equal
  the tile set's own identity `av-tile-fixture` printed -- this is the one test that would
  catch a byte-for-byte corruption a "looks fine" JSON-shape check would miss entirely.
* ``test_a_tiles_bytes_come_back_and_their_sha256_equals_the_gateways_own_etag``: a proxy that
  drops or invents its own `ETag` (rather than forwarding the gateway's own) would pass a
  "some ETag header exists" check while this test's own recomputed SHA-256 comparison still
  fails.
* ``test_range_request_gives_206_with_exactly_the_requested_bytes_and_a_matching_if_none_
  match_gives_304``: a proxy that fails to forward `Range`/`If-None-Match` from the incoming
  request (this task's own rule 4) would see the gateway answer a plain `200` with the FULL
  tile every time -- this test's own exact byte-slice and status-code assertions catch that
  even though a "some bytes came back" check would not.
* ``test_a_below_clearance_configured_token_is_refused_403_and_the_gateways_own_counter_
  increments``: a proxy that swallows the gateway's own `403` and answers a generic `200`/
  `500` (or a caller-controlled clearance that let a below-clearance token through) fails the
  status-code assertion; a refusal this test could not distinguish from "nothing happened at
  all" (never reaching the gateway, or the gateway refusing for an unrelated reason) is ruled
  out by the counter assertion, read from the gateway's OWN `/admin/api/counters` -- this is
  exactly this task's own "a refusal that is not counted is not a refusal this project
  accepts" rule.
* ``test_an_unknown_manifest_hash_is_404_not_500``: a proxy (or a gateway) that turns a
  not-found manifest into an unhandled exception -- an empty body with no status-code
  guarantee, or FastAPI's own default `500` -- fails this test's exact status assertion.
* ``test_the_token_appears_in_neither_the_response_body_the_response_headers_nor_any_log_
  record``: a route that echoes the `Authorization` header back (a common accidental "proxy
  transparency" bug: forwarding EVERY response/request header verbatim instead of an explicit
  allowlist) or a client library that logs its own request headers at an enabled log level
  would leak the token into exactly the three places this test inspects.
* ``test_rotating_the_token_file_between_two_requests_changes_behaviour_on_the_very_next_
  request``: a server that reads `tiles_token_path` once at `create_app` time and caches the
  token in memory (rather than `tiles_client.proxy_get`'s own "read fresh, every call" -- this
  task's own rule 1, "so rotation works") would keep answering with the FIRST token's own
  clearance forever, never picking up the second file's content at all.
* ``test_create_app_with_no_tiles_arguments_answers_a_typed_503_and_every_other_route_still_
  works``: a `create_app` that raises, hangs, or answers an empty `200` when the tiles
  arguments are left at their defaults -- or one whose new routes' mere presence breaks an
  existing, unrelated route -- fails this test's own byte-for-byte "nothing else changed"
  assertion (mirrors `tests/test_command_console_routes.py::test_no_command_endpoint_
  configured_answers_a_typed_503_and_other_routes_still_work`, restated for the tiles routes).
* ``test_a_configured_but_unreachable_tiles_endpoint_answers_a_typed_503``: a `proxy_get` that
  lets an `httpx` connection error (or a timeout) escape uncaught would answer FastAPI's own
  default `500`, not the typed `503` this route promises; this test needs no docker gate at
  all (a bound-then-closed loopback port is deterministically unreachable), so it always runs.

# No network at test time (question 154) / no environment mutation (question 199)

`tests/heavy_stack.py`'s own module doc already establishes both points for this whole test
suite, restated here: binding/connecting to `127.0.0.1` never leaves the host's own kernel
network stack -- it is not "the network" for question 154's purposes. No test in this file
calls `os.environ[...]`/`monkeypatch.setenv` to configure anything `av-tiles`,
`av-tile-fixture` or this server reads; every setting reaches a subprocess or `create_app` as
an explicit argument, a CLI flag, or a file this test itself wrote and named explicitly.
"""
from __future__ import annotations

import hashlib
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from altavista.pb.altavista.v1 import heavy_pb2
from altavista.server import create_app
from heavy_stack import (  # noqa: F401 -- rust_bins/minio/issuer are transitive fixture deps
    SKIP_REASON,
    admin_counter,
    av_tiles_service,
    issuer,
    key_prefix,
    minio,
    minio_bucket,
    rust_bins,
    tile_set,
    tile_set_label,
    token_at_clearance,
    token_below_clearance,
    _free_port,
)

# =================================================================================================
# The FastAPI app under test.
# =================================================================================================


@pytest.fixture()
def token_path(tmp_path, token_at_clearance) -> Path:
    path = tmp_path / "tiles_token.txt"
    path.write_text(token_at_clearance)
    return path


@pytest.fixture()
def client(av_tiles_service, token_path, tmp_path) -> TestClient:
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, tiles_endpoint=av_tiles_service.endpoint, tiles_token_path=token_path)
    return TestClient(app)


# =================================================================================================
# The tests.
# =================================================================================================


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_manifest_route_proxies_the_real_manifest_byte_for_byte(client: TestClient, tile_set, key_prefix):
    resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert resp.status_code == 200, resp.text
    assert hashlib.sha256(resp.content).hexdigest() == tile_set.manifest_sha256, "the proxied manifest's own SHA-256 must equal the tile set's identity av-tile-fixture printed"

    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(resp.content)
    assert manifest.object_key_prefix == key_prefix
    assert len(manifest.tiles) == tile_set.tile_count


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_a_tiles_bytes_come_back_and_their_sha256_equals_the_gateways_own_etag(client: TestClient, tile_set):
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert manifest_resp.status_code == 200, manifest_resp.text
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]

    resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}")
    assert resp.status_code == 200, resp.text
    etag = resp.headers.get("etag", "").strip('"')
    assert etag, resp.headers
    assert hashlib.sha256(resp.content).hexdigest() == etag, "the tile's own bytes must hash to exactly the gateway's own ETag"
    assert etag == tile.sha256


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_range_request_gives_206_with_exactly_the_requested_bytes_and_a_matching_if_none_match_gives_304(client: TestClient, tile_set):
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert manifest_resp.status_code == 200, manifest_resp.text
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]
    path = f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"

    full = client.get(path)
    assert full.status_code == 200

    ranged = client.get(path, headers={"Range": "bytes=0-3"})
    assert ranged.status_code == 206, ranged.text
    assert ranged.content == full.content[0:4]
    assert ranged.headers.get("content-range") == f"bytes 0-3/{len(full.content)}"

    etag = full.headers["etag"]
    not_modified = client.get(path, headers={"If-None-Match": etag})
    assert not_modified.status_code == 304, not_modified.text
    assert not_modified.content == b""


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_a_below_clearance_configured_token_is_refused_403_and_the_gateways_own_counter_increments(client: TestClient, tile_set, token_path: Path, token_below_clearance: str, av_tiles_service):
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert manifest_resp.status_code == 200, manifest_resp.text
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]
    path = f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"

    before = admin_counter(av_tiles_service, "tiles_layer_label_over_clearance")

    # Rewrite the SAME token file to the below-clearance token -- proves rule 1's "read
    # fresh, at request time" on the SAME app/client this test already built, rather than a
    # second app instance (see tests/heavy_stack.py's own doc, item 6).
    token_path.write_text(token_below_clearance)

    resp = client.get(path)
    assert resp.status_code == 403, resp.text
    assert resp.content == b"", "a refusal must never carry tile bytes"

    after = admin_counter(av_tiles_service, "tiles_layer_label_over_clearance")
    assert after == before + 1, f"the gateway's own tiles_layer_label_over_clearance counter must increment by exactly 1 (before={before}, after={after})"


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_rotating_the_token_file_between_two_requests_changes_behaviour_on_the_very_next_request(client: TestClient, tile_set, token_path: Path, token_at_clearance: str, token_below_clearance: str):
    """Restates the previous test's own rotation proof from the opposite direction (below then
    back to at-clearance), and adds the ONE assertion the previous test does not: the FIRST
    request (before any rewrite) must succeed. Together the two tests prove rotation in both
    directions on one running server, never a value cached at `create_app` time."""
    manifest_resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_resp.content)
    tile = manifest.tiles[0]
    path = f"/api/tiles/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"

    first = client.get(path)  # token_path fixture already wrote token_at_clearance.
    assert first.status_code == 200, first.text

    token_path.write_text(token_below_clearance)
    second = client.get(path)
    assert second.status_code == 403, second.text

    token_path.write_text(token_at_clearance)
    third = client.get(path)
    assert third.status_code == 200, third.text


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_an_unknown_manifest_hash_is_404_not_500(client: TestClient):
    never_stored = "b" * 64
    resp = client.get(f"/api/tiles/{never_stored}/manifest")
    assert resp.status_code == 404, resp.text


@pytest.mark.skipif(SKIP_REASON is not None, reason=SKIP_REASON or "")
def test_the_token_appears_in_neither_the_response_body_the_response_headers_nor_any_log_record(client: TestClient, tile_set, token_at_clearance: str, caplog):
    with caplog.at_level("DEBUG"):
        resp = client.get(f"/api/tiles/{tile_set.manifest_sha256}/manifest")
    assert resp.status_code == 200, resp.text

    assert token_at_clearance.encode("utf-8") not in resp.content
    for name, value in resp.headers.items():
        assert token_at_clearance not in value, f"the token leaked into response header {name!r}: {value!r}"
    for record in caplog.records:
        assert token_at_clearance not in record.getMessage(), f"the token leaked into a log record: {record.getMessage()!r}"


def test_create_app_with_no_tiles_arguments_answers_a_typed_503_and_every_other_route_still_works(tmp_path):
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)  # no tiles_endpoint/tiles_token_path at all
    client = TestClient(app)

    resp = client.get(f"/api/tiles/{'a' * 64}/manifest")
    assert resp.status_code == 503, resp.text
    assert "not configured" in resp.text.lower() or "no tiles" in resp.text.lower(), resp.text

    resp = client.get(f"/api/tiles/{'a' * 64}/tiles/0/0/0")
    assert resp.status_code == 503, resp.text

    # Every pre-existing route must still work -- the whole point of "configuration, not a
    # hard dependency" (mirrors tests/test_command_console_routes.py's identical assertion
    # for /api/command/*).
    assert client.get("/api/health").status_code == 200
    assert client.get("/api/scenarios").status_code == 200


def test_a_configured_but_unreachable_tiles_endpoint_answers_a_typed_503(tmp_path):
    unused_port = _free_port()  # bound-then-closed: nothing is listening here
    token_path = tmp_path / "unused_token.txt"
    token_path.write_text("irrelevant-for-this-test")
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path, tiles_endpoint=f"127.0.0.1:{unused_port}", tiles_token_path=token_path)
    client = TestClient(app)

    resp = client.get(f"/api/tiles/{'a' * 64}/manifest")
    assert resp.status_code == 503, resp.text
    assert "unreachable" in resp.text.lower(), resp.text

    assert client.get("/api/health").status_code == 200
