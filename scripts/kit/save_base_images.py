"""scripts/kit/save_base_images.py -- question 250(a), second half: the operator's side of the
operator-supplied base images.

A kit carries no base-image layers (`docs/kit-operator-supplied.md` items 1 and 2); the installer's
image step (`installer/install.sh --build-images`) builds the application layer on a base the
OPERATOR already loaded on the Docker host. This helper is what an operator with a connected
machine runs to produce those bases for the disconnected one:

    .venv/bin/python scripts/kit/save_base_images.py --out DIR

For every `container-base-image` in `scripts/kit/operator_supplied.json` it runs
`docker save -o DIR/<pin id>.tar <name>:<tag>@sha256:<digest>`, i.e. the image is selected BY
DIGEST and never by a floating tag, and then reads the tarball's own `index.json` to confirm it
names exactly that digest. The saved tarball carries no repository name (measured:
`manifest.json` has `"RepoTags": null`), so loading it with `docker load -i` names nothing on the
target host; the installer's image step finds the base by the digest-qualified reference its
Dockerfile starts `FROM`. `DIR/SHA256SUMS` lists each tarball's sha256 in `sha256sum` format, to be
carried with them and re-checked before loading.

The base must already be on the connected machine under that digest (a pull is the operator's own
act, not this script's: it never pulls). A missing image is a typed refusal (exit 2) naming the
reference. Standard library only; the only external program is `docker`.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import Callable, Optional

SCRIPT_DIR = Path(__file__).resolve().parent
PINS_FILE = SCRIPT_DIR / "operator_supplied.json"


class SaveRefused(RuntimeError):
    """A typed refusal; `main` prints it and exits 2."""


def base_pins(pins_file: Path = PINS_FILE) -> dict[str, dict]:
    """`pin id -> {reference, digest}` for every `container-base-image` in the pins file."""
    doc = json.loads(pins_file.read_text(encoding="utf-8"))
    pins = {}
    for pin_id, c in sorted(doc["components"].items()):
        if c.get("kind") != "container-base-image":
            continue
        ref, digest = c["reference"], c["digest"]
        if not ref.endswith("@" + digest):
            raise SaveRefused(f"{pin_id}: reference {ref!r} does not end with @{digest} -- refusing a base not pinned by digest")
        pins[pin_id] = {"reference": ref, "digest": digest}
    if not pins:
        raise SaveRefused(f"{pins_file} lists no container-base-image")
    return pins


def tarball_image_digests(tarball: Path) -> list[str]:
    with tarfile.open(tarball) as tf:
        index = json.load(tf.extractfile("index.json"))
    return sorted(m["digest"] for m in index.get("manifests", []))


def _sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _docker_save(reference: str, out_path: Path) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", "save", "-o", str(out_path), reference], capture_output=True, text=True, timeout=600)


def save_bases(
    out_dir: Path, pins: dict[str, dict], *, save: Callable[[str, Path], subprocess.CompletedProcess] = _docker_save,
) -> dict[str, dict]:
    """Writes `out_dir/<pin id>.tar` for each pin and `out_dir/SHA256SUMS`; returns
    `pin id -> {file, reference, digest, sha256, size}`. `out_dir` must not hold a SHA256SUMS or
    any of the tarballs already (nothing is overwritten)."""
    out_dir.mkdir(parents=True, exist_ok=True)
    sums = out_dir / "SHA256SUMS"
    targets = {pin_id: out_dir / f"{pin_id}.tar" for pin_id in pins}
    existing = [p for p in (sums, *targets.values()) if p.exists()]
    if existing:
        raise SaveRefused(f"{out_dir} already holds {[p.name for p in existing]}; refusing to overwrite -- use an empty directory")
    results: dict[str, dict] = {}
    written: list[Path] = []
    try:
        for pin_id, pin in pins.items():
            partial = targets[pin_id].with_name(targets[pin_id].name + ".partial")
            done = save(pin["reference"], partial)
            if done.returncode != 0:
                partial.unlink(missing_ok=True)
                raise SaveRefused(
                    f"{pin_id}: `docker save {pin['reference']}` failed (rc={done.returncode}): {done.stderr.strip()} -- the base "
                    f"must already be on this machine under exactly that digest; this script never pulls"
                )
            digests = tarball_image_digests(partial)
            if digests != [pin["digest"]]:
                partial.unlink(missing_ok=True)
                raise SaveRefused(f"{pin_id}: the saved tarball names {digests}, expected exactly [{pin['digest']}]")
            os.replace(partial, targets[pin_id])
            written.append(targets[pin_id])
            results[pin_id] = {
                "file": targets[pin_id].name, "reference": pin["reference"], "digest": pin["digest"],
                "sha256": _sha256_file(targets[pin_id]), "size": targets[pin_id].stat().st_size,
            }
        sums.write_text("".join(f"{r['sha256']}  {r['file']}\n" for r in results.values()), encoding="utf-8")
    except BaseException:
        for p in written:
            p.unlink(missing_ok=True)
        raise
    return results


def verify_sums(directory: Path) -> dict[str, str]:
    """Re-checks `directory/SHA256SUMS` against the files; returns `file -> sha256`. Raises
    `SaveRefused` on any difference or missing file."""
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
    p.add_argument("--out", type=Path, required=True, help="an empty (or new) directory for the tarballs and SHA256SUMS")
    p.add_argument("--pins", type=Path, default=PINS_FILE, help="the pins file (default: operator_supplied.json beside this script)")
    args = p.parse_args(argv)
    try:
        results = save_bases(args.out, base_pins(args.pins))
    except SaveRefused as e:
        print(f"save_base_images.py: REFUSED -- {e}", file=sys.stderr)
        return 2
    except FileNotFoundError as e:
        print(f"save_base_images.py: REFUSED -- {e.filename or 'docker'} not found: docker CLI missing?", file=sys.stderr)
        return 2
    for pin_id, r in results.items():
        print(f"{pin_id}: {r['file']} {r['size']} bytes sha256 {r['sha256']} ({r['reference']})")
    print(f"wrote {args.out / 'SHA256SUMS'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
