#!/usr/bin/env python3
"""Build the Blue Marble tile pyramid (question 264) from NASA's whole-world plate carree JPEG.

What it produces
----------------
A static, offline raster pyramid for the tiled globe (``web/js/globe.js``) and the 2D map
(``web/js/panels/map_panel.js``), plus one whole-Earth image for the plain Earth body when GMAT
is absent::

    {out}/{z}/{x}/{y}.jpg          256x256 JPEG tiles, levels 0 .. max_level
    {out}/body/earth-2048x1024.jpg the level-2 whole plate (2048x1024, the size of GMAT's own
                                   ModifiedBlueMarble.jpg)
    {out}/manifest.json            what was built, from what, with which tools, and every hash

Tile scheme (the reference is ``web/js/globe_lod.js`` ``tileBoundsDeg`` / ``tileCountX/Y``)
-------------------------------------------------------------------------------------------
Geographic plate carree. Level L has ``2**(L+1)`` columns x ``2**L`` rows over 360x180 degrees.
Tile ``(L, x, y)`` covers longitude ``[-180 + x*360/nx, +360/nx)`` and latitude
``[-90 + y*180/ny, +180/ny)``, so **tile y = 0 is the row at the south pole**. Each tile *image*
is **north-up** (pixel row 0 is the tile's northern edge), which is how the 2D map lays tile
images out. In the level-L whole-world plate (row 0 = north, column 0 = 180 W; width
``256 * 2**(L+1)``) tile ``(L, x, y)`` is the 256x256 block at plate rows ``(ny-1-y)*256 ...`` and
columns ``x*256 ...``. The source is 21600x10800 (row 0 = 90 N, column 0 = 180 W), so the deepest
level that needs no upsampling is the largest L with ``256 * 2**(L+1) <= width``: level 5 at
16384x8192 (computed from the width, never hard-coded).

Determinism, and what is and is not claimed
-------------------------------------------
* The resampling arithmetic is designed to be host-independent: given the same decoded plate it
  yields the same pixel arrays on any host. The deepest level is an area-weighted (box-filter)
  separable resample. Each output pixel covers an interval of the source; the weight of source
  pixel i is the exact overlap length (integer arithmetic, scaled by the output size) divided by
  the source size, one correctly rounded float64 division. The weights are applied by
  elementwise numpy multiply-adds in a fixed tap order (horizontal pass, then vertical pass);
  there is no ``@``, ``dot``, ``einsum`` or other BLAS/reduction whose summation order can change
  with the thread count. The result is rounded with ``np.rint`` (half to even) and clipped to
  uint8. Work is done in row bands to bound memory; every pixel is computed independently of
  the band size (``tests/test_build_blue_marble_tiles.py`` checks that). Each shallower level is
  the exact integer 2x2 mean ``(a+b+c+d+2)//4`` of the level below.
* The JPEG *bytes* are NOT claimed identical across platforms or encoder versions: they depend
  on the libjpeg build that decodes the original and encodes the tiles. Tiles are encoded by
  piping a binary PPM to ``cjpeg`` with the fixed argument list ``CJPEG_ARGS`` (no comments, no
  timestamps). The manifest records ``djpeg``'s and ``cjpeg``'s exact version lines and that full
  argument list, and the ``pyramid_sha256`` it carries holds *for libjpeg-turbo 3.2.0 with these
  options* (the build this was produced with) and for nothing else. The proof offered is that two
  builds on one host are byte-equal (``--verify`` re-hashes an installed pyramid).

Memory
------
The decoded 21600x10800 RGB plate is about 700 MB; the 16384x8192 level-5 plate is about
400 MB while the original is still held; row bands add tens of MB. Measured peak (macOS arm64,
``/usr/bin/time -l``, two real builds): maximum resident set size about 1.54 GB (1536 to
1547 MB), peak memory footprint about 1.52 to 1.53 GB, 23 s wall time. Plan for roughly 1.6 GB
free memory for the real original; smaller plates need proportionally less.

Tools (no new dependency; the manager's decision)
-------------------------------------------------
The JPEG decode and encode use the host's libjpeg-turbo command-line tools, ``djpeg`` and
``cjpeg`` (BSD-3-Clause / IJG / zlib licences; on Linux they are packaged as
``libjpeg-turbo-progs`` or ``libjpeg-turbo-utils``), found on ``PATH`` or named with
``--djpeg`` / ``--cjpeg``, plus numpy (already in ``.venv``). They are run through ``subprocess``
with argument lists, never a shell. The script refuses to run without them.

How to run
----------
::

    .venv/bin/python -I scripts/imagery/build_blue_marble_tiles.py \\
        --original ~/altavista-data/imagery/original/world.topo.bathy.200407.3x21600x10800.jpg
    .venv/bin/python -I scripts/imagery/build_blue_marble_tiles.py --verify ~/altavista-data/imagery/bluemarble

* ``--original PATH`` (required to build): the source JPEG. Its sha256 is checked against
  ``--sha256`` (default: the recorded hash of NASA's file, ``EXPECTED_SHA256``) **before it is
  decoded**; a mismatch is an error naming both hashes. The original is untrusted downloaded
  data: it is only ever streamed to ``djpeg`` on standard input.
* ``--out DIR`` (default ``~/altavista-data/imagery/bluemarble``, from ``Path.home()``): the
  pyramid is built into ``{out}.partial-<pid>`` and renamed to ``{out}`` only when complete, so a
  half-built pyramid is never seen as installed. An existing ``{out}`` is refused unless
  ``--force``, which replaces it only after the new build has succeeded.
* ``--verify DIR``: re-hash an existing pyramid against its manifest (every listed file present
  with the listed size and sha256, no unlisted file, the expected tile set for ``max_level``,
  ``pyramid_sha256`` recomputed); exits non-zero on any problem.

Manifest ``tile_count`` counts the tiles only; ``files`` and ``total_bytes`` include the
``body/`` image. ``pyramid_sha256`` is the sha256 of the UTF-8 lines ``"<path> <sha256>\\n"`` for
every entry of ``files``, sorted by path.

Output is data and is never committed: it lives under ``~/altavista-data/imagery/`` (outside the
repository). Credit NASA asks for: "NASA Earth Observatory".
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

EXPECTED_SHA256 = "d225f1f35a6448a4d1d8f6de6e48f3433e470085b70a35800e64f384f269a7b0"
ORIGINAL_URL = (
    "https://assets.science.nasa.gov/content/dam/science/esd/eo/images/bmng/"
    "bmng-topography-bathymetry/july/world.topo.bathy.200407.3x21600x10800.jpg"
)
TILE_PX = 256
MIN_LEVEL = 0
BODY_LEVEL = 2
BODY_FILE = "body/earth-2048x1024.jpg"
FORMAT = "altavista-imagery-pyramid/1"
SCHEME = (
    "geographic plate carree; level L has 2**(L+1) x 2**L tiles; tile y=0 at the south pole "
    "(web/js/globe_lod.js tileBoundsDeg); tile images north-up"
)
ATTRIBUTION = (
    "Blue Marble: Next Generation (July 2004, topography and bathymetry), "
    "credit NASA Earth Observatory"
)
CJPEG_ARGS = ["-quality", "88", "-optimize", "-dct", "int", "-sample", "2x2"]
DEFAULT_BAND_ROWS = 64


class BuildError(RuntimeError):
    """A refusal or failure with a message meant for the person running the script."""


# --------------------------------------------------------------------------------------------
# tools


def find_tool(name: str, explicit: str | None) -> str:
    path = explicit if explicit else shutil.which(name)
    if not path or not os.path.isfile(path) or not os.access(path, os.X_OK):
        raise BuildError(
            f"{name} not found ({'--' + name + ' ' + explicit if explicit else 'not on PATH'}); "
            "install libjpeg-turbo's command-line tools (macOS: brew install jpeg-turbo; "
            "Debian/Ubuntu: libjpeg-turbo-progs; Alpine/Fedora: libjpeg-turbo-utils)"
        )
    return path


def tool_version(path: str) -> str:
    """The first non-empty line of ``<tool> -version`` (libjpeg-turbo prints it on stderr)."""
    r = subprocess.run([path, "-version"], capture_output=True, text=True, check=False)
    for line in (r.stdout + "\n" + r.stderr).splitlines():
        if line.strip():
            return line.strip()
    raise BuildError(f"{path} -version printed nothing")


# --------------------------------------------------------------------------------------------
# hashing


def sha256_file(path: Path) -> tuple[str, int]:
    h = hashlib.sha256()
    n = 0
    with open(path, "rb") as f:
        while True:
            chunk = f.read(1 << 20)
            if not chunk:
                break
            h.update(chunk)
            n += len(chunk)
    return h.hexdigest(), n


def pyramid_digest(files: dict[str, dict]) -> str:
    h = hashlib.sha256()
    for rel in sorted(files):
        h.update(f"{rel} {files[rel]['sha256']}\n".encode("utf-8"))
    return h.hexdigest()


# --------------------------------------------------------------------------------------------
# decode


def _read_token(stream) -> bytes:
    """One whitespace-delimited PNM header token, skipping ``#`` comments."""
    tok = b""
    while True:
        c = stream.read(1)
        if not c:
            raise BuildError("djpeg output ended inside the PPM header")
        if c == b"#":
            while c not in (b"\n", b""):
                c = stream.read(1)
            continue
        if c.isspace():
            if tok:
                return tok
            continue
        tok += c


