"""The Blue Marble pyramid: served, judged at start-up, visible when missing, and the plain body
(docs/open-questions.md question 264, tasks 3-4 of the heavy basemap round).

Every test here builds a tiny SYNTHETIC pyramid in ``tmp_path`` (a handful of files and a matching
manifest in the generator's format) and passes it as ``create_app(imagery_dir=...)``. Nothing reads
the real ``~/altavista-data/imagery/bluemarble`` and nothing touches the network, so the file passes
with or without the real pyramid on the host. The SHIPPED profiles (``profiles/*.yaml``) are used
unchanged, because what is proved is exactly that they point at the mount.

* serving: an installed pyramid is served at ``/imagery/bluemarble/`` with the right content type,
  revalidating (``Cache-Control: no-cache``, 304 on ``If-None-Match``), ``scenario.imagery.installed``
  is true, ``/api/health`` and one INFO log carry ``max_level``/``tile_count``/``pyramid_sha256``.
* not installed (missing directory, no/unreadable/invalid/wrong-format manifest, a listed file
  missing or the wrong size, an unsafe manifest path): ``installed`` is false, the scenario carries a
  notice naming the directory and the build command, exactly one WARNING is logged, never an INFO.
* only an imagery template that points at the pyramid is judged by it (the test fixture's is not).
* traversal (``..``, encoded ``..``, a symlink out of the directory) never reaches a file outside it.
* the plain body (``Hub.put``): Earth's missing ``/textures/<file>`` becomes the pyramid's
  ``body/earth-2048x1024.jpg`` when the pyramid is installed; GMAT's texture, when present, is kept;
  with neither the body is left as published and the notice is there.
"""
from __future__ import annotations

import hashlib
import json
import logging
import os
import stat
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from altavista import imagery_pyramid
from altavista.server import Hub, create_app
from fixture_profiles import FIXTURE_PROFILES_DIR

TILE_BYTES = {
    "0/0/0.jpg": b"\xff\xd8\xff\xe0 tile 0/0/0",
    "0/1/0.jpg": b"\xff\xd8\xff\xe0 tile 0/1/0, a little longer",
    "1/0/0.jpg": b"\xff\xd8\xff\xe0 tile 1/0/0",
    imagery_pyramid.BODY_FILE: b"\xff\xd8\xff\xe0 the whole-Earth image",
}
SHA = "ab" * 32


def build_pyramid(root: Path, *, max_level: int = 5, tile_count: int = 3) -> dict:
    """A tiny pyramid in the generator's manifest format; returns the manifest."""
    files = {}
    for rel, data in TILE_BYTES.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        files[rel] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    manifest = {
        "format": imagery_pyramid.MANIFEST_FORMAT, "scheme": "geographic-2x1", "tile_px": 256,
        "min_level": 0, "max_level": max_level, "image_format": "jpeg",
        "attribution": "synthetic", "files": files, "tile_count": tile_count,
        "total_bytes": sum(f["bytes"] for f in files.values()), "pyramid_sha256": SHA,
    }
    (root / "manifest.json").write_text(json.dumps(manifest, indent=1))
    return manifest


@pytest.fixture()
def pyramid(tmp_path):
    root = tmp_path / "bluemarble"
    build_pyramid(root)
    return root


def make_client(tmp_path: Path, imagery_dir, **kwargs) -> TestClient:
    kwargs.setdefault("texture_dir", tmp_path / "no-textures")
    return TestClient(create_app(web_dir=tmp_path, imagery_dir=imagery_dir, **kwargs))


def publish(client: TestClient, name: str = "s", bodies=None) -> dict:
    scenario = {"name": name, "spacecraft": []}
    if bodies is not None:
        scenario["bodies"] = bodies
    assert client.post("/api/scenario", json=scenario).status_code == 200
    got = client.get(f"/api/scenario/{name}")
    assert got.status_code == 200
    return got.json()


