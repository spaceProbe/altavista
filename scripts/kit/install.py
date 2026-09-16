"""scripts/kit/install.py -- D3's second half (docs/p5-plan.md, P5 track round 2 task 3b): the
real logic behind scripts/kit/install.sh. Installs a kit (scripts/kit/build_kit.py's own output,
`kit_format` 2) into a fresh, empty target directory, using nothing beyond the kit itself, a
Python interpreter, and a shell -- no network, ever (question 154: "a kit is built with network
once and installed with none" -- this half is the "installed with none" clause, proved for real
by tests/test_kit_zero_egress_install.py).

# Why this is a separate module from install.sh

Exactly the split scripts/kit/manifest.py's own top doc argues for between the manifest format
and the builder: install.sh's job (locate a Python interpreter, exec into this file) needs no
logic of its own; every actual decision -- verify-first, refuse loudly and specifically, what
"install" means for each kit section -- belongs in one place that is importable and testable
without a subprocess, exactly like `manifest.verify_manifest` is reused here rather than
reimplemented (this task's own hard rule: "Reuse manifest.verify_manifest; do not reimplement
it").

# What "install" means, section by section

- **Verify first (step 0), refuse on any finding.** `manifest.verify_manifest(kit_dir)` re-hashes
  every file the kit claims to carry, checks every recorded symlink, and reports an unlisted
  file or an unsafe/mismatched symlink target -- this installer trusts NONE of a kit's content
  until that list comes back empty. A kit that fails to even parse (`KIT_MANIFEST` missing or not
  valid JSON -- `verify_manifest`'s own documented raise case) is caught here too, as a refusal
  with the same shape as every other one, never a bare traceback.
- **The whole kit tree is copied, verbatim, into `<target>/kit/`** (`shutil.copytree(...,
  symlinks=True)` -- the kit's own pack symlinks are copied AS symlinks, never dereferenced,
  exactly as `verify_manifest` just proved them safe to be). This is what "installs ... the
  binaries, the packs, the suite/site files, the SBOMs, the recorded runs, and the vendored crate
  sources with their `.cargo/config.toml`" means concretely: one recursive copy gets every one of
  those sections onto the target filesystem, in the same shape the kit itself uses, with nothing
  hand-picked (and so nothing silently missed) -- `<target>/kit/binaries/`, `<target>/kit/packs/
  <name>/`, `<target>/kit/suite.merged.toml`, `<target>/kit/secsite.merged.toml`,
  `<target>/kit/sbom/`, `<target>/kit/runs/`, `<target>/kit/vendor/` (when the kit carries it),
  `<target>/kit/KIT_MANIFEST` itself (kept, for provenance -- a copy of the exact document this
  install was verified against).
- **Binaries are made executable** (`chmod 0o755`) after the copy -- `shutil.copytree`
  preserves the source mode bit-for-bit (`copy2`-based), which is already `0o755` for a binary
  `build_kit.collect_binaries` wrote (see that function's own `dest.chmod(0o755)`), but this is
  re-asserted here rather than assumed, since a kit built or transferred by some other means
  (e.g. copied through a medium that does not preserve unix permissions) must not silently leave
  a non-executable "binary" behind.
- **Wheels are installed with `pip install --no-index --find-links <kit>/wheels`** into a fresh
  venv at `<target>/venv/` -- deliberately NOT a plain file copy like every other section:
  "install" for Python packages means resolved, importable site-packages, not a directory of
  `.whl` files sitting next to each other. `--no-index` is load-bearing (this task's own words):
  pip is handed no index to consult at all, so this installs correctly even if an index were
  reachable from wherever this runs -- the offline guarantee does not depend on the network
  actually being down at the moment `pip` runs. The venv is created with the plain stdlib `venv`
  module (`python -m venv`, no network: its bundled `ensurepip` wheels ship inside the
  interpreter itself, never fetched) before `pip install` runs inside it.

# What this refuses, loudly, with a typed message (never a bare traceback, never a silent partial
# install)

1. The kit does not verify (`verify_manifest` returns any finding, or raises because
   `KIT_MANIFEST` is missing/unparseable).
2. The target directory exists and is not empty.
3. `KIT_MANIFEST` is missing a top-level section this installer's own understanding of
   `kit_format` 2 requires (`_REQUIRED_TOP_LEVEL_KEYS`) -- a structural gap, never guessed past.
4. `kit_format` is not the one version (2) this installer understands.
5. A wheel the viewer actually needs is absent from the kit: either the whole `wheels` step was
   never collected (`wheels.collected` is `false`), or one of the viewer's own required
   distributions (`_REQUIRED_WHEEL_DIST_NAMES`, below -- the exact roots
   `scripts/kit/build_kit.py::VIEWER_RUNTIME_ROOTS` declares, plus `altavista` itself, the
   package `python -m altavista` actually is) never got a wheel (present instead in the kit's own
   `gaps` list as `wheel:<name>`).
6. **(task 3c)** the viewer's own static/config assets are absent from the kit: either of
   `_REQUIRED_VIEWER_ASSET_PACKS` (`web`, `profiles`) is missing `KIT_MANIFEST["packs"][name]
   ["copied"]` -- true for `scripts/kit/build_kit.py`'s own output (`manifest.ALWAYS_COPY_PACKS`
   copies both, unconditionally, in every kit it writes), so hitting this refusal means the kit
   at hand was not produced by this worktree's own builder, or was tampered with after the fact.
   Refused for the identical reason as (5): an installed viewer with no `web/` static root or no
   `profiles/` store to read from cannot serve a single request.

Every refusal exits 2 and prints one clearly-labelled line to stderr naming exactly what was
wrong -- never a bare non-zero exit with no explanation (this task's own "an exit code is not
evidence" rule applies just as much to a REFUSAL as to a success).

# Question 199 (no test/tool mutates the process environment)

Nothing below writes `os.environ`. The one subprocess this module runs with a non-default
environment (`pip install`, inside the freshly created venv) is invoked through that venv's own
`pip` executable directly (`<target>/venv/bin/pip`), which needs no extra environment variables
at all -- the venv's own `pyvenv.cfg`/interpreter path is what makes it "activated" for that one
subprocess, not `os.environ["VIRTUAL_ENV"]` or any other mutation of this process's own
environment.
"""
from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))  # so `import manifest` finds scripts/kit/manifest.py