def decode_ppm(djpeg: str, jpeg_path: Path) -> np.ndarray:
    """Decode with ``djpeg -pnm`` (the JPEG on stdin, the PPM on stdout) into (h, w, 3) uint8."""
    with open(jpeg_path, "rb") as fin, tempfile.TemporaryFile() as err:
        proc = subprocess.Popen(
            [djpeg, "-pnm"], stdin=fin, stdout=subprocess.PIPE, stderr=err
        )
        try:
            if _read_token(proc.stdout) != b"P6":
                raise BuildError("djpeg did not produce a binary PPM (P6); is the JPEG greyscale?")
            w = int(_read_token(proc.stdout))
            h = int(_read_token(proc.stdout))
            maxval = int(_read_token(proc.stdout))
            if maxval != 255:
                raise BuildError(f"unexpected PPM maxval {maxval}")
            arr = np.empty((h, w, 3), dtype=np.uint8)
            view = memoryview(arr).cast("B")
            got = 0
            while got < len(view):
                n = proc.stdout.readinto(view[got:])
                if not n:
                    break
                got += n
            if got != len(view):
                raise BuildError(f"djpeg produced {got} of {len(view)} pixel bytes")
        finally:
            proc.stdout.close()
            rc = proc.wait()
        if rc != 0:
            err.seek(0)
            raise BuildError(f"djpeg failed (exit {rc}): {err.read().decode('utf-8', 'replace')}")
    return arr


