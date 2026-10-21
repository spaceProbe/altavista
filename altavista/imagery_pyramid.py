"""The Blue Marble imagery pyramid on disk, and whether the server may rely on it (question 264).

``scripts/imagery/build_blue_marble_tiles.py`` turns the one NASA original into a directory::

    <dir>/manifest.json                 format, max_level, tile_count, files{path: {sha256, bytes}}, ...
    <dir>/{z}/{x}/{y}.jpg               the quadtree the globe and the 2D map read
    <dir>/body/earth-2048x1024.jpg      the whole-Earth image for the plain body (no GMAT needed)

``altavista.server`` serves that directory at ``/imagery/bluemarble/`` and every shipped profile's
``imagery:`` points at it. The pyramid is a large, fetched-once artefact that is not in the repository,
so the server must say plainly when it is not there instead of letting the viewer request thousands of
tiles that all 404 (or quietly drawing the test colours): ``check_pyramid`` is that verdict, taken once
at start-up, and ``PyramidStatus.notice`` is the sentence the viewer shows when it is negative.

The check is deliberately cheap. It reads ``manifest.json``, checks its ``format`` and shape, and
``stat``s every listed file for existence and byte size. It does **not** hash them: re-hashing a
couple of thousand tiles at every start-up is the generator's job, ``build_blue_marble_tiles.py
--verify DIR``. A pyramid that passes here is complete and not truncated; only ``--verify`` says it is
bit-identical to what was built.
"""
from __future__ import annotations

import json
import os
import stat
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Optional

#: The ``format`` the generator writes into ``manifest.json``; anything else is not this pyramid.
MANIFEST_FORMAT = "altavista-imagery-pyramid/1"

#: Where the server mounts the directory, and so where every shipped profile's ``url_template`` points.
URL_PREFIX = "/imagery/bluemarble/"

#: The whole-Earth image inside the pyramid, relative to its directory, and its served URL path.
BODY_FILE = "body/earth-2048x1024.jpg"
BODY_URL = URL_PREFIX + BODY_FILE

BUILD_COMMAND = ".venv/bin/python scripts/imagery/build_blue_marble_tiles.py --original <the original> --out {dir}"


def default_imagery_dir() -> Path:
    """``~/altavista-data/imagery/bluemarble``: nothing host-specific, overridable with
    ``--imagery-dir`` / ``create_app(imagery_dir=...)`` (never the process environment, question 199)."""
    return Path.home() / "altavista-data" / "imagery" / "bluemarble"


def template_uses_pyramid(url_template: object) -> bool:
    """True when a profile's ``url_template`` points at the served pyramid (``./imagery/bluemarble/...``
    or ``/imagery/bluemarble/...``); only such a template is judged by the pyramid's verdict. A
    template for the offline test fixture, or for any other source, is not."""
    if not isinstance(url_template, str):
        return False
    path = url_template[2:] if url_template.startswith("./") else url_template
    return ("/" + path.lstrip("/")).startswith(URL_PREFIX)


@dataclass(frozen=True)
class PyramidStatus:
    """The start-up verdict on one directory. ``installed`` is True only for a complete pyramid."""

    directory: Path
    installed: bool
    #: Why it is not installed (empty when it is).
    reason: str = ""
    max_level: Optional[int] = None
    tile_count: Optional[int] = None
    pyramid_sha256: Optional[str] = None
    #: Whether ``BODY_FILE`` is a listed (and, since installed, present) file of the pyramid.
    has_body_texture: bool = False

    @property
    def notice(self) -> str:
        """One sentence for a person: what is missing, where, and how to build it. Empty when installed."""
        if self.installed:
            return ""
        return (f"Blue Marble basemap not installed: {self.reason}. Expected in {self.directory}. "
                f"Build it once with: {BUILD_COMMAND.format(dir=self.directory)} "
                f"(then restart the server).")

    def health(self) -> dict:
        """The ``imagery`` entry of ``GET /api/health``."""
        out: dict = {"installed": self.installed, "directory": str(self.directory)}
        if self.installed:
            out.update(maxLevel=self.max_level, tileCount=self.tile_count, pyramidSha256=self.pyramid_sha256)
        else:
            out["reason"] = self.reason
        return out

    def summary(self) -> str:
        """A short phrase for the start-up log line."""
        if self.installed:
            return f"{self.directory} (installed, max_level {self.max_level}, {self.tile_count} tiles)"
        return f"{self.directory} (NOT INSTALLED: {self.reason})"


def _unsafe_relative_path(rel: object) -> bool:
    """A manifest key must be a plain relative POSIX path with no ``..`` or empty components."""
    if not isinstance(rel, str) or not rel or "\\" in rel or "\x00" in rel:
        return True
    p = PurePosixPath(rel)
    return p.is_absolute() or any(part in ("", ".", "..") for part in rel.split("/"))


def check_pyramid(directory: os.PathLike) -> PyramidStatus:
    """Judge ``directory`` (see the module docstring). Never raises: every failure is a
    ``PyramidStatus`` with ``installed=False`` and a ``reason``."""
    root = Path(directory)

    def no(reason: str) -> PyramidStatus:
        return PyramidStatus(directory=root, installed=False, reason=reason)

    if not root.is_dir():
        return no("the directory does not exist")
    manifest_path = root / "manifest.json"
    try:
        raw = manifest_path.read_bytes()
    except FileNotFoundError:
        return no("manifest.json is missing")
    except OSError as exc:
        return no(f"manifest.json is unreadable ({exc.strerror or exc})")
    try:
        manifest = json.loads(raw)
    except (ValueError, UnicodeDecodeError) as exc:
        return no(f"manifest.json is not valid JSON ({exc})")
    if not isinstance(manifest, dict):
        return no("manifest.json is not a JSON object")
    if manifest.get("format") != MANIFEST_FORMAT:
        return no(f"manifest.json format is {manifest.get('format')!r}, expected {MANIFEST_FORMAT!r}")
    max_level, tile_count, digest = manifest.get("max_level"), manifest.get("tile_count"), manifest.get("pyramid_sha256")
    if isinstance(max_level, bool) or not isinstance(max_level, int) or max_level < 0:
        return no("manifest.json has no usable max_level")
    if isinstance(tile_count, bool) or not isinstance(tile_count, int) or tile_count < 0:
        return no("manifest.json has no usable tile_count")
    if not isinstance(digest, str) or not digest:
        return no("manifest.json has no pyramid_sha256")
    files = manifest.get("files")
    if not isinstance(files, dict) or not files:
        return no("manifest.json lists no files")
    for rel, entry in files.items():
        if _unsafe_relative_path(rel):
            return no(f"manifest.json lists an unsafe path {rel!r}")
        size = entry.get("bytes") if isinstance(entry, dict) else None
        if isinstance(size, bool) or not isinstance(size, int):
            return no(f"manifest.json gives no byte size for {rel}")
        try:
            st = (root / rel).stat()
        except OSError:
            return no(f"listed file {rel} is missing")
        if not stat.S_ISREG(st.st_mode):
            return no(f"listed file {rel} is not a regular file")
        actual = st.st_size
        if actual != size:
            return no(f"listed file {rel} is {actual} bytes, the manifest says {size}")
    return PyramidStatus(directory=root, installed=True, max_level=max_level, tile_count=tile_count,
                         pyramid_sha256=digest, has_body_texture=BODY_FILE in files)