import manifest as kit_manifest  # noqa: E402  (path insert must precede this import)

#: `KIT_MANIFEST` top-level keys this installer's own understanding of `kit_format` 2 requires --
#: exactly `manifest.build_manifest`'s own parameter list (that function is this format's one
#: writer), so a kit missing any of these could not have been written by this worktree's own
#: builder at all -- a real structural problem, not a stylistic one, and refused rather than
#: guessed past.
_REQUIRED_TOP_LEVEL_KEYS = (
    "kit_format", "git_commit", "git_dirty", "git_status", "files", "pack_symlinks",
    "images", "sboms", "packs", "vendor", "wheels", "binaries", "runs", "gaps",
)

#: The viewer's own required distributions (normalized dist names) -- `scripts/kit/
#: build_kit.py::VIEWER_RUNTIME_ROOTS`'s own five keys, plus `altavista` itself (the package
#: `python -m altavista` -- the viewer server's own entry point -- actually is). Hard-coded here,
#: deliberately NOT imported from `build_kit` (which pulls in `packaging`, a third-party
#: dependency that a bare `python:3.13-slim` container -- this installer's own proof platform --
#: does not have installed before this installer runs): this installer needs to know these five
#: names to check "is the viewer's own wheel set complete", nothing more, so it names them
#: directly rather than dragging in a dependency it would then need a wheel for just to check
#: whether it has wheels.
_REQUIRED_WHEEL_DIST_NAMES = ("altavista", "fastapi", "uvicorn", "websockets", "numpy", "protobuf")