def records(caplog, level: int):
    return [r for r in caplog.records if r.name == "altavista.server" and r.levelno == level]


# ------------------------------------------------------------------------------------ installed
def test_an_installed_pyramid_is_served_with_the_right_type_and_revalidates(tmp_path, pyramid):
    client = make_client(tmp_path, pyramid)
    resp = client.get("/imagery/bluemarble/0/0/0.jpg")
    assert resp.status_code == 200
    assert resp.content == TILE_BYTES["0/0/0.jpg"]
    assert resp.headers["content-type"] == "image/jpeg"
    assert resp.headers["cache-control"] == "no-cache"  # revalidating, like the rest of web/
    again = client.get("/imagery/bluemarble/0/0/0.jpg", headers={"if-none-match": resp.headers["etag"]})
    assert again.status_code == 304
    body = client.get("/imagery/bluemarble/" + imagery_pyramid.BODY_FILE)
    assert body.status_code == 200 and body.content == TILE_BYTES[imagery_pyramid.BODY_FILE]
    assert client.get("/imagery/bluemarble/manifest.json").json()["pyramid_sha256"] == SHA
    assert client.get("/imagery/bluemarble/3/3/3.jpg").status_code == 404  # absent tile: a plain 404


@pytest.mark.parametrize("profile", ["design", "feasibility", "analysis", "execution"])
def test_every_shipped_profile_gets_installed_true_and_no_notice(tmp_path, pyramid, profile):
    client = make_client(tmp_path, pyramid, profile=profile)
    imagery = publish(client)["imagery"]
    assert imagery["installed"] is True
    assert "notice" not in imagery
    assert imagery["urlTemplate"] == "./imagery/bluemarble/{z}/{x}/{y}.jpg"
    assert imagery["maxLevel"] == 5
    assert "NASA Earth Observatory" in imagery["attribution"]


def test_an_installed_pyramid_is_logged_once_at_info_and_shown_by_health(tmp_path, pyramid, caplog):
    with caplog.at_level(logging.INFO, logger="altavista.server"):
        client = make_client(tmp_path, pyramid)
    infos = [r.getMessage() for r in records(caplog, logging.INFO)]
    installed = [m for m in infos if "Blue Marble basemap installed" in m]
    assert len(installed) == 1
    assert str(pyramid) in installed[0] and "max_level 5" in installed[0]
    assert "3 tiles" in installed[0] and SHA in installed[0]
    assert records(caplog, logging.WARNING) == []
    health = client.get("/api/health").json()["imagery"]
    assert health == {"installed": True, "directory": str(pyramid), "maxLevel": 5, "tileCount": 3,
                      "pyramidSha256": SHA}


# -------------------------------------------------------------------------------- not installed
def _break_missing_dir(root: Path):
    for p in sorted(root.rglob("*"), reverse=True):
        p.unlink() if p.is_file() else p.rmdir()
    root.rmdir()


def _break_no_manifest(root: Path):
    (root / "manifest.json").unlink()


def _break_unreadable_manifest(root: Path):
    (root / "manifest.json").chmod(0)


def _break_invalid_json(root: Path):
    (root / "manifest.json").write_text("{not json")


def _break_not_an_object(root: Path):
    (root / "manifest.json").write_text("[1, 2]")


def _break_wrong_format(root: Path):
    m = json.loads((root / "manifest.json").read_text())
    m["format"] = "something-else/9"
    (root / "manifest.json").write_text(json.dumps(m))


def _break_listed_file_missing(root: Path):
    (root / "0" / "1" / "0.jpg").unlink()


def _break_wrong_size(root: Path):
    (root / "1" / "0" / "0.jpg").write_bytes(b"truncated")


def _break_listed_file_is_a_directory(root: Path):
    (root / "0" / "0" / "0.jpg").unlink()
    (root / "0" / "0" / "0.jpg").mkdir()


