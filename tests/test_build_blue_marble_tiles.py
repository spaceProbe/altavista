"""`scripts/imagery/build_blue_marble_tiles.py` on a small synthetic plate (question 264).

Never touches the real NASA original or the network: a 3000x1500 plate (so the resample to the
2048x1024 deepest level is a non-integer ratio) with one flat colour per quadrant is encoded with
``cjpeg`` in ``tmp_path`` and the script runs on it as a subprocess with its own ``--sha256``.
Skipped, with the reason, when libjpeg-turbo's ``cjpeg``/``djpeg`` are not on PATH.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "imagery" / "build_blue_marble_tiles.py"
CJPEG = shutil.which("cjpeg")
DJPEG = shutil.which("djpeg")

pytestmark = pytest.mark.skipif(
    not (CJPEG and DJPEG),
    reason="cjpeg/djpeg (libjpeg-turbo-progs / libjpeg-turbo-utils / brew jpeg-turbo) not on PATH",
)

W, H = 3000, 1500
RED, GREEN, BLUE, YELLOW = (230, 20, 20), (20, 230, 20), (20, 20, 230), (230, 230, 20)
# row 0 = north, column 0 = west
NW, NE, SW, SE = RED, GREEN, BLUE, YELLOW


def _load_module():
    spec = importlib.util.spec_from_file_location("build_blue_marble_tiles", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _plate() -> np.ndarray:
    a = np.zeros((H, W, 3), dtype=np.uint8)
    a[: H // 2, : W // 2] = NW
    a[: H // 2, W // 2 :] = NE
    a[H // 2 :, : W // 2] = SW
    a[H // 2 :, W // 2 :] = SE
    return a


@pytest.fixture(scope="module")
def original(tmp_path_factory) -> tuple[Path, str]:
    d = tmp_path_factory.mktemp("orig")
    ppm = d / "plate.ppm"
    ppm.write_bytes(b"P6\n%d %d\n255\n" % (W, H) + _plate().tobytes())
    jpg = d / "plate.jpg"
    subprocess.run([CJPEG, "-quality", "95", "-outfile", str(jpg), str(ppm)], check=True)
    return jpg, hashlib.sha256(jpg.read_bytes()).hexdigest()


def _run(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    r = subprocess.run([sys.executable, "-I", str(SCRIPT), *args], capture_output=True, text=True)
    if check:
        assert r.returncode == 0, r.stderr
    return r


@pytest.fixture(scope="module")
def built(tmp_path_factory, original) -> tuple[Path, Path, dict]:
    jpg, sha = original
    base = tmp_path_factory.mktemp("builds")
    a, b = base / "a", base / "b"
    _run("--original", str(jpg), "--sha256", sha, "--out", str(a))
    _run("--original", str(jpg), "--sha256", sha, "--out", str(b))
    return a, b, json.loads((a / "manifest.json").read_text())


def _decode(path: Path) -> np.ndarray:
    r = subprocess.run([DJPEG, "-pnm", str(path)], capture_output=True, check=True)
    toks = r.stdout.split(None, 4)
    w, h = int(toks[1]), int(toks[2])
    return np.frombuffer(toks[4], dtype=np.uint8).reshape(h, w, 3)


def _near(px, colour, tol=40) -> bool:
    return all(abs(int(p) - c) <= tol for p, c in zip(px, colour))


def test_levels_and_tile_counts(built):
    a, _, m = built
    assert m["format"] == "altavista-imagery-pyramid/1"
    assert (m["min_level"], m["max_level"], m["tile_px"]) == (0, 2, 256)  # 3000 -> 2048, not 4096
    assert m["tile_count"] == 2 + 8 + 32
    assert len(m["files"]) == m["tile_count"] + 1
    for level in range(3):
        for x in range(2 ** (level + 1)):
            for y in range(2**level):
                assert (a / str(level) / str(x) / f"{y}.jpg").is_file()
    assert not (a / "3").exists()
    assert _decode(a / "body" / "earth-2048x1024.jpg").shape == (1024, 2048, 3)
    assert m["total_bytes"] == sum(f["bytes"] for f in m["files"].values())
    assert m["original"]["width"] == W and m["original"]["height"] == H
    assert m["encoder"]["cjpeg"].startswith("libjpeg-turbo") and m["encoder"]["djpeg"].startswith("libjpeg-turbo")
    assert "-quality" in m["encoder"]["args"]
    assert (a / "manifest.json").read_text().endswith("}\n")


def test_two_builds_are_byte_identical(built):
    a, b, m = built
    mb = json.loads((b / "manifest.json").read_text())
    assert m["pyramid_sha256"] == mb["pyramid_sha256"]
    assert m["files"] == mb["files"]
    for rel in m["files"]:
        assert (a / rel).read_bytes() == (b / rel).read_bytes(), rel


def test_wrong_sha256_refuses_before_writing(tmp_path, original):
    jpg, sha = original
    out = tmp_path / "out"
    bad = "0" * 64
    r = _run("--original", str(jpg), "--sha256", bad, "--out", str(out), check=False)
    assert r.returncode != 0
    assert bad in r.stderr and sha in r.stderr  # names both hashes
    assert not out.exists()
    assert list(tmp_path.glob("out.partial-*")) == []


def test_existing_out_refused_without_force(tmp_path, original):
    jpg, sha = original
    out = tmp_path / "out"
    out.mkdir()
    (out / "keep").write_text("x")
    r = _run("--original", str(jpg), "--sha256", sha, "--out", str(out), check=False)
    assert r.returncode != 0 and "already exists" in r.stderr
    assert (out / "keep").read_text() == "x"
    _run("--original", str(jpg), "--sha256", sha, "--out", str(out), "--force")
    assert not (out / "keep").exists() and (out / "manifest.json").is_file()
    assert [p.name for p in tmp_path.iterdir()] == ["out"]  # no partial/old leftovers


def test_verify_passes_then_fails_after_one_byte_changes(built, tmp_path):
    a, _, m = built
    copy = tmp_path / "copy"
    shutil.copytree(a, copy)
    ok = _run("--verify", str(copy))
    assert m["pyramid_sha256"] in ok.stdout
    tile = copy / "2" / "3" / "1.jpg"
    data = bytearray(tile.read_bytes())
    data[len(data) // 2] ^= 0x01
    tile.write_bytes(bytes(data))
    bad = _run("--verify", str(copy), check=False)
    assert bad.returncode != 0 and "2/3/1.jpg" in bad.stderr
    (copy / "1" / "0" / "0.jpg").unlink()
    bad = _run("--verify", str(copy), check=False)
    assert bad.returncode != 0 and "missing file 1/0/0.jpg" in bad.stderr


def test_verify_flags_an_unlisted_file(built, tmp_path):
    a, _, _ = built
    copy = tmp_path / "copy"
    shutil.copytree(a, copy)
    (copy / "2" / "extra.jpg").write_bytes(b"x")
    bad = _run("--verify", str(copy), check=False)
    assert bad.returncode != 0 and "extra.jpg" in bad.stderr


def test_orientation_north_up_and_y0_is_south(built):
    a, _, _ = built
    # level 1 is 4x2 tiles; y=0 is the south row, x=0 the westmost column
    for (x, y), colour in {(0, 0): SW, (0, 1): NW, (1, 0): SW, (1, 1): NW, (2, 0): SE, (3, 0): SE, (2, 1): NE, (3, 1): NE}.items():
        img = _decode(a / "1" / str(x) / f"{y}.jpg")
        assert _near(img[128, 128], colour), (x, y, img[128, 128])
        assert _near(img[8, 8], colour) and _near(img[247, 247], colour)
    # level 0, x=0 (the western hemisphere, y=0 its only row): pixel row 0 is NORTH, last row SOUTH
    west = _decode(a / "0" / "0" / "0.jpg")
    assert _near(west[4, 64], NW) and _near(west[251, 64], SW)
    assert _near(west[4, 192], NW) and _near(west[251, 192], SW)
    east = _decode(a / "0" / "1" / "0.jpg")
    assert _near(east[4, 64], NE) and _near(east[251, 64], SE)
    # the body image is the whole plate, north-up, west-left
    body = _decode(a / "body" / "earth-2048x1024.jpg")
    assert _near(body[100, 100], NW) and _near(body[100, 1900], NE)
    assert _near(body[900, 100], SW) and _near(body[900, 1900], SE)


def test_resample_is_independent_of_band_size_and_halve_is_exact():
    mod = _load_module()
    rng = np.random.default_rng(7)
    src = rng.integers(0, 256, size=(150, 300, 3), dtype=np.uint8)
    ref = mod.box_resample(src, 205, 102, band_rows=1000)
    for band in (1, 7, 64):
        assert np.array_equal(ref, mod.box_resample(src, 205, 102, band_rows=band)), band
    # weights are a partition of unity (exact overlaps / n_in) up to float64 rounding
    idx, w = mod.box_taps(300, 205)
    assert np.allclose(w.sum(axis=0), 1.0, rtol=0, atol=1e-15)
    assert mod.box_taps(8, 8)[1].max() == 1.0
    # integer 2x2 mean with +2 rounding
    sq = np.array([[[0], [0]], [[0], [3]]], dtype=np.uint8).repeat(3, axis=2)  # (0+0+0+3+2)//4 = 1
    assert mod.halve(sq)[0, 0, 0] == 1
    sq = np.array([[[1], [1]], [[1], [2]]], dtype=np.uint8).repeat(3, axis=2)  # (5+2)//4 = 1
    assert mod.halve(sq)[0, 0, 0] == 1
    sq = np.array([[[1], [2]], [[2], [2]]], dtype=np.uint8).repeat(3, axis=2)  # (7+2)//4 = 2
    assert mod.halve(sq)[0, 0, 0] == 2
    sq = np.array([[[0], [0]], [[1], [1]]], dtype=np.uint8).repeat(3, axis=2)  # (2+2)//4 = 1 (+1 would give 0)
    assert mod.halve(sq)[0, 0, 0] == 1
    sq = np.array([[[1], [1]], [[2], [2]]], dtype=np.uint8).repeat(3, axis=2)  # (6+2)//4 = 2 (+1 would give 1)
    assert mod.halve(sq)[0, 0, 0] == 2
    assert mod.max_level_for_width(21600) == 5 and mod.max_level_for_width(16384) == 5
    assert mod.max_level_for_width(16383) == 4 and mod.max_level_for_width(500) == -1
