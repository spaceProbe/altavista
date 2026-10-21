"""Question 264, the proof that the Blue Marble pyramid lands on the right places, in a REAL browser.

Every other viewer test draws the flat-colour fixture (``web/fixtures/tiles/``: one colour per tile),
and a flat colour cannot show a picture drawn upside down, mirrored, or shifted by half the world.
This file measures the real basemap: it points a headless Chrome at a real ``altavista serve`` that
serves the real pyramid, and compares the pixels the browser DRAWS with the NASA original, decoded
independently, at known latitudes and longitudes.

What is proved, and how
-----------------------
* **Reference truth**: the original (``world.topo.bathy.200407.3x21600x10800.jpg``, 21600 x 10800, 27 MB)
  decoded by ``djpeg -scale 1/8`` (libjpeg-turbo, a different code path from the pyramid generator's
  numpy resampling and from Chrome's decoder): 2700 x 1350 pixels, each the exact mean of an 8 x 8 block
  of the original (the DCT's DC term). Decoding all 700 MB of the original in a test would be
  needlessly heavy; the 1/8 decode IS the original, box-averaged, and the probes are chosen inside
  large homogeneous regions (desert, ice, ocean, rain forest) so that averaging does not matter. A
  colour at (lat, lon) is the mean of a +-1 degree window of it. It never reads a tile of the pyramid.
* **2D map** (``web/js/panels/map_panel.js``, its real ``render``): at about a dozen points of strong,
  distinct colour, including both sides of lon 0 and of the date line and both poles, the colour of the
  screenshot pixel at ``lonLatToPercent(lon, lat)`` of the mosaic matches the reference; and the
  ``<img>`` under a point just inside (0, 0) is the tile ``tileBoundsDeg`` says (tile y = 0 is the SOUTH
  row, rendered at the bottom).
* **Tiled globe** (the checkbox, ``GlobeLayer``): the camera is placed on the ECEF radial through a
  known (lat, lon) at 3.2, 1.5 and 1.15 Earth radii (the three select different levels), over both
  hemispheres, both sides of lon 0 and of lon 180. For the centre pixel and a ring of other points
  (projected to the screen with the camera that drew them): the ray through the pixel must hit the globe
  AT that lat/lon (not on the far side), on the tile ``tileBoundsDeg`` predicts, whose bound texture is
  that tile's URL; and the rendered colour must match the reference. Lighting is neutralised in the page
  (ambient pi, sun 0, so the pixel is the albedo) and DOM labels are hidden.
* **Body vs tiled globe**: the same cameras with the globe off draw Earth's plain body (GMAT's
  ``ModifiedBlueMarble.jpg`` when GMAT is present on this host, else, and always in the second run with
  ``--textures <empty dir>``, the pyramid's ``body/earth-2048x1024.jpg``); the colours must agree with the
  globe's and with the reference.
* **Hypotheses**: for every probe the reference is also read at the four ways a basemap goes wrong (the
  latitude negated, the longitude negated, the longitude shifted by 180, both), and the test asserts the
  identity wins by a margin, so a north-south flip, a mirror, or a half-world shift fails with a
  message naming which one the browser's picture matches. The probe set itself is checked to be able to
  tell all four from the truth before any browser is started.
* **Zero** page exceptions or console errors (the three CDP signals, question 211), and **no request
  leaves 127.0.0.1**.

How to run it (opt-in: it runs where its inputs are installed, and skips with a stated reason otherwise)
------------------------------------------------------------------------------------------------------
Needs Chrome/Chromium, ``djpeg`` and ``websockets`` (already in ``.venv``), the real pyramid (default
``~/altavista-data/imagery/bluemarble``, built once by ``scripts/imagery/build_blue_marble_tiles.py``)
and the original it was built from (default
``~/altavista-data/imagery/original/world.topo.bathy.200407.3x21600x10800.jpg``)::

    .venv/bin/python -m pytest -q -rs tests/test_viewer_basemap_orientation_browser.py

The two paths can be moved with ``AV_BASEMAP_DIR`` and ``AV_BASEMAP_ORIGINAL`` (read here, never assigned;
the server under test is configured with ``--imagery-dir`` / ``--textures``, never the environment,
question 199). ``AV_BASEMAP_SHOTS=<dir>`` also saves every screenshot taken (a diagnostic, like
``AV_BODIES_DUMP``), and ``pytest -s`` prints each probe's drawn and original colours. Pointing ``AV_BASEMAP_DIR`` at a pyramid built wrong (rows flipped, columns mirrored,
half a world off) makes the tests fail, which is how they were checked to have teeth. Never
cross-platform byte identity: the pyramid's hash holds only for the libjpeg-turbo recorded in its
manifest; this test compares pictures, with a stated tolerance, not bytes.
"""

from __future__ import annotations

import asyncio
import base64
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

import numpy as np
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent

BASEMAP_DIR = Path(os.environ.get("AV_BASEMAP_DIR") or Path.home() / "altavista-data" / "imagery" / "bluemarble")
ORIGINAL = Path(
    os.environ.get("AV_BASEMAP_ORIGINAL")
    or Path.home() / "altavista-data" / "imagery" / "original" / "world.topo.bathy.200407.3x21600x10800.jpg"
)

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
]