def _break_unsafe_path(root: Path):
    m = json.loads((root / "manifest.json").read_text())
    m["files"]["../outside.jpg"] = {"sha256": SHA, "bytes": 1}
    (root / "manifest.json").write_text(json.dumps(m))


def _break_no_max_level(root: Path):
    m = json.loads((root / "manifest.json").read_text())
    del m["max_level"]
    (root / "manifest.json").write_text(json.dumps(m))


def _break_empty_files(root: Path):
    m = json.loads((root / "manifest.json").read_text())
    m["files"] = {}
    (root / "manifest.json").write_text(json.dumps(m))


BREAKS = [
    ("missing directory", _break_missing_dir, "does not exist"),
    ("missing manifest", _break_no_manifest, "manifest.json is missing"),
    ("unreadable manifest", _break_unreadable_manifest, "manifest.json is unreadable"),
    ("invalid json", _break_invalid_json, "not valid JSON"),
    ("manifest not an object", _break_not_an_object, "not a JSON object"),
    ("wrong format", _break_wrong_format, "something-else/9"),
    ("listed file missing", _break_listed_file_missing, "listed file 0/1/0.jpg is missing"),
    ("wrong size", _break_wrong_size, "listed file 1/0/0.jpg is 9 bytes"),
    ("listed file is a directory", _break_listed_file_is_a_directory, "not a regular file"),
    ("unsafe manifest path", _break_unsafe_path, "unsafe path"),
    ("no max_level", _break_no_max_level, "max_level"),
    ("no files", _break_empty_files, "lists no files"),
]


@pytest.mark.parametrize("label,break_it,reason", BREAKS, ids=[b[0] for b in BREAKS])
def test_a_pyramid_that_is_not_installed_is_visible_everywhere(tmp_path, pyramid, caplog, label, break_it, reason):
    if label == "unreadable manifest" and os.geteuid() == 0:
        pytest.skip("root reads a mode-0 file; this case needs an unprivileged user")
    break_it(pyramid)
    try:
        with caplog.at_level(logging.INFO, logger="altavista.server"):
            client = make_client(tmp_path, pyramid)
        imagery = publish(client)["imagery"]
        # the verdict on the scenario: installed false, and a notice a person can act on
        assert imagery["installed"] is False
        notice = imagery["notice"]
        assert str(pyramid) in notice
        assert ".venv/bin/python scripts/imagery/build_blue_marble_tiles.py --original <the original> --out " + str(pyramid) in notice
        assert reason in notice
        assert imagery["urlTemplate"] == "./imagery/bluemarble/{z}/{x}/{y}.jpg"  # still the profile's own
        # exactly one WARNING naming the directory and the build command; never the INFO "installed"
        warnings = records(caplog, logging.WARNING)
        assert len(warnings) == 1
        assert str(pyramid) in warnings[0].getMessage()
        assert "build_blue_marble_tiles.py" in warnings[0].getMessage()
        assert not [r for r in records(caplog, logging.INFO) if "installed at" in r.getMessage()]
        # /api/health says so too, with the reason
        health = client.get("/api/health").json()["imagery"]
        assert health["installed"] is False and reason in health["reason"] and "maxLevel" not in health
    finally:
        if pyramid.exists() and (pyramid / "manifest.json").exists():
            (pyramid / "manifest.json").chmod(stat.S_IRUSR | stat.S_IWUSR)


def test_a_path_that_is_a_file_is_not_a_pyramid(tmp_path, caplog):
    not_a_dir = tmp_path / "bluemarble"
    not_a_dir.write_text("hello")
    with caplog.at_level(logging.WARNING, logger="altavista.server"):
        client = make_client(tmp_path, not_a_dir)
    assert publish(client)["imagery"]["installed"] is False
    assert len(records(caplog, logging.WARNING)) == 1
    assert client.get("/imagery/bluemarble/0/0/0.jpg").status_code == 404


