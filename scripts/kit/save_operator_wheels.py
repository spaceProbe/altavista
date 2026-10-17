"""scripts/kit/save_operator_wheels.py -- question 250(a): the operator's side of the operator-supplied
Python wheels (a `python-wheel` component of `scripts/kit/operator_supplied.json`).

A kit does not carry a wheel that bundles GPL-family code (`wheel_libs.py`: numpy's linux/aarch64
wheel bundles libgfortran); the installer asks the operator for it (`install.sh ... --operator-wheels
DIR`) and accepts only the file whose sha256 is the pin's. This helper is what an operator with a
connected machine runs to produce DIR for the disconnected one:

    .venv/bin/python scripts/kit/save_operator_wheels.py --out DIR

For every `python-wheel` pin it runs `pip download --no-deps --only-binary=:all:` for exactly that
distribution and version and for the platform, Python and ABI tags the pinned file name carries, writes
the file to `DIR`, and refuses unless the file pip produced has the pinned name and the pinned sha256
(nothing else is left in `DIR`). `DIR/SHA256SUMS` lists each wheel in `sha256sum` format, to be carried
with them. This is a network use, on the connected machine, once (pip against PyPI, default headers);
the disconnected install uses none. Standard library only; the external program is `pip`.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Callable, Optional

SCRIPT_DIR = Path(__file__).resolve().parent
PINS_FILE = SCRIPT_DIR / "operator_supplied.json"

_PYTHON_TAG = re.compile(r"^cp(\d)(\d+)$")


class SaveRefused(RuntimeError):
    """A typed refusal; `main` prints it and exits 2."""


def wheel_pins(pins_file: Path = PINS_FILE) -> dict[str, dict]:
    """`pin id -> pin` for every `python-wheel` in the pins file."""
    doc = json.loads(pins_file.read_text(encoding="utf-8"))
    return {cid: c for cid, c in sorted(doc["components"].items()) if c.get("kind") == "python-wheel"}


def pip_arguments(pin: dict) -> list[str]:
    """The `pip download` arguments that select exactly the pinned file: the distribution and version, and
    the python/abi/platform tags of the pinned file name (each dot-separated platform tag is its own
    `--platform`). Refused for a file name that is not a CPython wheel."""
    parts = pin["filename"][: -len(".whl")].split("-")
    if len(parts) not in (5, 6):
        raise SaveRefused(f"{pin['filename']!r} is not a wheel file name")
    python_tag, abi_tag, platform_tags = parts[-3], parts[-2], parts[-1]
    m = _PYTHON_TAG.match(python_tag)
    if m is None:
        raise SaveRefused(f"{pin['filename']!r}: the python tag {python_tag!r} is not a CPython tag (cpXYZ)")
    args = ["--no-deps", "--only-binary=:all:", "--python-version", f"{m.group(1)}.{m.group(2)}",
            "--implementation", "cp", "--abi", abi_tag]
    for tag in platform_tags.split("."):
        args += ["--platform", tag]
    return args + [f"{pin['dist']}=={pin['version']}"]


def _sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _pip_download(arguments: list[str], directory: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, "-m", "pip", "download", *arguments, "-d", str(directory)],
        capture_output=True, text=True, timeout=600,
    )


def save_wheels(
    out_dir: Path, pins: dict[str, dict], *,
    download: Callable[[list[str], Path], subprocess.CompletedProcess] = _pip_download,
) -> dict[str, dict]:
    """Writes `out_dir/<filename>` for each pin and `out_dir/SHA256SUMS`; returns
    `pin id -> {file, sha256, size}`. `out_dir` must hold no SHA256SUMS or pinned file already."""
    if not pins:
        raise SaveRefused("the pins file lists no python-wheel component")
    out_dir.mkdir(parents=True, exist_ok=True)
    sums = out_dir / "SHA256SUMS"
    existing = [p for p in (sums, *(out_dir / pin["filename"] for pin in pins.values())) if p.exists()]
    if existing:
        raise SaveRefused(f"{out_dir} already holds {[p.name for p in existing]}; refusing to overwrite -- use an empty directory")
    results: dict[str, dict] = {}
    written: list[Path] = []
    try:
        for cid, pin in pins.items():
            staging = out_dir / f".{cid}.partial"
            staging.mkdir()
            try:
                done = download(pip_arguments(pin), staging)
                got = sorted(p for p in staging.iterdir() if p.is_file())
                if done.returncode != 0 or len(got) != 1:
                    raise SaveRefused(
                        f"{cid}: `pip download {' '.join(pip_arguments(pin))}` failed or produced {[p.name for p in got]} "
                        f"(rc={done.returncode}): {(done.stderr or done.stdout).strip()[-400:]}"
                    )
                if got[0].name != pin["filename"]:
                    raise SaveRefused(f"{cid}: pip produced {got[0].name!r}, not the pinned {pin['filename']!r}")
                digest = _sha256_file(got[0])
                if "sha256:" + digest != pin["sha256"]:
                    raise SaveRefused(f"{cid}: {got[0].name} has sha256:{digest}, not the pinned {pin['sha256']}")
                target = out_dir / pin["filename"]
                got[0].replace(target)
                written.append(target)
                results[cid] = {"file": target.name, "sha256": digest, "size": target.stat().st_size}
            finally:
                for p in staging.iterdir():
                    p.unlink()
                staging.rmdir()
        sums.write_text("".join(f"{r['sha256']}  {r['file']}\n" for r in results.values()), encoding="utf-8")
    except BaseException:
        for p in written:
            p.unlink(missing_ok=True)
        raise
    return results


def verify_sums(directory: Path) -> dict[str, str]:
    """Re-checks `directory/SHA256SUMS` against the files; returns `file -> sha256`. Raises `SaveRefused`
    on any difference or missing file."""
    sums = directory / "SHA256SUMS"
    if not sums.is_file():
        raise SaveRefused(f"{sums} is missing")
    out = {}
    for line in sums.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        digest, _, name = line.partition("  ")
        target = directory / name
        if not target.is_file():
            raise SaveRefused(f"{target} is listed in SHA256SUMS but missing")
        got = _sha256_file(target)
        if got != digest:
            raise SaveRefused(f"{target}: sha256 {got} != listed {digest}")
        out[name] = got
    if not out:
        raise SaveRefused(f"{sums} lists no file")
    return out


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", type=Path, required=True, help="an empty (or new) directory for the wheels and SHA256SUMS")
    p.add_argument("--pins", type=Path, default=PINS_FILE, help="the pins file (default: operator_supplied.json beside this script)")
    args = p.parse_args(argv)
    try:
        results = save_wheels(args.out, wheel_pins(args.pins))
    except SaveRefused as e:
        print(f"save_operator_wheels.py: REFUSED -- {e}", file=sys.stderr)
        return 2
    for cid, r in results.items():
        print(f"{cid}: {r['file']} {r['size']} bytes sha256 {r['sha256']}")
    print(f"wrote {args.out / 'SHA256SUMS'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