# --------------------------------------------------------------------------------------------
# deterministic resampling


def max_level_for_width(width: int) -> int:
    """The deepest level with ``256 * 2**(L+1) <= width`` (no upsampling); -1 if none."""
    level = -1
    while TILE_PX * 2 ** (level + 2) <= width:
        level += 1
    return level


def box_taps(n_in: int, n_out: int) -> tuple[np.ndarray, np.ndarray]:
    """Box-filter taps shrinking ``n_in`` source pixels to ``n_out`` (n_out <= n_in).

    Source pixel i spans [i*n_out, (i+1)*n_out) and output pixel j spans [j*n_in, (j+1)*n_in) in
    one shared integer unit, so the overlaps are exact integers and each weight is one correctly
    rounded float64 division. Returns ``(idx, w)``, both ``(taps, n_out)``; missing taps are
    padded with the last index and weight 0.0.
    """
    rows: list[list[tuple[int, float]]] = []
    for j in range(n_out):
        start, end = j * n_in, (j + 1) * n_in
        first, last = start // n_out, (end - 1) // n_out
        taps = []
        for i in range(first, last + 1):
            overlap = min(end, (i + 1) * n_out) - max(start, i * n_out)
            taps.append((i, overlap / n_in))
        rows.append(taps)
    k = max(len(t) for t in rows)
    idx = np.empty((k, n_out), dtype=np.int64)
    w = np.zeros((k, n_out), dtype=np.float64)
    for j, taps in enumerate(rows):
        for t in range(k):
            if t < len(taps):
                idx[t, j], w[t, j] = taps[t]
            else:
                idx[t, j] = taps[-1][0]
    return idx, w