def test_a_missing_directory_serves_404_and_the_rest_of_the_app_still_works(tmp_path):
    client = make_client(tmp_path, tmp_path / "never-built")
    assert client.get("/imagery/bluemarble/0/0/0.jpg").status_code == 404
    assert client.get("/imagery/bluemarble/manifest.json").status_code == 404
    assert client.get("/api/health").json()["ok"] is True


def test_only_a_template_that_points_at_the_pyramid_is_judged_by_it(tmp_path):
    """The test fixture's template is not the pyramid: with no pyramid anywhere, the scenario carries
    no `installed` and no `notice` at all (they would be false statements about a source that does
    not use the pyramid)."""
    client = make_client(tmp_path, tmp_path / "never-built", profiles_dir=FIXTURE_PROFILES_DIR)
    imagery = publish(client)["imagery"]
    assert imagery["urlTemplate"] == "./fixtures/tiles/{z}/{x}/{y}.png"
    assert "installed" not in imagery and "notice" not in imagery


def test_the_judging_rule_for_a_url_template():
    uses = imagery_pyramid.template_uses_pyramid
    assert uses("./imagery/bluemarble/{z}/{x}/{y}.jpg")
    assert uses("/imagery/bluemarble/{z}/{x}/{y}.jpg")
    assert not uses("./fixtures/tiles/{z}/{x}/{y}.png")
    assert not uses("https://tiles.example.test/imagery/bluemarble/{z}/{x}/{y}.png")
    assert not uses("./imagery/bluemarble-other/{z}/{x}/{y}.jpg")
    assert not uses(None)


# ------------------------------------------------------------------------------------ the flag
def test_the_default_directory_is_under_the_home_directory_and_the_argument_overrides_it(tmp_path, monkeypatch):
    assert imagery_pyramid.default_imagery_dir() == Path.home() / "altavista-data" / "imagery" / "bluemarble"
    built = tmp_path / "home-default"
    build_pyramid(built)
    monkeypatch.setattr(imagery_pyramid, "default_imagery_dir", lambda: built)
    client = TestClient(create_app(web_dir=tmp_path, texture_dir=tmp_path / "t"))  # no imagery_dir: the default
    assert client.get("/api/health").json()["imagery"]["directory"] == str(built)
    assert client.get("/imagery/bluemarble/0/0/0.jpg").status_code == 200


def test_an_empty_imagery_dir_is_refused(tmp_path):
    with pytest.raises(ValueError):
        create_app(web_dir=tmp_path, texture_dir=tmp_path, imagery_dir="")


def test_the_cli_flag_reaches_serve_and_defaults_to_none(monkeypatch):
    import altavista.server
    from altavista.__main__ import main

    seen = {}
    monkeypatch.setattr(altavista.server, "serve", lambda **kw: seen.update(kw))
    assert main(["serve", "--imagery-dir", "/data/bluemarble"]) == 0
    assert seen["imagery_dir"] == "/data/bluemarble"
    seen.clear()
    assert main(["serve"]) == 0
    assert seen["imagery_dir"] is None


# ---------------------------------------------------------------------------------- traversal
@pytest.mark.parametrize("path", [
    "/imagery/bluemarble/../secret.txt",
    "/imagery/bluemarble/%2e%2e/secret.txt",
    "/imagery/bluemarble/..%2fsecret.txt",
    "/imagery/bluemarble/%2e%2e%2fsecret.txt",
    "/imagery/bluemarble/0/../../secret.txt",
    "/imagery/bluemarble/..\\secret.txt",
    "/imagery/bluemarble//etc/passwd",
])
def test_traversal_out_of_the_pyramid_is_refused(tmp_path, pyramid, path):
    (tmp_path / "secret.txt").write_text("SECRET-OUTSIDE-THE-PYRAMID")
    client = make_client(tmp_path, pyramid)
    resp = client.get(path)
    assert "SECRET-OUTSIDE-THE-PYRAMID" not in resp.text
    assert "root:" not in resp.text
    assert resp.status_code in (400, 404), (resp.status_code, resp.text[:200])


