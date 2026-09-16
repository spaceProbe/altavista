"""scripts/kit/install.py -- D3's second half (docs/p5-plan.md, P5 track round 2 task 3b, round 3
questions 217(b)/217(c)): the real logic behind scripts/kit/install.sh. Installs a kit
(scripts/kit/build_kit.py's own output, `kit_format` 4) into a fresh, empty target directory,
using nothing beyond the kit itself, a
Python interpreter, and a shell -- no network, ever (question 154: "a kit is built with network
once and installed with none" -- this half is the "installed with none" clause, proved for real
by tests/test_kit_zero_egress_install.py).

# This module now ships INSIDE every kit (round 3, question 217(c))

Before this round, the zero-egress install proof bind-mounted THIS repository's own
`scripts/kit/` (read-only) into the installing container alongside the kit itself, so the
"nothing beyond the kit itself" claim two paragraphs up was not, in fact, true end to end -- round
2's own decision 9 recorded that gap openly. `build_kit.py::assemble_installer` now copies this
file, `install.sh`, and the exact local modules it imports (`manifest.py`, `sbom.py`,
`licences.py` -- nothing else, checked recursively; see `build_kit.py::INSTALLER_SOURCE_FILES`'s
own comment) into `<kit>/installer/`, unconditionally, every kit. The version of this file that
actually runs during an install is therefore the COPY inside `installer/`, not this repository's
own `scripts/kit/install.py` -- both start out byte-identical (the same source file, copied), but
only the one bundled in the kit is ever executed by `tests/test_kit_zero_egress_install.py`'s real
proof, or by any real install of that kit.

**What that bundling proves, and does not prove** -- worth stating plainly rather than leaving
implied: `_load_manifest_or_refuse`, below, re-hashes every file `KIT_MANIFEST` lists, including
this file's own bundled copy and the other three modules it imports, before trusting any of them.
That proves the kit has not been corrupted or partially transferred, and that `KIT_MANIFEST`
accounts for everything present -- ordinary CONSISTENCY. It does not, and cannot, prove
AUTHENTICITY: whoever can tamper with a kit's files can equally tamper with `KIT_MANIFEST` (and
with this installer) to match, since both come from the same source and neither is trusted
independently of the other -- an installer cannot verify itself into legitimacy. The one thing
that anchors trust here is `KIT_MANIFEST`'s own SHA-256, computed and recorded OUTSIDE the kit, by
whoever built or received it, and compared before this installer is ever run -- see
`manifest.py`'s own top doc, "P5 track round 3, question 217(c)", for the fuller version of this
same argument.

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
   `kit_format` requires (`_REQUIRED_TOP_LEVEL_KEYS`) -- a structural gap, never guessed past.
4. `kit_format` is not the one version (`manifest.KIT_FORMAT`, `4` as of round 3 question 217(c))
   this installer understands.
5. A wheel the viewer actually needs is absent from the kit: either the whole `wheels` step was
   never collected (`wheels.collected` is `false`), or one of the viewer's own required
   distributions (`_REQUIRED_WHEEL_DIST_NAMES`, below -- the exact roots
   `scripts/kit/build_kit.py::VIEWER_RUNTIME_ROOTS` declares, plus `altavista` itself, the
   package `python -m altavista` actually is) never got a wheel (present instead in the kit's own
   `gaps` list as `wheel:<name>`).
6. **(round 3, question 217(b))** the viewer's own static/config assets are absent from the kit's
   `altavista` wheel: `_check_viewer_assets_in_wheel` opens that wheel (a wheel is a zip) and
   checks its namelist for `altavista/web/index.html` and `altavista/profiles/design.yaml` --
   both present in any wheel built from this worktree's own round-3 packaging change
   (`pyproject.toml`/`setup.py`). Task 3c's original version of this refusal checked two now-
   deleted kit PACKS (`web`, `profiles`) instead of the wheel; round 3 replaces it, in place, with
   the equivalent check for the new shape (see `manifest.py`'s own top doc, "P5 track round 3",
   for why the packs are gone and `kit_format` moved to 3) -- never simply dropped: an installed
   viewer with no `web/` static root or no `profiles/` store to read from still cannot serve a
   single request, so this is exactly as load-bearing as it always was.

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
import zipfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))  # so `import manifest` finds scripts/kit/manifest.py

import manifest as kit_manifest  # noqa: E402  (path insert must precede this import)

#: `KIT_MANIFEST` top-level keys this installer's own understanding of `kit_format` 3 requires --
#: exactly `manifest.build_manifest`'s own parameter list (that function is this format's one
#: writer), so a kit missing any of these could not have been written by this worktree's own
#: builder at all -- a real structural problem, not a stylistic one, and refused rather than
#: guessed past. Unchanged by round 3 (question 217(b)): the TOP-LEVEL shape of `KIT_MANIFEST` did
#: not change, only the *content* of its `packs` section (the `web`/`profiles` packs are gone) --
#: which is exactly why `kit_format` still had to bump (see `manifest.py`'s own top doc) even
#: though this particular list of keys did not.
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

#: (round 3, question 217(b)) Marker paths inside the `altavista` wheel's own zip namelist that
#: prove it was built with the round-3 packaging change (`pyproject.toml`/`setup.py`) -- one file
#: from each of `web/` and `profiles/`, not merely the directory prefix (a zip has no notion of an
#: empty directory entry that could pass a prefix-only check for the wrong reason). Replaces task
#: 3c's `_REQUIRED_VIEWER_ASSET_PACKS` (`manifest.ALWAYS_COPY_PACKS`, both now deleted along with
#: the `web`/`profiles` packs themselves -- see `manifest.py`'s own top doc) -- see
#: `_check_viewer_assets_in_wheel`, below, for the check itself.
_VIEWER_ASSET_WHEEL_MARKERS = ("altavista/web/index.html", "altavista/profiles/design.yaml")


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


def _check_viewer_assets_in_wheel(kit_dir: Path, doc: dict) -> None:
    """(round 3, question 217(b)) refusal (6) from this module's own top doc -- replaces task
    3c's `_check_required_viewer_assets`, which refused a kit whose `web`/`profiles` PACKS were
    not copied. Those packs no longer exist at all (`manifest.py`'s own top doc, "P5 track round
    3"): the viewer's static/config assets now ship INSIDE the `altavista` wheel itself
    (`pyproject.toml`/`setup.py`'s packaging change), so the equivalent protection reads that
    wheel's own zip namelist directly (`zipfile`, stdlib, no dependency on the wheel being
    installed anywhere first) and refuses if either marker file is absent -- never simply
    dropped, exactly as load-bearing as the pack check it replaces: an installed viewer with no
    `web/` static root or no `profiles/` store to read from still cannot serve a single request.

    Called AFTER `_check_required_wheels` (which already refuses a kit with no `altavista` entry
    in `wheels.fetched` at all), so `altavista_entry` below is expected to exist; the `None` guard
    is defence in depth, never reached in practice, not the primary refusal path."""
    altavista_entry = next(
        (f for f in doc.get("wheels", {}).get("fetched", [])
         if _normalize_dist_name(f["name"]) == "altavista"), None,
    )
    if altavista_entry is None:
        raise InstallRefused(
            "KIT_MANIFEST's wheels.fetched names no altavista wheel at all -- cannot check it "
            "for the viewer's own static/config assets"
        )
    wheel_path = kit_dir / "wheels" / altavista_entry["filename"]
    if not wheel_path.is_file():
        raise InstallRefused(
            f"KIT_MANIFEST names {wheel_path} as the altavista wheel, but it is not present in "
            f"the kit (manifest.verify_manifest, already run above, should have caught this "
            f"first -- this is defence in depth)"
        )
    with zipfile.ZipFile(wheel_path) as zf:
        names = set(zf.namelist())
    missing = [m for m in _VIEWER_ASSET_WHEEL_MARKERS if m not in names]
    if missing:
        raise InstallRefused(
            f"{wheel_path.name} does not contain {missing!r} -- this kit's own altavista wheel "
            f"was not built with the round-3 packaging change (pyproject.toml/setup.py, question "
            f"217(b)) that ships web/ and profiles/ inside it. Refusing to install a viewer with "
            f"no web/ static root or no profiles/ store to read from."
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
    _check_viewer_assets_in_wheel(kit_dir, doc)
    _check_target_dir(target_dir)

    installed_kit_dir = _copy_kit_tree(kit_dir, target_dir)
    binaries_made_executable = _make_binaries_executable(installed_kit_dir)
    wheels_info = _install_wheels_into_venv(kit_dir, target_dir)

    # (round 3, question 217(b)) No separate "viewer_assets" entry any more -- task 3c's version
    # named `kit/packs/web`/`kit/packs/profiles` here because those were real, separate paths a
    # caller had to point `create_app(web_dir=...)`/`altavista.profile.PROFILES_DIR` at by hand.
    # Now that both ship INSIDE the `altavista` wheel (`_check_viewer_assets_in_wheel`, just run,
    # already proved it), they are simply part of the installed wheel this record's own "wheels"
    # section already names -- `altavista.server.WEB_DIR`/`altavista.profile.resolve_profiles_dir`
    # find them on their own, with no extra path for this installer to compute or report.
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