#: Tolerances are mean absolute differences over R, G and B (0..255) between a drawn pixel window and
#: the reference, and are measured, not guessed. The 2D map's pixels are the tile images at about one
#: texel per pixel: worst probe 7 (JPEG at quality 88, a window that straddles a coast). The tiled
#: globe samples a tile at a slant and at its own level: worst probe 12 (the 1.15 R view). A picture that is upside down,
#: mirrored or half a world off is 40..220 off at the probes below.
MAP_TOLERANCE = 15.0
GLOBE_TOLERANCE = 20.0
#: GMAT's own Earth texture (`ModifiedBlueMarble.jpg`) is a deliberately re-graded Blue Marble
#: (brighter, bluer oceans; up to 60 off the original at the probes), so it is held to the picture's
#: ORIENTATION, not to its grading: the pyramid's body image is held to GLOBE_TOLERANCE instead.
GMAT_BODY_VS_GLOBE_TOLERANCE = 90.0
BODY_VS_GLOBE_TOLERANCE = 25.0
#: A wrong hypothesis must explain the browser's pixels at least this many times WORSE (summed over a
#: view's probes) than the truth does (for GMAT's re-graded texture, which cannot match the original
#: closely, only this much worse).
MARGIN = 2.0
GMAT_MARGIN = 1.2
#: The ray through a probe's pixel must hit the globe within this many degrees of the probe.
HIT_TOLERANCE_DEG = 1.5
#: Brightness of the plain body against the tiled globe, over every probe of a run (Pearson r):
#: the same Earth, so the light and dark places agree whatever the grading. Measured 0.976 for
#: GMAT's re-graded texture over 49 probes and 0.9997 for the pyramid's own body image.
BODY_GLOBE_CORRELATION_GMAT = 0.9
BODY_GLOBE_CORRELATION = 0.99


def _find_chrome() -> str | None:
    for candidate in CHROME_CANDIDATES:
        if candidate.startswith("/") and Path(candidate).exists():
            return candidate
        found = shutil.which(candidate)
        if found:
            return found
    return None


def _free_port() -> int:
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


# ------------------------------------------------------------------------------ the reference
def _decode_original() -> np.ndarray:
    """The original, decoded by djpeg at 1/8 (each output pixel is the mean of an 8 x 8 block)."""
    djpeg = shutil.which("djpeg")
    if not djpeg:
        pytest.skip("djpeg (libjpeg-turbo) is not on PATH: the reference is the original decoded independently")
    if not ORIGINAL.is_file():
        pytest.skip(
            f"the NASA original is not at {ORIGINAL} (AV_BASEMAP_ORIGINAL): the reference is the original, "
            "decoded independently of the pyramid generator"
        )
    done = subprocess.run([djpeg, "-scale", "1/8", "-pnm", str(ORIGINAL)], capture_output=True, check=False)
    if done.returncode != 0:
        pytest.skip(f"djpeg could not decode {ORIGINAL}: {done.stderr.decode('utf-8', 'replace')[:300]}")
    head, dims, maxval, body = done.stdout.split(b"\n", 3)
    assert head == b"P6" and maxval == b"255", f"unexpected djpeg output header {head!r}/{maxval!r}"
    w, h = (int(v) for v in dims.split())
    assert (w, h) == (2700, 1350), f"expected the 21600x10800 original at 1/8 = 2700x1350, got {w}x{h}"
    return np.frombuffer(body, dtype=np.uint8).reshape(h, w, 3)


def _ref_color(ref: np.ndarray, lat: float, lon: float, half_deg: float = 1.0) -> np.ndarray:
    """Mean RGB of the +-half_deg window around (lat, lon) of the equirectangular original."""
    h, w, _ = ref.shape
    r0 = int(round((90 - min(90.0, lat + half_deg)) / 180 * h))
    r1 = max(r0 + 1, int(round((90 - max(-90.0, lat - half_deg)) / 180 * h)))
    lon0 = ((lon - half_deg + 180) % 360) - 180
    c0 = int(math.floor((lon0 + 180) / 360 * w))
    n = int(math.ceil(2 * half_deg / 360 * w)) + 1
    cols = (c0 + np.arange(n)) % w
    return ref[r0:r1][:, cols].reshape(-1, 3).astype(float).mean(axis=0)


#: The four ways a basemap goes wrong, as a map from (lat, lon) to where the WRONG picture would read.
HYPOTHESES = {
    "north-south flipped": lambda lat, lon: (-lat, lon),
    "east-west mirrored": lambda lat, lon: (lat, -lon),
    "shifted 180 degrees of longitude": lambda lat, lon: (lat, lon + 180),
    "rotated 180 degrees (flipped and shifted)": lambda lat, lon: (-lat, lon + 180),
}


def _err(a, b) -> float:
    return float(np.abs(np.asarray(a, dtype=float) - np.asarray(b, dtype=float)).mean())