def test_a_symlink_that_leaves_the_pyramid_is_not_followed(tmp_path, pyramid):
    (tmp_path / "secret.txt").write_text("SECRET-OUTSIDE-THE-PYRAMID")
    (pyramid / "leak.txt").symlink_to(tmp_path / "secret.txt")
    client = make_client(tmp_path, pyramid)
    resp = client.get("/imagery/bluemarble/leak.txt")
    assert resp.status_code == 404
    assert "SECRET" not in resp.text


# ------------------------------------------------------------------------------- the plain body
EARTH_GMAT = {"name": "Earth", "texture": "/textures/ModifiedBlueMarble.jpg", "radius": 6378.137}
LUNA_GMAT = {"name": "Luna", "texture": "/textures/Moon_HermesCelestiaMotherlode.jpg", "radius": 1737.4}


def every_gmat_body() -> list:
    """One body per name altavista/bodies.py's TEXTURES knows, each naming its GMAT texture file."""
    from altavista.bodies import TEXTURES

    return [{"name": n, "texture": f"/textures/{f}", "radius": 1.0} for n, f in TEXTURES.items()]


def textures_published(got: dict) -> dict:
    return {b["name"]: b.get("texture") for b in got["bodies"]}


def test_earth_without_gmat_texture_gets_the_pyramids_body_image_and_the_others_none(tmp_path, pyramid, caplog):
    textures = tmp_path / "textures"
    textures.mkdir()  # GMAT's texture folder exists but is empty
    with caplog.at_level(logging.INFO, logger="altavista.server"):
        client = make_client(tmp_path, pyramid, texture_dir=textures)
        got = publish(client, bodies=every_gmat_body())
        publish(client, "second", bodies=every_gmat_body())
    published = textures_published(got)
    assert published.pop("Earth") == "/imagery/bluemarble/body/earth-2048x1024.jpg"
    # the substituted URL is really served
    assert client.get("/imagery/bluemarble/body/earth-2048x1024.jpg").content == TILE_BYTES[imagery_pyramid.BODY_FILE]
    # every other body: no texture at all (its flat colour), never a /textures/ URL that would 404
    assert published and all(t is None for t in published.values()), published
    assert got["imagery"]["installed"] is True
    # each body's change is logged once, however many scenarios
    earth = [r for r in records(caplog, logging.INFO) if r.getMessage().startswith("Earth texture")]
    luna = [r for r in records(caplog, logging.INFO) if r.getMessage().startswith("Luna texture")]
    assert len(earth) == 1 and str(textures) in earth[0].getMessage() and "pyramid" in earth[0].getMessage()
    assert len(luna) == 1 and "no texture" in luna[0].getMessage()


def test_an_empty_texture_dir_publishes_no_textures_url_that_would_404(tmp_path):
    """No GMAT and no pyramid: nothing under /textures/ is published for any body, so the browser
    makes no request that 404s; the scenario's imagery notice is the visible signal."""
    textures = tmp_path / "textures"
    textures.mkdir()
    client = make_client(tmp_path, tmp_path / "never-built", texture_dir=textures)
    got = publish(client, bodies=every_gmat_body())
    published = textures_published(got)
    assert all(t is None for t in published.values()), published
    for body in got["bodies"]:
        assert body["radius"] == 1.0 and body["name"]  # only the texture is dropped
    assert got["imagery"]["installed"] is False and got["imagery"]["notice"]
    # and the health report says the same thing the publish did
    assert client.get("/api/health").json()["earthTexture"]["url"] is None


def test_a_missing_texture_dir_publishes_no_textures_url_either(tmp_path):
    client = make_client(tmp_path, tmp_path / "never-built", texture_dir=tmp_path / "absent")
    assert all(t is None for t in textures_published(publish(client, bodies=every_gmat_body())).values())


