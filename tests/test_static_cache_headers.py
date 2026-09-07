"""Question 139 / M21.1: after M20 landed, the lead's browser loaded a fresh ``app.js``
against a *cached* ``cdm_run.js`` and the viewer died at module import ("does not provide an
export named `frameOptionLabel`") -- the served files were correct, the browser's heuristic
cache was not. ``altavista.server`` mounted ``web/`` with no ``Cache-Control`` header at all, so
browsers were free to keep old modules past a deploy. The lead's ruling (``docs/open-
questions.md`` #139): every ``web/`` response -- the mounted static files *and* the separate
``/`` -> ``FileResponse(index.html)`` route -- must send ``Cache-Control: no-cache`` with an
ETag, so every load revalidates and an unchanged file still comes back as a 304.

Each test below is written to fail against the pre-M21.1 server: mounting a plain
``fastapi.staticfiles.StaticFiles`` (no ``Cache-Control`` at all, so the browser falls back to
heuristic freshness) and serving ``/`` via a bare ``FileResponse(web / "index.html")`` (no
``Cache-Control``, and no conditional-request handling at all -- a bare ``FileResponse`` built
without a ``stat_result`` never inspects ``If-None-Match``, so it always answers 200).
"""
from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from altavista.server import create_app


@pytest.fixture()
def client(tmp_path):
    web_dir = tmp_path / "web"
    (web_dir / "js").mkdir(parents=True)
    (web_dir / "index.html").write_text("<!doctype html><html><body>viewer</body></html>\n")
    (web_dir / "js" / "app.js").write_text("export const frameOptionLabel = 'ok';\n")
    texture_dir = tmp_path / "textures"
    texture_dir.mkdir()
    app = create_app(texture_dir=texture_dir, web_dir=web_dir)
    return TestClient(app)


def test_js_module_response_has_revalidating_cache_control_and_etag(client):
    """Fails against today's server: StaticFiles is mounted with no Cache-Control header at
    all, so ``resp.headers.get("cache-control")`` is ``None``."""
    resp = client.get("/js/app.js")
    assert resp.status_code == 200
    cache_control = resp.headers.get("cache-control")
    assert cache_control is not None, "web/ asset response is missing Cache-Control entirely"
    assert "no-cache" in cache_control
    # no-cache means "revalidate before use", not "don't store" -- must not regress to no-store.
    assert "no-store" not in cache_control
    assert resp.headers.get("etag"), "web/ asset response is missing an ETag"


def test_js_module_revalidation_returns_304_for_matching_etag(client):
    """Fails against a naive fix that only sets Cache-Control on 200 responses (e.g. a
    middleware that stamps the header after StaticFiles has already decided the status code,
    or one that only patches the FileResponse branch of file_response and not the
    NotModifiedResponse branch): the 304 branch would then come back without Cache-Control."""
    first = client.get("/js/app.js")
    etag = first.headers["etag"]
    second = client.get("/js/app.js", headers={"If-None-Match": etag})
    assert second.status_code == 304
    cache_control = second.headers.get("cache-control")
    assert cache_control is not None, "304 response is missing Cache-Control"
    assert "no-cache" in cache_control


def test_index_html_has_revalidating_cache_control_and_etag(client):
    """Fails against today's ``/`` handler, ``FileResponse(web / "index.html")``: no
    Cache-Control header is ever set on that response."""
    resp = client.get("/")
    assert resp.status_code == 200
    cache_control = resp.headers.get("cache-control")
    assert cache_control is not None, "index.html response is missing Cache-Control entirely"
    assert "no-cache" in cache_control
    assert "no-store" not in cache_control
    assert resp.headers.get("etag"), "index.html response is missing an ETag"


def test_index_html_revalidation_returns_304_for_matching_etag(client):
    """Fails against today's ``/`` handler: a bare ``FileResponse`` built without a
    ``stat_result`` never looks at ``If-None-Match`` at all, so it always answers 200 with the
    full body instead of 304 -- this is exactly the "no conditional-request handling on '/'"
    gap the brief calls out."""
    first = client.get("/")
    etag = first.headers["etag"]
    second = client.get("/", headers={"If-None-Match": etag})
    assert second.status_code == 304
    cache_control = second.headers.get("cache-control")
    assert cache_control is not None, "304 response for index.html is missing Cache-Control"
    assert "no-cache" in cache_control