def box_resample(src: np.ndarray, out_w: int, out_h: int, band_rows: int = DEFAULT_BAND_ROWS) -> np.ndarray:
    """Area-weighted separable downsample of (h, w, 3) uint8 to (out_h, out_w, 3) uint8.

    Elementwise multiply-adds in a fixed tap order only (no dot/matmul/einsum/sum reductions).
    """
    in_h, in_w, _ = src.shape
    if out_w > in_w or out_h > in_h:
        raise BuildError("refusing to upsample")
    hidx, hw = box_taps(in_w, out_w)
    vidx, vw = box_taps(in_h, out_h)
    out = np.empty((out_h, out_w, 3), dtype=np.uint8)
    for r0 in range(0, out_h, band_rows):
        r1 = min(r0 + band_rows, out_h)
        lo = int(vidx[:, r0:r1].min())
        hi = int(vidx[:, r0:r1].max()) + 1
        rows = src[lo:hi]
        # horizontal pass: (hi-lo, out_w, 3) float64
        hacc = rows[:, hidx[0]].astype(np.float64)
        hacc *= hw[0][None, :, None]
        for t in range(1, hidx.shape[0]):
            term = rows[:, hidx[t]].astype(np.float64)
            term *= hw[t][None, :, None]
            hacc += term
        # vertical pass
        acc = hacc[vidx[0, r0:r1] - lo] * vw[0, r0:r1][:, None, None]
        for t in range(1, vidx.shape[0]):
            term = hacc[vidx[t, r0:r1] - lo] * vw[t, r0:r1][:, None, None]
            acc += term
        np.rint(acc, out=acc)
        np.clip(acc, 0.0, 255.0, out=acc)
        out[r0:r1] = acc.astype(np.uint8)
    return out