def test_gmats_own_textures_are_kept_when_they_are_there(tmp_path, pyramid, caplog):
    from altavista.bodies import TEXTURES

    textures = tmp_path / "textures"
    textures.mkdir()
    for f in TEXTURES.values():
        (textures / f).write_bytes(b"gmat " + f.encode())
    with caplog.at_level(logging.INFO, logger="altavista.server"):
        client = make_client(tmp_path, pyramid, texture_dir=textures)
        got = publish(client, bodies=every_gmat_body())
    assert textures_published(got) == {n: f"/textures/{f}" for n, f in TEXTURES.items()}
    assert client.get("/textures/ModifiedBlueMarble.jpg").content == b"gmat ModifiedBlueMarble.jpg"
    assert not [r for r in records(caplog, logging.INFO) if "texture" in r.getMessage() and "is not in" in r.getMessage()]


def test_a_body_whose_texture_file_is_present_is_kept_while_an_absent_one_is_dropped(tmp_path, pyramid):
    textures = tmp_path / "textures"
    textures.mkdir()
    (textures / "Moon_HermesCelestiaMotherlode.jpg").write_bytes(b"moon")  # Luna's only
    client = make_client(tmp_path, pyramid, texture_dir=textures)
    got = textures_published(publish(client, bodies=[dict(EARTH_GMAT), dict(LUNA_GMAT), {"name": "Mars", "texture": "/textures/Mars_JPLCaltechUSGS.jpg"}]))
    assert got == {"Earth": "/imagery/bluemarble/body/earth-2048x1024.jpg",
                   "Luna": "/textures/Moon_HermesCelestiaMotherlode.jpg", "Mars": None}


def test_with_neither_texture_nor_pyramid_earth_drops_its_texture_and_the_notice_is_there(tmp_path, caplog):
    with caplog.at_level(logging.INFO, logger="altavista.server"):
        client = make_client(tmp_path, tmp_path / "never-built", texture_dir=tmp_path / "textures")
        got = publish(client, bodies=[dict(EARTH_GMAT)])
    assert got["bodies"][0]["texture"] is None  # was left as a 404 URL before this fix
    assert got["imagery"]["installed"] is False and got["imagery"]["notice"]
    assert len([r for r in records(caplog, logging.INFO) if r.getMessage().startswith("Earth texture")]) == 1


def test_a_pyramid_without_the_body_image_does_not_substitute_so_earth_drops_its_texture(tmp_path, pyramid):
    manifest = json.loads((pyramid / "manifest.json").read_text())
    del manifest["files"][imagery_pyramid.BODY_FILE]
    (pyramid / "manifest.json").write_text(json.dumps(manifest))
    client = make_client(tmp_path, pyramid, texture_dir=tmp_path / "textures")
    got = publish(client, bodies=[dict(EARTH_GMAT)])
    assert got["imagery"]["installed"] is True
    assert got["bodies"][0]["texture"] is None


@pytest.mark.parametrize("texture", [None, "https://elsewhere.test/earth.jpg", 7])
def test_only_a_slash_textures_name_is_ever_touched(tmp_path, pyramid, texture):
    client = make_client(tmp_path, pyramid, texture_dir=tmp_path / "textures")
    got = publish(client, bodies=[{"name": "Earth", "texture": texture}])
    assert got["bodies"][0]["texture"] == texture


def test_a_slash_textures_name_that_is_not_a_path_component_counts_as_absent(tmp_path):
    """`/textures/{file}` answers 400 for such a name, so it can never be served: it is dropped."""
    client = make_client(tmp_path, tmp_path / "never-built", texture_dir=tmp_path / "textures")
    got = publish(client, bodies=[{"name": "Luna", "texture": "/textures/../../etc/passwd"}])
    assert got["bodies"][0]["texture"] is None