def _expected_tile(level: int, lat: float, lon: float) -> list[int]:
    """The tile containing (lat, lon), from the scheme as documented (docs, ``web/js/globe_lod.js``
    ``tileBoundsDeg``): level L has 2**(L+1) x 2**L tiles; x counts east from lon -180; y counts NORTH
    from the south pole (y = 0 is the southernmost row). Deliberately a second statement of the scheme."""
    nx, ny = 2 ** (level + 1), 2**level
    return [level, min(nx - 1, int((lon + 180) // (360 / nx))), min(ny - 1, int((lat + 90) // (180 / ny)))]


# ----------------------------------------------------------------- probes (lat, lon, what it is)
MAP_PROBES = [
    (23, 13, "Sahara"),
    (22, 47, "Arabian desert"),
    (-28, 22, "Kalahari"),
    (-25, 135, "Australian outback"),
    (-3, -60, "Amazon forest"),
    (72, -40, "Greenland ice"),
    (-80, 0, "Antarctic ice"),
    (-55, -150, "Southern Ocean"),
    (0, -150, "equatorial Pacific"),
    (45, 105, "Mongolia"),
    (0, 12, "Gabon forest, east of lon 0"),
    (0, -12, "Atlantic, west of lon 0"),
    (66, 172, "Chukotka, west of the date line"),
    (65, -160, "Alaska, east of the date line"),
    (-17, 178, "Fiji sea, west of the date line"),
    (84, -150, "Arctic Ocean"),
]

# (sub-point lat, sub-point lon, camera distance in Earth radii, [(lat, lon, what), ...]); the first
# probe of each view is its sub-point, so the centre pixel is looked at.
GLOBE_VIEWS = [
    (10, 15, 3.2, [(10, 15, "Sahel"), (24, 45, "Arabia"), (32, 5, "Algerian Sahara"), (-24, 29, "southern Africa"), (56, 5, "North Sea"), (0, -12, "Atlantic")]),
    (-25, 135, 3.2, [(-25, 135, "outback"), (-27, 125, "outback west"), (-19, 109, "Indian Ocean"), (-3, 149, "Bismarck Sea"), (-51, 117, "Southern Ocean")]),
    (-3, -60, 3.2, [(-3, -60, "Amazon"), (-21, -62, "Gran Chaco"), (35, -86, "eastern North America"), (27, -54, "North Atlantic"), (-21, -54, "Brazil")]),
    (72, -40, 3.2, [(72, -40, "Greenland ice"), (30, 6, "Algerian Sahara"), (62, -10, "Norwegian Sea"), (38, -90, "midwest USA"), (30, -58, "Sargasso")]),
    (-80, 0, 3.2, [(-80, 0, "Antarctic ice"), (-74, 30, "Antarctic ice east"), (-66, -50, "Weddell sea"), (-58, 30, "Southern Ocean")]),
    (0, 180, 3.2, [(0, 180, "date line, equatorial"), (-26, 162, "Coral Sea"), (-25, 150, "eastern Australia"), (-2, -158, "Pacific"), (-18, -134, "Tuamotu"), (-34, 178, "New Zealand sea")]),
    # Close views: the canvas is a 887 x 642 pane, so at 1.5 R it shows about +-18 degrees of longitude and
    # +-12 of latitude around the sub-point, and at 1.15 R about +-5 and +-3.5: probes stay inside that.
    (10, 0, 1.5, [(10, 0, "savanna at lon 0"), (20, 6, "Sahara"), (21, -4, "Sahara, west of lon 0"), (4, 11, "Cameroon forest, east of lon 0"), (2, -9, "Atlantic, west of lon 0"), (18, 13, "Sahara, east of lon 0")]),
    (64, 180, 1.5, [(64, 180, "Gulf of Anadyr"), (67, 179, "Chukotka, west of the date line"), (63, -177, "Bering shelf, east of the date line"), (59, 174, "Bering Sea"), (65, -160, "Alaska"), (68, 171, "Chukotka")]),
    (5, 7, 1.15, [(5, 7, "Niger delta"), (6.5, 5, "forest, west"), (5, 9, "forest, east"), (3.5, 3, "Gulf of Guinea"), (3.5, 7, "shallow coast"), (8, 3, "Benin forest")]),
]


def _probe_power(probes) -> dict[str, float]:
    """Summed reference error of each wrong hypothesis against the truth over `probes`."""
    ref = _REF_CACHE["ref"]
    out = {}
    for name, f in HYPOTHESES.items():
        out[name] = sum(_err(_ref_color(ref, lat, lon), _ref_color(ref, *f(lat, lon))) for lat, lon, *_ in probes)
    return out


_REF_CACHE: dict = {}


@pytest.fixture(scope="module")
def reference() -> np.ndarray:
    ref = _decode_original()
    _REF_CACHE["ref"] = ref
    return ref


@pytest.fixture(scope="module")
def chrome_path() -> str:
    path = _find_chrome()
    if not path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    return path


def test_the_probe_sets_can_tell_every_wrong_basemap_from_the_right_one(reference):
    """Before any browser: each hypothesis must read a clearly different picture at the probes, or a test
    built on them could not fail for it. Needs only the original."""
    for label, probes in [("map", MAP_PROBES)] + [(f"globe view {i}", v[3]) for i, v in enumerate(GLOBE_VIEWS)]:
        power = _probe_power(probes)
        for name, total in power.items():
            assert total >= len(probes) * 12.0, (
                f"the {label} probes read nearly the same colours under '{name}' (summed difference {total:.0f} "
                f"over {len(probes)} probes): a basemap that is {name} would pass. Choose probes that tell them apart."
            )


# ------------------------------------------------------------------------------- the server
class _LiveServer:
    def __init__(self, port: int, proc: subprocess.Popen) -> None:
        self.port = port
        self.proc = proc
        self.health: dict = {}

    @property
    def base(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def stop(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=10)


_EARTH_KM = 6378.137
_SCENARIO = {
    "name": "basemap-orientation-proof",
    "frame": {"name": "EarthMJ2000Eq"},
    "bodies": [
        {
            "name": "Earth", "central": True, "radius": _EARTH_KM, "t": [0.0], "pos": [0.0, 0.0, 0.0],
            "quat": [0.0, 0.0, 0.0, 1.0], "texture": "/textures/ModifiedBlueMarble.jpg",
        }
    ],
    "spacecraft": [],
}


def _start_server(imagery_dir: Path, textures: Path | None) -> _LiveServer:
    port = _free_port()
    cmd = [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port), "--imagery-dir", str(imagery_dir)]
    if textures is not None:
        cmd += ["--textures", str(textures)]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT), env=dict(os.environ))
    server = _LiveServer(port, proc)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"{server.base}/api/health", timeout=0.5) as r:
                if r.status == 200:
                    server.health = json.loads(r.read())
                    break
        except Exception:
            time.sleep(0.05)
    else:
        server.stop()
        pytest.fail("altavista server did not become ready within 30s")
    return server


def _publish(server: _LiveServer) -> None:
    req = urllib.request.Request(
        f"{server.base}/api/scenario", data=json.dumps(_SCENARIO).encode(), headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=20) as r:
        assert r.status in (200, 201), f"publishing the proof scenario answered HTTP {r.status}"


@pytest.fixture(scope="module")
def server_gmat(reference):
    """The server with its default textures: GMAT's own Earth texture where GMAT is present."""
    server = _start_server(BASEMAP_DIR, None)
    imagery = server.health.get("imagery") or {}
    if imagery.get("installed") is not True:
        server.stop()
        pytest.skip(
            f"the Blue Marble pyramid is not installed at {BASEMAP_DIR} (AV_BASEMAP_DIR): the server says "
            f"{imagery.get('reason') or imagery!r}. Build it with scripts/imagery/build_blue_marble_tiles.py."
        )
    _publish(server)
    try:
        yield server
    finally:
        server.stop()


