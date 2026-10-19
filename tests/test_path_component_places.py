"""Question 254 item 1, the Python places: an id, name or request segment that becomes a path
component is validated as one (``altavista.path_id``) before any path is built.

- ``GET /textures/{file}`` (``altavista.server``): the segment arrives decoded by the ASGI server,
  so ``%2e%2e`` and ``..%2f`` are ``..`` and ``../`` by the time the route sees them. Refused with
  HTTP 400; a name that is a valid component but absent is still 404; a real texture is still 200.
- ``altavista.profile.load_imagery_config(profile_id)``: ``<profiles_dir>/<profile_id>.yaml``;
  refused as ``ProfileError``. ``../x`` would have loaded ``<profiles_dir>/../x.yaml``.
- ``altavista.scenario.prepare_script``: a spacecraft name becomes ``<report_dir>/<name>.rpt``;
  refused as ``ValueError``.

Every traversal check also snapshots the tree around the served/read directory and asserts it did
not change and that the traversal target is neither served nor written.
"""
import pytest
from fastapi.testclient import TestClient

from altavista import profile as profile_loader
from altavista.model import Frame
from altavista.scenario import prepare_script
from altavista.server import create_app

SECRET = "TOP-SECRET-OUTSIDE-THE-ROOT"


def _tree(root):
    return {str(p.relative_to(root)): (p.stat().st_size if p.is_file() else "dir") for p in sorted(root.rglob("*"))}


@pytest.fixture
def texture_world(tmp_path):
    textures = tmp_path / "root" / "textures"
    textures.mkdir(parents=True)
    (textures / "Earth.jpg").write_bytes(b"jpeg-bytes")
    (tmp_path / "root" / "secret.txt").write_text(SECRET)  # a sibling of the texture directory
    (tmp_path / "x.yaml").write_text(SECRET)
    web = tmp_path / "web"
    web.mkdir()
    (web / "index.html").write_text("<html></html>")
    app = create_app(texture_dir=textures, web_dir=web)
    return tmp_path, TestClient(app)


def test_a_real_texture_is_still_served_and_a_missing_valid_name_is_404(texture_world):
    _, client = texture_world
    ok = client.get("/textures/Earth.jpg")
    assert ok.status_code == 200 and ok.content == b"jpeg-bytes"
    assert client.get("/textures/Absent.jpg").status_code == 404


@pytest.mark.parametrize("segment", [
    "%2e%2e",              # ".." once the server decodes it (a literal ".." is normalized away by the HTTP client)
    "%2e",                 # "."
    "%2e%2e%5csecret.txt",  # "..\secret.txt"
    "a%5cb",               # "a\b"
    "%00",                 # NUL
    "Earth.jpg%00.png",
    "%20",
    "%2e%2e%2e",           # "..." (a leading dot)
    ".hidden",
    "caf%C3%A9",
])
def test_texture_segments_that_are_not_path_components_are_refused_400(texture_world, segment):
    tmp_path, client = texture_world
    before = _tree(tmp_path)
    resp = client.get(f"/textures/{segment}")
    assert resp.status_code == 400, (segment, resp.status_code, resp.text[:200])
    assert SECRET not in resp.text
    assert _tree(tmp_path) == before


@pytest.mark.parametrize("segment", ["%2e%2e%2fsecret.txt", "..%2fsecret.txt", "%2e%2e/secret.txt", "..%2f..%2fx.yaml", "%2fetc%2fpasswd"])
def test_texture_paths_that_decode_to_a_separator_never_serve_the_target(texture_world, segment):
    """Once decoded these hold a ``/``, so the single-segment route does not match them at all
    (the static mount answers, and Starlette's own containment refuses); the secret is never
    served and nothing changes."""
    tmp_path, client = texture_world
    before = _tree(tmp_path)
    resp = client.get(f"/textures/{segment}")
    assert resp.status_code in (400, 404), (segment, resp.status_code)
    assert SECRET not in resp.text
    assert _tree(tmp_path) == before


@pytest.fixture
def profiles_world(tmp_path):
    profiles = tmp_path / "root" / "profiles"
    profiles.mkdir(parents=True)
    imagery = ("imagery:\n  url_template: 'https://tiles.example/{z}/{x}/{y}.png'\n"
               "  attribution: 'test'\n  max_level: 3\n")
    (profiles / "design.yaml").write_text(imagery)
    (tmp_path / "root" / "outside.yaml").write_text(imagery)  # reachable as profiles/../outside
    return tmp_path, profiles