#: (task 3c) The viewer's own static/config packs, WITHOUT which an installed viewer has no
#: static root and no profile store to read from. Unlike `_REQUIRED_WHEEL_DIST_NAMES` (kept
#: hard-coded because importing `build_kit` would drag in `packaging`, a dependency this
#: installer's own bare-Python proof platform lacks), `manifest.py` is already this installer's
#: own dependency (`kit_manifest`, imported above) and is stdlib-only itself, so this reuses
#: `manifest.ALWAYS_COPY_PACKS` directly rather than restating the same two names a second time.
_REQUIRED_VIEWER_ASSET_PACKS = kit_manifest.ALWAYS_COPY_PACKS


class InstallRefused(RuntimeError):
    """Raised for every one of this installer's own typed refusals (see this module's top doc,
    "What this refuses"). Caught once, in `main`, and printed as a single labelled stderr line
    plus exit code 2 -- never a bare traceback for a condition this installer itself detected and
    named."""


def _normalize_dist_name(name: str) -> str:
    return name.lower().replace("_", "-")


def _load_manifest_or_refuse(kit_dir: Path) -> dict:
    manifest_path = kit_dir / "KIT_MANIFEST"
    if not manifest_path.is_file():
        raise InstallRefused(
            f"{kit_dir} has no KIT_MANIFEST -- this is not a kit this installer can verify, let "
            f"alone install from"
        )
    try:
        findings = kit_manifest.verify_manifest(kit_dir)
    except (OSError, json.JSONDecodeError) as e:
        raise InstallRefused(
            f"{manifest_path} could not be read/parsed as a kit manifest ({type(e).__name__}: "
            f"{e}) -- refusing to install from a kit whose own manifest is not trustworthy"
        ) from e
    if findings:
        lines = "\n".join(f"  - {f.kind}: {f.path} -- {f.message}" for f in findings)
        raise InstallRefused(
            f"{kit_dir} does NOT verify against its own KIT_MANIFEST -- {len(findings)} "
            f"finding(s), refusing to install a single byte from an untrusted kit:\n{lines}"
        )
    return json.loads(manifest_path.read_text(encoding="utf-8"))


def _check_required_sections(doc: dict) -> None:
    missing = [k for k in _REQUIRED_TOP_LEVEL_KEYS if k not in doc]
    if missing:
        raise InstallRefused(
            f"KIT_MANIFEST is missing required section(s) {missing!r} -- this installer only "
            f"understands kit_format {kit_manifest.KIT_FORMAT}'s shape "
            f"(scripts/kit/manifest.py::build_manifest's own field list), and a kit missing one "
            f"of these could not have been written by that function at all"
        )
    if doc["kit_format"] != kit_manifest.KIT_FORMAT:
        raise InstallRefused(
            f"KIT_MANIFEST declares kit_format {doc['kit_format']!r}, but this installer only "
            f"understands kit_format {kit_manifest.KIT_FORMAT} -- refusing to guess at a shape "
            f"it was not written against"
        )


def _check_required_wheels(doc: dict) -> None:
    wheels = doc["wheels"]
    if not wheels.get("collected"):
        raise InstallRefused(
            "KIT_MANIFEST's wheels.collected is false -- this kit was built without "
            "--with-wheels, so it carries no installable Python packages at all; the viewer "
            "cannot be installed from it. Rebuild the kit with --with-wheels."
        )
    fetched_names = {_normalize_dist_name(f["name"]) for f in wheels.get("fetched", [])}
    missing = [
        name for name in _REQUIRED_WHEEL_DIST_NAMES
        if _normalize_dist_name(name) not in fetched_names
    ]
    if missing:
        raise InstallRefused(
            f"the viewer needs a wheel for {missing!r}, but KIT_MANIFEST's wheels.fetched does "
            f"not name it -- check KIT_MANIFEST's own gaps list for a matching 'wheel:<name>' "
            f"entry naming why. Refusing to install a viewer that cannot import."
        )