def test_a_hub_built_directly_changes_nothing():
    hub = Hub()
    hub.put({"name": "x", "spacecraft": [], "bodies": [dict(EARTH_GMAT)]})
    assert hub.scenarios["x"]["bodies"][0]["texture"] == "/textures/ModifiedBlueMarble.jpg"
    assert "imagery" not in hub.scenarios["x"]


# ------------------------------------------------------------- /api/health tells the truth
# Defect "health textures:true for an empty texture dir": the route reported
# `textures: textures.exists()`, true for a directory with nothing in it. Root cause: it tested the
# directory, not anything servable. It now reports a regular file's presence, and separately the
# Earth texture GMAT's file name implies and which texture the server will publish.
def _health(tmp_path, texture_dir, imagery_dir=None):
    client = make_client(tmp_path, imagery_dir or tmp_path / "never-built", texture_dir=texture_dir)
    return client.get("/api/health").json()


def test_health_textures_is_false_for_an_empty_texture_dir(tmp_path):
    empty = tmp_path / "textures"
    empty.mkdir()
    h = _health(tmp_path, empty)
    assert h["textures"] is False  # was True: the directory merely existed
    assert h["textureDir"] == str(empty)
    assert h["earthTexture"] == {"file": "ModifiedBlueMarble.jpg", "present": False, "publishes": "none", "url": None}


def test_health_textures_is_false_for_a_missing_texture_dir(tmp_path):
    h = _health(tmp_path, tmp_path / "absent")
    assert h["textures"] is False
    assert h["earthTexture"]["present"] is False and h["earthTexture"]["publishes"] == "none"


def test_health_reports_a_present_gmat_earth_texture(tmp_path):
    textures = tmp_path / "textures"
    textures.mkdir()
    (textures / "ModifiedBlueMarble.jpg").write_bytes(b"gmat earth")
    h = _health(tmp_path, textures)
    assert h["textures"] is True
    assert h["earthTexture"] == {"file": "ModifiedBlueMarble.jpg", "present": True, "publishes": "gmat",
                                 "url": "/textures/ModifiedBlueMarble.jpg"}


def test_health_a_texture_dir_with_other_files_but_not_the_earth_one(tmp_path):
    textures = tmp_path / "textures"
    textures.mkdir()
    (textures / "Sun.jpg").write_bytes(b"sun")
    h = _health(tmp_path, textures)
    assert h["textures"] is True  # something is servable...
    assert h["earthTexture"]["present"] is False  # ...but not what Earth needs


def test_health_says_the_pyramids_body_image_is_published_when_gmat_has_no_earth(tmp_path, pyramid):
    textures = tmp_path / "textures"
    textures.mkdir()
    h = _health(tmp_path, textures, pyramid)
    assert h["textures"] is False
    assert h["earthTexture"] == {"file": "ModifiedBlueMarble.jpg", "present": False, "publishes": "pyramid",
                                 "url": "/imagery/bluemarble/body/earth-2048x1024.jpg"}
    assert h["imagery"]["installed"] is True and h["imagery"]["directory"] == str(pyramid)


def test_health_prefers_gmats_texture_over_the_pyramid_when_both_exist(tmp_path, pyramid):
    textures = tmp_path / "textures"
    textures.mkdir()
    (textures / "ModifiedBlueMarble.jpg").write_bytes(b"gmat earth")
    assert _health(tmp_path, textures, pyramid)["earthTexture"]["publishes"] == "gmat"


def test_health_still_answers_ok_and_the_published_prediction_matches_what_put_does(tmp_path, pyramid):
    """The report must not be a second opinion: whatever it says is published is what a real
    publish produces."""
    textures = tmp_path / "textures"
    textures.mkdir()
    client = make_client(tmp_path, pyramid, texture_dir=textures)
    health = client.get("/api/health").json()
    assert health["ok"] is True
    earth = publish(client, bodies=[dict(EARTH_GMAT)])["bodies"][0]
    assert earth["texture"] == health["earthTexture"]["url"]