def test_a_valid_profile_id_still_loads(profiles_world):
    _, profiles = profiles_world
    assert profile_loader.load_imagery_config("design", profiles_dir=profiles)["maxLevel"] == 3


def test_the_committed_profiles_all_load_by_their_ids():
    """Every committed profile id passes the rule (the inventory's profile row)."""
    from altavista.profile import REPO_ROOT
    for path in sorted((REPO_ROOT / "profiles").glob("*.yaml")):
        try:
            profile_loader.load_imagery_config(path.stem)
        except profile_loader.ProfileError as exc:
            assert "cannot name a profile file" not in str(exc), (path.stem, exc)


@pytest.mark.parametrize("profile_id", ["../outside", "..", "a/b", "a\\b", "a\0b", "", "%2e%2e%2foutside", "..%2foutside", ".hidden"])
def test_a_profile_id_that_is_not_a_path_component_is_refused(profiles_world, profile_id):
    tmp_path, profiles = profiles_world
    before = _tree(tmp_path)
    with pytest.raises(profile_loader.ProfileError, match="cannot name a profile file"):
        profile_loader.load_imagery_config(profile_id, profiles_dir=profiles)
    assert _tree(tmp_path) == before


def test_an_absolute_profile_id_is_refused(profiles_world):
    tmp_path, profiles = profiles_world
    absolute = str(tmp_path / "root" / "outside")  # `Path / "<absolute>.yaml"` would replace the directory
    with pytest.raises(profile_loader.ProfileError, match="cannot name a profile file"):
        profile_loader.load_imagery_config(absolute, profiles_dir=profiles)


SCRIPT = "Create Spacecraft {names};\nBeginMissionSequence;\n"


def test_valid_spacecraft_names_still_get_report_files(tmp_path):
    text, files = prepare_script(SCRIPT.format(names="SatA, Sat_B"), Frame(), str(tmp_path))
    assert files == {"SatA": str(tmp_path / "SatA.rpt"), "Sat_B": str(tmp_path / "Sat_B.rpt")}


@pytest.mark.parametrize("name", ["../x", "..", "a/b", "a\\b", "%2e%2e%2fx", ".hidden"])
def test_a_spacecraft_name_that_is_not_a_path_component_is_refused(tmp_path, name):
    reports = tmp_path / "reports"
    reports.mkdir()
    before = _tree(tmp_path)
    with pytest.raises(ValueError, match="cannot name a report file"):
        prepare_script(SCRIPT.format(names=name), Frame(), str(reports))
    with pytest.raises(ValueError, match="cannot name a report file"):
        prepare_script("BeginMissionSequence;\n", Frame(), str(reports), spacecraft=[name])
    assert _tree(tmp_path) == before


def test_an_absolute_spacecraft_name_is_refused(tmp_path):
    with pytest.raises(ValueError, match="cannot name a report file"):
        prepare_script("BeginMissionSequence;\n", Frame(), str(tmp_path / "reports"), spacecraft=[str(tmp_path / "x")])


@pytest.mark.parametrize("target", ["%2e%2e/secret.txt", "..%2fsecret.txt", "%2e%2e%2fsecret.txt", "a/..%2f..%2fsecret.txt", "%2fetc%2fpasswd", "static/%2e%2e/%2e%2e/secret.txt"])
def test_static_file_serving_never_leaves_the_web_directory(tmp_path, target):
    """Audit row: the viewer's static mount (Starlette ``StaticFiles`` via
    ``RevalidatingStaticFiles``) resolves a request path under ``web_dir`` and refuses one that
    resolves outside it. Proven here with encoded and plain traversals of a sibling file."""
    web = tmp_path / "web"
    web.mkdir()
    (web / "index.html").write_text("<html>home</html>")
    (tmp_path / "secret.txt").write_text(SECRET)
    client = TestClient(create_app(texture_dir=web, web_dir=web))
    before = _tree(tmp_path)
    resp = client.get(f"/{target}")
    assert SECRET not in resp.text
    assert resp.status_code in (400, 404), (target, resp.status_code)
    assert _tree(tmp_path) == before