def _check_required_viewer_assets(doc: dict) -> None:
    """(task 3c) refusal (6) from this module's own top doc: every pack in
    `_REQUIRED_VIEWER_ASSET_PACKS` must show `KIT_MANIFEST["packs"][name]["copied"]` true, or an
    installed viewer would have no static root (`web`) or no profile/policy store (`profiles`)
    to read from -- the identical failure mode `_check_required_wheels` refuses for a missing
    wheel, one level up the same dependency chain."""
    missing = [
        name for name in _REQUIRED_VIEWER_ASSET_PACKS
        if not doc.get("packs", {}).get(name, {}).get("copied")
    ]
    if missing:
        raise InstallRefused(
            f"the viewer needs its own static/config assets, but KIT_MANIFEST's packs section "
            f"does not show {missing!r} as copied -- this kit was not built with "
            f"manifest.ALWAYS_COPY_PACKS honoured (every kit scripts/kit/build_kit.py itself "
            f"writes copies these unconditionally; a kit missing one either predates task 3c or "
            f"was tampered with). Refusing to install a viewer with no web/ static root or no "
            f"profiles/ store to read from."
        )


def _check_target_dir(target_dir: Path) -> None:
    if target_dir.exists():
        if not target_dir.is_dir():
            raise InstallRefused(f"{target_dir} exists and is not a directory")
        if any(target_dir.iterdir()):
            raise InstallRefused(
                f"{target_dir} already exists and is not empty -- refusing to merge into or "
                f"overwrite an existing directory; pass a fresh or empty target"
            )
    else:
        target_dir.mkdir(parents=True)


def _copy_kit_tree(kit_dir: Path, target_dir: Path) -> Path:
    """Copies the ENTIRE kit tree into `<target_dir>/kit/`, symlinks preserved verbatim
    (`shutil.copytree(..., symlinks=True)`) -- see this module's own top doc for why one
    wholesale copy, rather than a hand-picked list of sections, is what "install the packs, the
    suite/site files, the SBOMs, the recorded runs, and the vendored crate sources" means here.
    `manifest.verify_manifest` (already run, successfully, before this is ever called) is what
    makes copying every symlink verbatim safe: every one still present has already been proven
    relative and lexically inside its own pack's root."""
    dest = target_dir / "kit"
    shutil.copytree(kit_dir, dest, symlinks=True)
    return dest


def _make_binaries_executable(installed_kit_dir: Path) -> list[str]:
    bin_dir = installed_kit_dir / "binaries"
    if not bin_dir.is_dir():
        return []
    made_executable = []
    for entry in sorted(bin_dir.iterdir()):
        if entry.is_file() and not entry.is_symlink():
            entry.chmod(0o755)
            made_executable.append(entry.name)
    return made_executable


def _install_wheels_into_venv(kit_dir: Path, target_dir: Path) -> dict:
    """`python -m venv <target>/venv` (offline: ensurepip's own wheels ship inside the
    interpreter) then `<target>/venv/bin/pip install --no-index --find-links <kit>/wheels
    altavista` -- installs `altavista` and, transitively, every real runtime dependency
    `build_kit.viewer_runtime_closure` walked and the kit carries a wheel for (`--no-index` means
    pip can resolve ONLY from `--find-links`, never a real index, so an incomplete closure is a
    hard, visible `pip` failure here -- never a silent partial install)."""
    venv_dir = target_dir / "venv"
    venv_result = subprocess.run(
        [sys.executable, "-m", "venv", str(venv_dir)],
        capture_output=True, text=True, timeout=120,
    )
    if venv_result.returncode != 0:
        raise InstallRefused(
            f"python -m venv {venv_dir} failed (rc={venv_result.returncode}):\n"
            f"{(venv_result.stderr or venv_result.stdout).strip()}"
        )
    venv_python = venv_dir / "bin" / "python"
    venv_pip = venv_dir / "bin" / "pip"
    if not venv_pip.is_file():
        # Some minimal base images produce a venv with no pip unless ensurepip ran -- fall back
        # to `python -m ensurepip --default-pip` (still no network: bundled wheels only) before
        # giving up.
        ensure = subprocess.run(
            [str(venv_python), "-m", "ensurepip", "--default-pip"],
            capture_output=True, text=True, timeout=60,
        )
        if ensure.returncode != 0 or not venv_pip.is_file():
            raise InstallRefused(
                f"the venv at {venv_dir} has no pip and `ensurepip` could not provide one "
                f"(rc={ensure.returncode}): {(ensure.stderr or ensure.stdout).strip()}"
            )
    wheels_dir = kit_dir / "wheels"
    install_result = subprocess.run(
        [
            str(venv_pip), "install", "--no-index", "--find-links", str(wheels_dir),
            "altavista",
        ],
        capture_output=True, text=True, timeout=180,
    )
    if install_result.returncode != 0:
        raise InstallRefused(
            f"pip install --no-index --find-links {wheels_dir} altavista failed "
            f"(rc={install_result.returncode}):\n"
            f"--- stdout ---\n{install_result.stdout}\n--- stderr ---\n{install_result.stderr}"
        )
    freeze = subprocess.run(
        [str(venv_pip), "list", "--format=json"], capture_output=True, text=True, timeout=30,
    )
    installed_packages = json.loads(freeze.stdout) if freeze.returncode == 0 else []
    return {
        "venv": "venv",
        "installed_packages": installed_packages,
        "pip_install_stdout_tail": install_result.stdout.strip().splitlines()[-5:],
    }