@pytest.fixture(scope="module")
def server_no_gmat(server_gmat, tmp_path_factory):
    """The server with `--textures <empty dir>` (a host without GMAT): Earth uses the pyramid's body image."""
    empty = tmp_path_factory.mktemp("no-gmat-textures")
    server = _start_server(BASEMAP_DIR, empty)
    report = server.health.get("earthTexture") or {}
    if report.get("publishes") != "pyramid":
        server.stop()
        pytest.fail(f"with an empty --textures directory Earth must be published with the pyramid's body image; health says {report!r}")
    _publish(server)
    try:
        yield server
    finally:
        server.stop()


# ------------------------------------------------------------------------------- the browser
class _Session:
    """A fresh headless Chrome on one page, one CDP connection, the three error signals (question 211)
    plus every request's host, with screenshot and page-side PNG sampling."""

    def __init__(self, chrome_path: str, width: int = 2000, height: int = 760) -> None:
        self.chrome_path, self.width, self.height = chrome_path, width, height
        self.errors: list[str] = []
        self.requests: list[str] = []
        self.external: list[str] = []

    async def __aenter__(self) -> "_Session":
        import websockets

        self._profile = tempfile.mkdtemp(prefix="basemap-orientation-chrome-")
        cdp_port = _free_port()
        self._chrome = subprocess.Popen(
            [
                self.chrome_path, f"--user-data-dir={self._profile}", "--headless=new", f"--remote-debugging-port={cdp_port}",
                "--no-first-run", "--no-default-browser-check", "--disable-background-networking", "--disable-sync",
                "--disable-component-update", "--disable-extensions", "--use-gl=swiftshader", "--enable-unsafe-swiftshader",
                f"--window-size={self.width},{self.height}", "about:blank",
            ],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        info = None
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/version", timeout=0.5) as r:
                    info = json.loads(r.read())
                    break
            except Exception:
                await asyncio.sleep(0.1)
        if info is None:
            raise RuntimeError("headless Chrome did not expose a DevTools endpoint in time")
        self._bws = await websockets.connect(info["webSocketDebuggerUrl"], max_size=None)
        await self._bws.send(json.dumps({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}))
        async for raw in self._bws:
            msg = json.loads(raw)
            if msg.get("id") == 1:
                target_id = msg["result"]["targetId"]
                break
        plist = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/list", timeout=3).read())
        page = next(p for p in plist if p["id"] == target_id)
        self._ws = await websockets.connect(page["webSocketDebuggerUrl"], max_size=None)
        self._n = 1
        self._pending: dict[int, asyncio.Future] = {}
        self._reader = asyncio.create_task(self._read())
        for domain in ("Runtime", "Log", "Page", "Network"):
            await self.send(f"{domain}.enable")
        # One CSS pixel = one device pixel, so screenshot pixels are DOM pixels.
        await self.send("Emulation.setDeviceMetricsOverride", {"width": self.width, "height": self.height, "deviceScaleFactor": 1, "mobile": False})
        return self

    async def _read(self) -> None:
        async for raw in self._ws:
            msg = json.loads(raw)
            mid = msg.get("id")
            if mid is not None and mid in self._pending:
                self._pending.pop(mid).set_result(msg)
                continue
            method, params = msg.get("method"), msg.get("params", {})
            if method == "Log.entryAdded":
                entry = params["entry"]
                if entry.get("level") == "error":
                    self.errors.append(f"[Log:{entry.get('source')}] {entry.get('text')} ({entry.get('url')})")
            elif method == "Runtime.consoleAPICalled":
                if params.get("type") == "error":
                    self.errors.append("[console.error] " + " ".join(str(a.get("value", a.get("description", ""))) for a in params.get("args", [])))
            elif method == "Runtime.exceptionThrown":
                details = params.get("exceptionDetails", {})
                exc = details.get("exception") or {}
                self.errors.append(f"[Runtime.exceptionThrown] {details.get('text', 'exception')}: {exc.get('description') or exc.get('value') or ''}")
            elif method == "Network.requestWillBeSent":
                url = params["request"]["url"]
                if url.startswith(("data:", "blob:", "about:")):
                    continue
                self.requests.append(url)
                if urlparse(url).hostname != "127.0.0.1":
                    self.external.append(url)

    async def send(self, method: str, params: dict | None = None, timeout: float = 90):
        self._n += 1
        fut = asyncio.get_running_loop().create_future()
        self._pending[self._n] = fut
        await self._ws.send(json.dumps({"id": self._n, "method": method, "params": params or {}}))
        msg = await asyncio.wait_for(fut, timeout)
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error']}")
        return msg.get("result", {})

    async def js(self, expr: str):
        r = await self.send("Runtime.evaluate", {"expression": expr, "returnByValue": True, "awaitPromise": True})
        if r.get("exceptionDetails"):
            raise RuntimeError(f"page JS threw: {json.dumps(r['exceptionDetails'])[:1500]}")
        return r["result"].get("value")

    async def open_viewer(self, base: str) -> None:
        await self.send("Page.navigate", {"url": base + "/"})
        for _ in range(300):
            await asyncio.sleep(0.1)
            try:
                if await self.js("!!(window.altavistaViewer && window.altavistaViewer.bodies.has('Earth') && window.altavistaCurrentScenario)"):
                    return
            except RuntimeError:
                pass
        raise RuntimeError("the viewer did not load the proof scenario within 30 s")

    async def screenshot(self, save_to: Path | None = None) -> bytes:
        r = await self.send("Page.captureScreenshot", {"format": "png", "captureBeyondViewport": False})
        png = base64.b64decode(r["data"])
        if save_to is not None:
            save_to.parent.mkdir(parents=True, exist_ok=True)
            save_to.write_bytes(png)
        return png

    async def settled_shot(self, windows: list[list[float]], save_to: Path | None = None) -> list:
        """Screenshot and sample `windows`, retrying while every probe reads pure white: the first frame
        a fresh headless Chrome presents after its shaders compile can be a blank white canvas, measured
        as a whole view of 255,255,255 that is not a picture of anything."""
        cols: list = []
        for attempt in range(6):
            await asyncio.sleep(0.4 if attempt == 0 else 1.0)
            png = await self.screenshot(save_to)
            cols = await self.sample(png, windows)
            real = [c for c in cols if c is not None]
            if not real or not all(min(c) >= 254.5 for c in real):
                break
        return cols

    async def sample(self, png: bytes, windows: list[list[float]]) -> list[list[float]]:
        """Mean RGB of each (x, y, half) window of the PNG, decoded by the page itself."""
        expr = """(async (b64, wins) => {
          const img = new Image(); img.src = 'data:image/png;base64,' + b64; await img.decode();
          const c = document.createElement('canvas'); c.width = img.naturalWidth; c.height = img.naturalHeight;
          const g = c.getContext('2d', {willReadFrequently: true}); g.drawImage(img, 0, 0);
          return wins.map(([x, y, h]) => {
            const x0 = Math.max(0, Math.round(x) - h), y0 = Math.max(0, Math.round(y) - h);
            const w = Math.min(c.width - x0, 2 * h + 1), hh = Math.min(c.height - y0, 2 * h + 1);
            if (w <= 0 || hh <= 0) return null;
            const d = g.getImageData(x0, y0, w, hh).data; let r = 0, gg = 0, b = 0, n = 0;
            for (let i = 0; i < d.length; i += 4) { r += d[i]; gg += d[i + 1]; b += d[i + 2]; n++; }
            return [r / n, gg / n, b / n];
          });
        })(%s, %s)""" % (json.dumps(base64.b64encode(png).decode()), json.dumps(windows))
        return await self.js(expr)

    async def __aexit__(self, *exc) -> None:
        self._reader.cancel()
        try:
            await self._reader
        except asyncio.CancelledError:
            pass
        await self._ws.close()
        await self._bws.close()
        self._chrome.terminate()
        try:
            self._chrome.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self._chrome.kill()
            self._chrome.wait(timeout=10)
        shutil.rmtree(self._profile, ignore_errors=True)


def _session_problems(session: _Session) -> list[str]:
    """Zero page exceptions or console errors (three CDP signals), and no request off 127.0.0.1."""
    out = [f"page exception/console error: {e}" for e in session.errors]
    out += [f"a request left 127.0.0.1: {u}" for u in session.external]
    if not any("/imagery/bluemarble/" in u for u in session.requests):
        out.append("no request ever asked for the Blue Marble pyramid")
    return out


def _hypothesis_report(ref, rows) -> tuple[float, dict[str, float]]:
    """`rows` = [(lat, lon, drawn_rgb), ...] -> (identity error sum, {hypothesis: error sum})."""
    ident = sum(_err(drawn, _ref_color(ref, lat, lon)) for lat, lon, drawn in rows)
    wrong = {
        name: sum(_err(drawn, _ref_color(ref, *f(lat, lon))) for lat, lon, drawn in rows) for name, f in HYPOTHESES.items()
    }
    return ident, wrong


def _verdict(ref, rows, what: str, tolerance: float | None = GLOBE_TOLERANCE, margin: float = MARGIN) -> list[str]:
    """Problems with `rows` (empty = ok): each probe within `tolerance` of the truth (None: not held to
    the original's colours, only to its orientation), and the truth beating every wrong hypothesis by
    `margin` over the whole set."""
    problems: list[str] = []
    ident, wrong = _hypothesis_report(ref, rows)
    # `pytest -s` shows exactly what was measured (question 148: show the artifact).
    print(f"\n{what}: {len(rows)} probes, summed error vs the original {ident:.0f}; against the wrong pictures: "
          + ", ".join(f"{n} {t:.0f}" for n, t in wrong.items()))
    for lat, lon, drawn in rows:
        truth = _ref_color(ref, lat, lon)
        e = _err(drawn, truth)
        print(f"    lat {lat:>4} lon {lon:>5}: drawn {np.round(drawn).astype(int).tolist()} original {np.round(truth).astype(int).tolist()} error {e:4.1f}")
        if tolerance is not None and e > tolerance:
            best = min(HYPOTHESES, key=lambda n: _err(drawn, _ref_color(ref, *HYPOTHESES[n](lat, lon))))
            best_e = _err(drawn, _ref_color(ref, *HYPOTHESES[best](lat, lon)))
            hint = f"; it matches the picture {best} better (error {best_e:.0f})" if best_e < e else ""
            problems.append(
                f"{what} at lat {lat}, lon {lon}: drawn rgb {np.round(drawn).astype(int).tolist()} but the original there is "
                f"{np.round(truth).astype(int).tolist()} (mean abs error {e:.0f} > {tolerance:.0f}){hint}"
            )
    for name, total in wrong.items():
        if total < margin * max(ident, 1.0):
            problems.append(
                f"{what}: the drawn pixels fit the original {name} (summed error {total:.0f}) nearly as well as "
                f"the original as it is (summed error {ident:.0f})"
            )
    return problems


# --------------------------------------------------------------------------------- 2D map
_MAP_JS = r"""
(async (level) => {
  const mod = await import('/js/panels/map_panel.js');
  const gl = await import('/js/globe_lod.js');
  const sc = window.altavistaCurrentScenario;
  let box = document.getElementById('basemap-probe');
  if (box) box.remove();
  box = document.createElement('div');
  box.id = 'basemap-probe';
  box.style.cssText = 'position:fixed;left:0;top:0;width:1024px;height:512px;z-index:99999;background:#000;margin:0;padding:0;overflow:hidden';
  document.body.appendChild(box);
  mod.render(box, {sc, level});
  const imgs = () => [...box.querySelectorAll('img.av-map-tile')];
  for (let i = 0; i < 600; i++) {
    const all = imgs();
    if (all.length && all.every((im) => im.complete && im.naturalWidth > 0)) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const all = imgs();
  const mosaic = box.querySelector('.av-map-mosaic');
  const wrap = box.querySelector('.av-map-wrap');
  const r = mosaic.getBoundingClientRect();
  const wr = wrap.getBoundingClientRect();
  const out = {
    level, rect: [r.x, r.y, r.width, r.height], wrapRect: [wr.x, wr.y, wr.width, wr.height],
    tiles: all.length, loaded: all.filter((im) => im.complete && im.naturalWidth > 0).length,
    srcOrder: all.map((im) => im.getAttribute('alt')),
    notice: (box.querySelector('.av-panel-notice') || {}).textContent || null,
  };
  return out;
})(%d)
"""

_MAP_POINTS_JS = r"""
(async (pts) => {
  const mod = await import('/js/panels/map_panel.js');
  const box = document.getElementById('basemap-probe');
  const mosaic = box.querySelector('.av-map-mosaic');
  const r = mosaic.getBoundingClientRect();
  return pts.map(([lat, lon]) => {
    const p = mod.lonLatToPercent(lon, lat);
    const x = r.x + p.xPct / 100 * r.width, y = r.y + p.yPct / 100 * r.height;
    const el = document.elementsFromPoint(x, y).find((e) => e.classList && e.classList.contains('av-map-tile'));
    return {lat, lon, x, y, tile: el ? el.getAttribute('alt') : null};
  });
})(%s)
"""


async def _map_proof(chrome_path: str, server: _LiveServer, ref: np.ndarray, shots: Path | None) -> list[str]:
    problems: list[str] = []
    async with _Session(chrome_path) as s:
        await s.open_viewer(server.base)
        for level in (1, 3):
            info = await s.js(_MAP_JS % level)
            if info["notice"]:
                problems.append(f"level {level}: the map shows a notice instead of the basemap: {info['notice']}")
                continue
            nx, ny = 2 ** (level + 1), 2**level
            if info["tiles"] != nx * ny or info["loaded"] != info["tiles"]:
                problems.append(f"level {level}: {info['loaded']} of {info['tiles']} tile images loaded, expected {nx * ny}")
            if info["wrapRect"][2:] != info["rect"][2:]:
                problems.append(f"level {level}: the mosaic ({info['rect']}) does not fill the map wrap ({info['wrapRect']}), so lonLatToPercent would land off the picture")
            pts = [(lat, lon) for lat, lon, _ in MAP_PROBES] + [(0.5, 0.5), (-0.5, 0.5), (0.5, -0.5), (-0.5, -0.5)]
            located = await s.js(_MAP_POINTS_JS % json.dumps(pts))
            png = await s.screenshot(shots / f"map-level{level}.png" if shots else None)
            cols = await s.sample(png, [[p["x"], p["y"], 2] for p in located])
            rows = [(p["lat"], p["lon"], c) for p, c in zip(located[: len(MAP_PROBES)], cols[: len(MAP_PROBES)])]
            problems += _verdict(ref, rows, f"2D map level {level}", tolerance=MAP_TOLERANCE)
            # The tile under each of the four points around (0, 0) is the one tileBoundsDeg says; the
            # level's row 0 is the NORTH row, so y counts up from the bottom of the picture.
            for p in located[len(MAP_PROBES):]:
                want = _expected_tile(level, p["lat"], p["lon"])
                got = p["tile"]
                if got != f"tile {want[0]}/{want[1]}/{want[2]}":
                    problems.append(
                        f"level {level}: the image under lat {p['lat']}, lon {p['lon']} is '{got}' but tileBoundsDeg puts that point in tile {want[0]}/{want[1]}/{want[2]}"
                    )
    return problems + _session_problems(s)


def test_the_2d_map_draws_the_original_at_the_right_places(chrome_path, server_gmat, reference, tmp_path):
    """The map's own ``render`` over the real pyramid, at its real level (1) and a deeper one (3)."""
    shots = Path(os.environ["AV_BASEMAP_SHOTS"]) if os.environ.get("AV_BASEMAP_SHOTS") else None
    problems = asyncio.run(_map_proof(chrome_path, server_gmat, reference, shots))
    assert not problems, "the 2D map does not draw the original where it belongs:\n  " + "\n  ".join(problems)


# -------------------------------------------------------------------------------- the globe
_GLOBE_JS = r"""
(async (lon, lat, radii, pts, mode) => {
  const v = window.altavistaViewer;
  const g = await import('/js/globe_lod.js');
  const THREE = await import('three');
  const pump = async (n) => { for (let i = 0; i < n; i++) await new Promise((r) => requestAnimationFrame(r)); };
  const box = document.getElementById('opt-globe');
  const want = mode === 'globe';
  if (box.checked !== want) { box.checked = want; box.dispatchEvent(new Event('change', {bubbles: true})); }
  // Lighting is a pure multiplier of the albedo: ambient pi (Lambert's 1/pi) and no sun makes a pixel
  // equal the texel. Labels are DOM overlays on the canvas.
  v.ambient.intensity = Math.PI; v.sun.intensity = 0; v.options.labels = false;
  for (const el of document.querySelectorAll('.label, .av-label')) el.style.display = 'none';
  const earth = v.bodies.get('Earth');
  v.setFocus(null); v.fit();
  const V = v.camera.position.constructor;
  const e = g.geodeticToEcef(lon, lat, 0);
  const d = new V(e.x, e.y, e.z).normalize().applyQuaternion(earth.mesh.quaternion);
  const c = earth.mesh.position.clone();
  const R = earth.mesh.scale.x;
  v.controls.target.copy(c);
  v.camera.position.copy(c).addScaledVector(d, R * radii);
  v._updateEntitiesCamera();
  let stable = 0, last = '';
  for (let i = 0; i < 900 && stable < 20; i++) {
    await pump(1);
    const gl = v.globeLayer;
    let ready = true; const keys = [];
    if (want) {
      if (!gl) { ready = false; }
      else gl.group.traverse((o) => { if (o.isMesh) { keys.push(g.tileKey(o.userData.tile)); if (!o.material.map) ready = false; } });
    } else {
      if (gl) ready = false;
      const mat = earth.mesh.material;
      if (!earth.mesh.visible || !mat.map || !(mat.map.image && (mat.map.image.naturalWidth || mat.map.image.width))) ready = false;
    }
    const k = keys.sort().join(',');
    if (ready && k === last) stable++; else stable = 0;
    last = k;
  }
  await pump(3);
  const rect = document.getElementById('canvas').getBoundingClientRect();
  const out = {rect: [rect.x, rect.y, rect.width, rect.height], pts: [], levels: {}, meshes: 0, ready: stable >= 20};
  const gl = v.globeLayer;
  if (gl) {
    gl.group.updateMatrixWorld(true);
    gl.group.traverse((o) => { if (o.isMesh) { out.meshes++; const L = o.userData.tile.level; out.levels[L] = (out.levels[L] || 0) + 1; } });
  } else {
    out.bodyTexture = (earth.mesh.material.map && earth.mesh.material.map.image && (earth.mesh.material.map.image.currentSrc || earth.mesh.material.map.image.src)) || null;
  }
  const ray = new THREE.Raycaster();
  for (const [plon, plat] of pts) {
    const ec = g.geodeticToEcef(plon, plat, 0);
    const world = gl
      ? gl.group.localToWorld(new V(ec.x, ec.y, ec.z).multiplyScalar(g.SCENE_UNITS_PER_METRE))
      : earth.mesh.localToWorld(new V(ec.x, ec.y, ec.z).normalize());
    const ndc = world.clone().project(v.camera);
    const rec = {lon: plon, lat: plat, x: rect.x + (ndc.x + 1) / 2 * rect.width, y: rect.y + (1 - ndc.y) / 2 * rect.height};
    if (gl) {
      ray.setFromCamera({x: ndc.x, y: ndc.y}, v.camera);
      const hits = ray.intersectObjects(gl.group.children, false);
      if (hits.length) {
        const h = hits[0], t = h.object.userData.tile;
        const lp = gl.group.worldToLocal(h.point.clone()).divideScalar(g.SCENE_UNITS_PER_METRE);
        const geo = g.ecefToGeodeticDeg(lp.x, lp.y, lp.z);
        const img = h.object.material.map && h.object.material.map.image;
        rec.hit = {tile: [t.level, t.x, t.y], lon: geo.lonDeg, lat: geo.latDeg, src: img ? (img.currentSrc || img.src) : null};
      }
    }
    out.pts.push(rec);
  }
  return out;
})(%s, %s, %s, %s, %s)
"""


def _look(probe, hit_lat, hit_lon) -> float:
    """Great-circle angle, degrees, between the probe and where the ray hit."""
    p1, p2 = math.radians(probe[0]), math.radians(hit_lat)
    c = math.sin(p1) * math.sin(p2) + math.cos(p1) * math.cos(p2) * math.cos(math.radians(hit_lon - probe[1]))
    return math.degrees(math.acos(max(-1.0, min(1.0, c))))


async def _globe_proof(chrome_path: str, server: _LiveServer, ref: np.ndarray, views, shots: Path | None, tag: str, gmat: bool) -> tuple[list[str], dict]:
    """Drive every view with the globe on, then off (the plain body), and report every way the picture
    differs from the original. `gmat`: the body's texture is GMAT's re-graded one (held to orientation)
    rather than the pyramid's own whole-Earth image (held to colour)."""
    problems: list[str] = []
    levels_used: dict[str, dict] = {}
    luminance: list[tuple[float, float]] = []
    gmat_rows: list = []
    want_body = "/textures/ModifiedBlueMarble.jpg" if gmat else "/imagery/bluemarble/body/earth-2048x1024.jpg"
    async with _Session(chrome_path) as s:
        await s.open_viewer(server.base)
        for i, (lat0, lon0, radii, probes) in enumerate(views):
            pts = json.dumps([[lon, lat] for lat, lon, _ in probes])
            name = f"{tag}-view{i}-lat{lat0}-lon{lon0}-{radii}R"
            on = await s.js(_GLOBE_JS % (lon0, lat0, radii, pts, json.dumps("globe")))
            cols_on = await s.settled_shot([[p["x"], p["y"], 2] for p in on["pts"]], shots / f"{name}-globe.png" if shots else None)
            levels_used[name] = on["levels"]
            label = f"tiled globe {tag} view lat {lat0}, lon {lon0}, {radii} R (levels {on['levels']})"
            if not on["ready"] or not on["meshes"]:
                problems.append(f"{label}: the globe never settled (meshes {on['meshes']})")
                continue
            # The selected level under the disc centre (the first probe is the sub-point): at least level 2
            # (256 * 2**3 = 2048 px round the equator, as sharp as the plain body's 2048-px texture) at the
            # default 3.2 R and at 1.5 R, and the finest level (5) at 1.15 R.
            centre = ((on["pts"][0] or {}).get("hit") or {}).get("tile", [None])[0]
            if centre is None or not (centre == 5 if radii < 1.3 else centre >= 2):
                problems.append(
                    f"{label}: the tile under the centre of the disc is level {centre}, expected "
                    f"{'5' if radii < 1.3 else 'at least 2'} (level 2 is as sharp as the 2048-px body texture; "
                    f"the level floor, globe_lod.js DEFAULT_MIN_LEVEL, or the screen-space-error selection is not delivering it)"
                )
            rows = []
            for k, (probe, p, col) in enumerate(zip(probes, on["pts"], cols_on)):
                plat, plon, what = probe
                hit = p.get("hit")
                rx, ry, rw, rh = on["rect"]
                if col is None or not (rx + 4 <= p["x"] <= rx + rw - 4 and ry + 4 <= p["y"] <= ry + rh - 4):
                    problems.append(
                        f"{label}: {what} ({plat}, {plon}) projects to ({p['x']:.0f}, {p['y']:.0f}), outside the 3D canvas "
                        f"{[round(v) for v in on['rect']]}; choose a probe nearer the sub-point"
                    )
                    continue
                if not hit:
                    problems.append(f"{label}: the ray through the pixel of {what} ({plat}, {plon}) hit no tile at all")
                    continue
                off = _look((plat, plon), hit["lat"], hit["lon"])
                if off > HIT_TOLERANCE_DEG:
                    problems.append(
                        f"{label}: the ray through the pixel of {what} (lat {plat}, lon {plon}) hits the globe at lat {hit['lat']:.1f}, "
                        f"lon {hit['lon']:.1f}, tile {hit['tile']}: the surface drawn there is not the near side "
                        f"(the globe is drawn inside out, or placed wrongly)"
                    )
                    continue
                # The tile whose mesh is under the pixel must be the tile tileBoundsDeg puts the hit
                # point on (the hit point is within HIT_TOLERANCE_DEG of the probe, checked above).
                # (a hit exactly on a tile edge may be either neighbour: allow 0.01 degree of slack)
                wants = [_expected_tile(hit["tile"][0], hit["lat"] + da, hit["lon"] + do) for da in (-0.01, 0, 0.01) for do in (-0.01, 0, 0.01)]
                if hit["tile"] not in wants:
                    problems.append(f"{label}: {what} (lat {plat}, lon {plon}) is on tile {hit['tile']} but tileBoundsDeg puts the hit point (lat {hit['lat']:.2f}, lon {hit['lon']:.2f}) on {wants[4]}")
                if hit["src"] is not None and not hit["src"].endswith(f"/imagery/bluemarble/{hit['tile'][0]}/{hit['tile'][1]}/{hit['tile'][2]}.jpg"):
                    problems.append(f"{label}: the texture bound to tile {hit['tile']} is {hit['src']}")
                rows.append((plat, plon, col))
            problems += _verdict(ref, rows, label)
            off_ = await s.js(_GLOBE_JS % (lon0, lat0, radii, pts, json.dumps("body")))
            cols_off = await s.settled_shot([[p["x"], p["y"], 2] for p in on["pts"]], shots / f"{name}-body.png" if shots else None)
            body_label = f"plain body {tag} view lat {lat0}, lon {lon0}, {radii} R"
            if not off_["ready"] or not (off_.get("bodyTexture") or "").endswith(want_body):
                problems.append(f"{body_label}: the body's texture is {off_.get('bodyTexture')!r}, expected one ending {want_body} (ready {off_['ready']})")
                continue
            brows = []
            for probe, p, a, b in zip(probes, on["pts"], cols_on, cols_off):
                rx, ry, rw, rh = on["rect"]
                if a is None or b is None or not (rx + 4 <= p["x"] <= rx + rw - 4 and ry + 4 <= p["y"] <= ry + rh - 4):
                    continue
                plat, plon, what = probe
                brows.append((plat, plon, b))
                luminance.append((sum(a) / 3, sum(b) / 3))
                limit = GMAT_BODY_VS_GLOBE_TOLERANCE if gmat else BODY_VS_GLOBE_TOLERANCE
                if _err(a, b) > limit:
                    problems.append(
                        f"{body_label}: {what} (lat {plat}, lon {plon}) is rgb {np.round(b).astype(int).tolist()} on the body and "
                        f"{np.round(a).astype(int).tolist()} on the tiled globe (difference {_err(a, b):.0f} > {limit:.0f})"
                    )
            if gmat:
                gmat_rows += brows  # judged once, over every view: a re-graded picture only has to be the right way up
            else:
                problems += _verdict(ref, brows, body_label, tolerance=GLOBE_TOLERANCE, margin=MARGIN)
    if gmat_rows:
        problems += _verdict(ref, gmat_rows, f"plain body {tag}, all views (GMAT's re-graded texture)", tolerance=None, margin=GMAT_MARGIN)
    if len(luminance) >= 8:
        r = float(np.corrcoef(np.array(luminance).T)[0, 1])
        print(f"\nbrightness of the plain body against the tiled globe over {len(luminance)} probes: Pearson r = {r:.4f}")
        need = BODY_GLOBE_CORRELATION_GMAT if gmat else BODY_GLOBE_CORRELATION
        if r < need:
            problems.append(f"{tag}: the plain body and the tiled globe disagree about where Earth is light and dark (Pearson r {r:.3f} < {need})")
    else:
        problems.append(f"{tag}: only {len(luminance)} probes had both a body and a globe reading")
    return problems + _session_problems(s), levels_used


def test_the_tiled_globe_and_the_plain_body_draw_the_original_at_the_right_places(chrome_path, server_gmat, reference):
    """GMAT present on this host (its Earth texture is the plain body's)."""
    if (server_gmat.health.get("earthTexture") or {}).get("publishes") != "gmat":
        pytest.skip("GMAT's Earth texture is not present on this host; the second test covers the pyramid's body image")
    shots = Path(os.environ["AV_BASEMAP_SHOTS"]) if os.environ.get("AV_BASEMAP_SHOTS") else None
    problems, levels = asyncio.run(_globe_proof(chrome_path, server_gmat, reference, GLOBE_VIEWS, shots, "gmat", True))
    print("\nlevels selected per view:", json.dumps(levels, sort_keys=True))
    assert max((int(k) for v in levels.values() for k in v), default=-1) >= 2, f"no view selected a level deeper than 1; the deeper addressing is untested: {levels}"
    assert not problems, "the tiled globe or the plain body does not draw the original where it belongs:\n  " + "\n  ".join(problems)


def test_without_gmat_the_body_uses_the_pyramids_whole_earth_image_and_agrees_with_the_globe(chrome_path, server_no_gmat, reference):
    """`--textures <empty dir>`: Earth is published with ``/imagery/bluemarble/body/earth-2048x1024.jpg``."""
    views = [GLOBE_VIEWS[0], GLOBE_VIEWS[3], GLOBE_VIEWS[6]]
    shots = Path(os.environ["AV_BASEMAP_SHOTS"]) if os.environ.get("AV_BASEMAP_SHOTS") else None
    problems, levels = asyncio.run(_globe_proof(chrome_path, server_no_gmat, reference, views, shots, "nogmat", False))
    assert not problems, "without GMAT the body or the globe does not draw the original where it belongs:\n  " + "\n  ".join(problems)