def halve(src: np.ndarray, band_rows: int = 512) -> np.ndarray:
    """Exact integer 2x2 mean ``(a+b+c+d+2)//4`` of an even-sized (h, w, 3) uint8 image."""
    h, w, _ = src.shape
    out = np.empty((h // 2, w // 2, 3), dtype=np.uint8)
    for r0 in range(0, h, band_rows):
        r1 = min(r0 + band_rows, h)
        band = src[r0:r1]
        s = band[0::2, 0::2].astype(np.uint16)
        s += band[0::2, 1::2]
        s += band[1::2, 0::2]
        s += band[1::2, 1::2]
        s += 2
        s //= 4
        out[r0 // 2 : r1 // 2] = s.astype(np.uint8)
    return out


# --------------------------------------------------------------------------------------------
# encode and tile


def encode_jpeg(cjpeg: str, rgb: np.ndarray) -> bytes:
    h, w, _ = rgb.shape
    ppm = b"P6\n%d %d\n255\n" % (w, h) + np.ascontiguousarray(rgb).tobytes()
    r = subprocess.run([cjpeg, *CJPEG_ARGS], input=ppm, capture_output=True, check=False)
    if r.returncode != 0 or not r.stdout:
        raise BuildError(f"cjpeg failed (exit {r.returncode}): {r.stderr.decode('utf-8', 'replace')}")
    return r.stdout


def write_file(root: Path, rel: str, data: bytes, files: dict[str, dict]) -> None:
    dest = root / rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(data)
    files[rel] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}


def write_level_tiles(cjpeg: str, root: Path, level: int, plate: np.ndarray, files: dict[str, dict]) -> int:
    nx, ny = 2 ** (level + 1), 2**level
    if plate.shape[:2] != (ny * TILE_PX, nx * TILE_PX):
        raise BuildError(f"level {level} plate is {plate.shape}, expected {ny * TILE_PX}x{nx * TILE_PX}")
    count = 0
    for x in range(nx):
        for y in range(ny):  # y = 0 is the south row, the last row of the north-up plate
            r0 = (ny - 1 - y) * TILE_PX
            block = plate[r0 : r0 + TILE_PX, x * TILE_PX : (x + 1) * TILE_PX]
            write_file(root, f"{level}/{x}/{y}.jpg", encode_jpeg(cjpeg, block), files)
            count += 1
    return count


# --------------------------------------------------------------------------------------------
# build


def build(
    original: Path,
    out: Path,
    expected_sha: str,
    cjpeg: str,
    djpeg: str,
    force: bool = False,
    band_rows: int = DEFAULT_BAND_ROWS,
    log=print,
) -> dict:
    if out.exists() and not force:
        raise BuildError(f"{out} already exists; refusing to overwrite (use --force to replace it)")
    digest, size = sha256_file(original)
    if digest != expected_sha.lower():
        raise BuildError(
            f"sha256 mismatch for {original.name}: expected {expected_sha.lower()}, got {digest}; "
            "refusing to decode"
        )
    versions = {"cjpeg": tool_version(cjpeg), "djpeg": tool_version(djpeg)}
    log(f"original sha256 ok ({size} bytes); {versions['cjpeg']}")
    plate = decode_ppm(djpeg, original)
    height, width, _ = plate.shape
    if width != 2 * height:
        raise BuildError(f"expected a 2:1 plate carree, got {width}x{height}")
    max_level = max_level_for_width(width)
    if max_level < BODY_LEVEL:
        raise BuildError(f"{width}x{height} is too small: the pyramid needs at least level {BODY_LEVEL}")
    log(f"decoded {width}x{height}; levels {MIN_LEVEL}..{max_level}")

    partial = out.with_name(f"{out.name}.partial-{os.getpid()}")
    if partial.exists():
        shutil.rmtree(partial)
    partial.mkdir(parents=True)
    try:
        files: dict[str, dict] = {}
        tile_count = 0
        deep_w = TILE_PX * 2 ** (max_level + 1)
        level_img = box_resample(plate, deep_w, deep_w // 2, band_rows)
        del plate
        level = max_level
        while True:
            tile_count += write_level_tiles(cjpeg, partial, level, level_img, files)
            log(f"level {level}: {level_img.shape[1]}x{level_img.shape[0]} done")
            if level == BODY_LEVEL:
                write_file(partial, BODY_FILE, encode_jpeg(cjpeg, level_img), files)
            if level == MIN_LEVEL:
                break
            level_img = halve(level_img)
            level -= 1
        manifest = {
            "format": FORMAT,
            "scheme": SCHEME,
            "tile_px": TILE_PX,
            "min_level": MIN_LEVEL,
            "max_level": max_level,
            "image_format": "jpeg",
            "attribution": ATTRIBUTION,
            "original": {
                "file": original.name,
                "sha256": digest,
                "bytes": size,
                "width": width,
                "height": height,
                "url": ORIGINAL_URL,
            },
            "encoder": {"cjpeg": versions["cjpeg"], "djpeg": versions["djpeg"], "args": CJPEG_ARGS},
            "files": files,
            "tile_count": tile_count,
            "total_bytes": sum(f["bytes"] for f in files.values()),
            "pyramid_sha256": pyramid_digest(files),
        }
        (partial / "manifest.json").write_text(
            json.dumps(manifest, sort_keys=True, indent=1) + "\n", encoding="utf-8"
        )
        # Install: only now does {out} appear; an old one is replaced after the new one is whole.
        old = None
        if out.exists():
            old = out.with_name(f"{out.name}.old-{os.getpid()}")
            os.rename(out, old)
        out.parent.mkdir(parents=True, exist_ok=True)
        os.rename(partial, out)
        if old is not None:
            shutil.rmtree(old)
    except BaseException:
        shutil.rmtree(partial, ignore_errors=True)
        raise
    return manifest


# --------------------------------------------------------------------------------------------
# verify


def verify(root: Path, log=print) -> list[str]:
    """Problems found re-hashing the pyramid at ``root`` against its manifest (empty = good)."""
    problems: list[str] = []
    mpath = root / "manifest.json"
    if not mpath.is_file():
        return [f"{mpath} missing"]
    try:
        m = json.loads(mpath.read_text(encoding="utf-8"))
        files = m["files"]
        max_level = m["max_level"]
        min_level = m["min_level"]
    except (ValueError, KeyError) as e:
        return [f"{mpath} unreadable: {e!r}"]
    if m.get("format") != FORMAT:
        problems.append(f"manifest format {m.get('format')!r}, expected {FORMAT!r}")
    expected = {BODY_FILE}
    for level in range(min_level, max_level + 1):
        for x in range(2 ** (level + 1)):
            for y in range(2**level):
                expected.add(f"{level}/{x}/{y}.jpg")
    for rel in sorted(expected - set(files)):
        problems.append(f"manifest does not list expected file {rel}")
    for rel in sorted(set(files) - expected):
        problems.append(f"manifest lists unexpected file {rel}")
    for rel in sorted(files):
        p = root / rel
        if not p.is_file():
            problems.append(f"missing file {rel}")
            continue
        digest, size = sha256_file(p)
        if size != files[rel]["bytes"]:
            problems.append(f"{rel}: {size} bytes, manifest says {files[rel]['bytes']}")
        if digest != files[rel]["sha256"]:
            problems.append(f"{rel}: sha256 {digest}, manifest says {files[rel]['sha256']}")
    on_disk = {
        p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file() and p.name != "manifest.json"
    }
    for rel in sorted(on_disk - set(files)):
        problems.append(f"file on disk not in manifest: {rel}")
    if m.get("pyramid_sha256") != pyramid_digest(files):
        problems.append("pyramid_sha256 does not match the file list")
    if m.get("tile_count") != len(files) - 1:
        problems.append(f"tile_count {m.get('tile_count')} but {len(files) - 1} tile entries")
    if m.get("total_bytes") != sum(f["bytes"] for f in files.values()):
        problems.append("total_bytes does not match the file list")
    if not problems:
        log(
            f"verify ok: {len(files)} files, {m['tile_count']} tiles, levels {min_level}..{max_level}, "
            f"pyramid_sha256 {m['pyramid_sha256']}"
        )
    return problems


# --------------------------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Build or verify the Blue Marble tile pyramid.",
        epilog=(
            "Needs roughly 1.6 GB free memory for the 21600x10800 original (measured peak: 1.54 GB "
            "resident, ~23 s). The recorded pyramid_sha256 holds for the libjpeg-turbo version and "
            "cjpeg options in the manifest only; tile bytes are not claimed identical across "
            "encoder builds. See the module docstring."
        ),
    )
    ap.add_argument("--original", type=Path, help="the NASA world plate JPEG (required to build)")
    ap.add_argument("--sha256", default=EXPECTED_SHA256, help="expected sha256 of the original")
    ap.add_argument(
        "--out",
        type=Path,
        default=Path.home() / "altavista-data" / "imagery" / "bluemarble",
        help="output directory (default: ~/altavista-data/imagery/bluemarble)",
    )
    ap.add_argument("--force", action="store_true", help="replace an existing --out after a good build")
    ap.add_argument("--verify", type=Path, metavar="DIR", help="re-hash an existing pyramid against its manifest")
    ap.add_argument("--djpeg", help="path to djpeg (default: on PATH)")
    ap.add_argument("--cjpeg", help="path to cjpeg (default: on PATH)")
    args = ap.parse_args(argv)
    try:
        if args.verify is not None:
            problems = verify(args.verify)
            for p in problems:
                print(f"FAIL: {p}", file=sys.stderr)
            return 1 if problems else 0
        if args.original is None:
            ap.error("--original is required unless --verify is given")
        cjpeg = find_tool("cjpeg", args.cjpeg)
        djpeg = find_tool("djpeg", args.djpeg)
        m = build(args.original, args.out, args.sha256, cjpeg, djpeg, force=args.force)
        print(
            f"built {args.out}: levels {m['min_level']}..{m['max_level']}, {m['tile_count']} tiles, "
            f"{m['total_bytes']} bytes, pyramid_sha256 {m['pyramid_sha256']}"
        )
        return 0
    except BuildError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