def install(kit_dir: Path, target_dir: Path) -> dict:
    """Runs the whole install: verify, refuse-checks, copy, chmod, pip install, INSTALL_RECORD.
    Returns the INSTALL_RECORD dict (also written to `<target_dir>/INSTALL_RECORD`). Raises
    `InstallRefused` for every named refusal condition -- callers (this module's own `main`, and
    tests) get one typed exception, never a bare traceback for a condition this function itself
    detected."""
    kit_dir = kit_dir.resolve()
    target_dir = target_dir.resolve()

    doc = _load_manifest_or_refuse(kit_dir)
    kit_manifest_sha256 = kit_manifest.sbom.sha256_file(kit_dir / "KIT_MANIFEST")
    _check_required_sections(doc)
    _check_required_wheels(doc)
    _check_required_viewer_assets(doc)
    _check_target_dir(target_dir)

    installed_kit_dir = _copy_kit_tree(kit_dir, target_dir)
    binaries_made_executable = _make_binaries_executable(installed_kit_dir)
    wheels_info = _install_wheels_into_venv(kit_dir, target_dir)

    # (task 3c) named explicitly, not merely implied by `"packs"` below -- these are the two
    # paths (relative to `target_dir`) a caller starting the viewer from this install actually
    # needs (`create_app(web_dir=...)`, `altavista.profile.PROFILES_DIR = ...`); see
    # `_check_required_viewer_assets` for why both are guaranteed present at this point.
    viewer_assets = {
        name: f"kit/packs/{name}" for name in _REQUIRED_VIEWER_ASSET_PACKS
    }

    record = {
        "kit_manifest_sha256": kit_manifest_sha256,
        "kit_format": doc["kit_format"],
        "kit_git_commit": doc["git_commit"],
        "kit_git_dirty": doc["git_dirty"],
        "installed": {
            "kit_tree": "kit",
            "binaries_made_executable": binaries_made_executable,
            "packs": sorted(doc["packs"].keys()),
            "wheels": wheels_info,
            "vendor_collected": doc["vendor"]["collected"],
            "sboms": sorted(doc["sboms"].keys()),
            "runs": sorted(doc["runs"].keys()),
            "suite_site_files": ["suite.merged.toml", "secsite.merged.toml"],
            "viewer_assets": {
                "web_dir": viewer_assets["web"],
                "profiles_dir": viewer_assets["profiles"],
            },
        },
        "gaps": doc["gaps"],
    }
    record_path = target_dir / "INSTALL_RECORD"
    record_path.write_text(json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")
    return record


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("kit_dir", type=Path, help="an existing kit directory (KIT_MANIFEST + content)")
    p.add_argument("target_dir", type=Path, help="a fresh or empty directory to install into")
    return p


def main(argv: "list[str] | None" = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        record = install(args.kit_dir, args.target_dir)
    except InstallRefused as e:
        print(f"scripts/kit/install.py: REFUSED -- {e}", file=sys.stderr)
        return 2
    print(f"installed kit {args.kit_dir} -> {args.target_dir}")
    print(f"kit manifest sha256: {record['kit_manifest_sha256']}")
    print(f"wrote {args.target_dir / 'INSTALL_RECORD'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
