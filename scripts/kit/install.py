"""scripts/kit/install.py -- D3's second half (docs/p5-plan.md, P5 track round 2 task 3b, round 3
questions 217(b)/217(c)): the real logic behind scripts/kit/install.sh. Installs a kit
(scripts/kit/build_kit.py's own output, `kit_format` 5) into a fresh, empty target directory,
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

# Images are NOT built here (question 250(a), second half; the lead's ruling)

This install runs inside the enclave container, which never holds the Docker socket, `DOCKER_HOST`
or any other path to the daemon: a container that controls the host's daemon can start a networked
container, which would defeat the `--internal` isolation the zero-egress proof rests on. Building
an image needs the daemon, so it is a separate step the operator runs ON THE DOCKER HOST with the
same kit and the same bundled installer:

    <kit>/installer/install.sh --build-images <kit> [--image-tag COMPONENT=REF ...] [--record FILE]
    <kit>/installer/install.sh --probe-offline-build <kit>

`main` below dispatches those two flags to `image_build.py` (its module doc is the specification);
everything else here is the files install, unchanged in shape. `INSTALL_RECORD.installed.images`
says so in the record itself: `built: false`, who builds them and with which command, and for each
image the pinned base, the runtime-content hash the build must reproduce and the default tag.
The image step writes its own record (`IMAGE_BUILD_RECORD`) rather than amending this one, because
it runs on a different machine at a different time and the files install's target is not
necessarily reachable from the Docker host.

# GMAT is operator-supplied and verified here, never copied (question 250(a), task 2)

GMAT R2026a bundles OpenSceneGraph and wxWidgets (LGPL), so a kit records GMAT's pack descriptor
(`packs.gmat`, with an `operator_supplied` marker) and never carries a byte of it
(`docs/kit-operator-supplied.md` item 3). When the kit has `packs.gmat`, the operator brings an
unpacked GMAT R2026a for the target platform and says where it is and which platform it is:

    <kit>/installer/install.sh <kit> <target> --gmat-root PATH --gmat-platform KEY

`verify_operator_gmat` measures the tree (`manifest.measure_tree`, under the pin's `scope`) and
compares it with the pin recorded for KEY in the installer's own `operator_supplied.json`: content
hash, file count, total bytes, and the count and hash of the symlink listing. A match is recorded in
`INSTALL_RECORD.installed.operator_supplied.gmat` (platform, root exactly as given, what was found,
the pin) and the tree is not touched or copied. The platform is never guessed from the installing
host: this installer also runs in a Linux container over a macOS tree (question 217), where the
host says nothing about the tree. KEY is `<os>-<uname -m>`, e.g. `macos-arm64`. When the kit has no
`packs.gmat` the install does not need GMAT and says so in the record. The measurement is a check at
install time over what the pin covers (`not_covered` in the pin component, repeated in the record);
it does not prove the tree is unchanged afterwards.

# Every operator-supplied component is listed, the ones this step can reach are verified (question 250(a), task 3)

`docs/kit-operator-supplied.md` lists six components; `operator_check.py` (its module doc is the
specification) decides which of them this kit needs, from `KIT_MANIFEST` alone, and this install prints
one line for each of the six through `verify_operator_supplied`:

    operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  verified by the image step, not here (record: IMAGE_BUILD_RECORD operator_supplied; this step does not read it)
    operator-supplied: gmat-r2026a  required (packs.gmat)  verified macos-arm64 sha256:d21cddbbffbd… 2060 files
    operator-supplied: clickhouse  outside the kit, not verified (not declared; held pending licence review, q247)

This step verifies trees (GMAT); container images are verified by the host-side image step, which
prints the same six lines with the roles reversed. Together the two outputs cover every component: a
`required` line says `verified` in one and `not here` in the other. The structured list is
`INSTALL_RECORD.installed.operator_supplied.components`. A required component that is missing or
different is a typed refusal (exit 2), after the lines up to and including it were printed.
`install.sh --check-operator-supplied <kit> [--gmat-root PATH --gmat-platform KEY]` runs just this
part (the kit's own verification, then the list) and installs nothing.

# An operator-supplied wheel (question 250(a), wheel libraries)

The wheel step inventories the native libraries every wheel bundles (`wheel_libs.py`; the inventory is in
`KIT_MANIFEST.wheels.fetched[*].bundled_libraries`). A wheel that bundles a library under a GPL-family licence
(numpy's x86_64 wheels bundle `libquadmath`; its linux/aarch64 wheel does not) is not carried: it is listed
in `KIT_MANIFEST.wheels.operator_supplied` with its filename and sha256, and the pins file has a
`python-wheel` component for it. Such a kit needs the operator's copy:

    <kit>/installer/install.sh <kit> <target> --operator-wheels DIR

`DIR/<filename>` must have the pinned sha256 (`operator_check.verify_python_wheel`). The verified file is
COPIED to `<target>/operator-wheels/` and the copy re-hashed, and pip installs from that copy by exact path
alongside `--find-links <kit>/wheels`: pip is never given the operator's directory as a place to search,
so no other file in it (another version of the same package, a different package) can be picked up, and a
file replaced between the check and the install is caught. A kit with no operator-supplied wheel needs no
option and ignores one (`INSTALL_RECORD.installed.operator_wheels` says so).

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
4. `kit_format` is not the one version (`manifest.KIT_FORMAT`, `5` as of question 250(a))
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

7. **(question 250(a), task 2)** `packs.gmat` is in the kit and the operator's GMAT is not what its
   pin says, each with a `gmat-*` code (`OperatorSuppliedRefused.code`): `gmat-root-missing`
   (no `--gmat-root`; names every pinned platform with hash, files, bytes),
   `gmat-root-not-a-directory`, `gmat-platform-missing`, `gmat-no-pin-for-platform` (lists the
   platforms that have pins), `gmat-root-unreadable`, `gmat-descriptor-differs` (expected and found
   hash, file count, bytes, symlinks), and, for a broken kit or pins file,
   `gmat-kit-not-operator-supplied` and `gmat-pin-malformed`. All of them happen before the target
   directory is created. (Task 3) The pins file itself (`pins-malformed`), a kit that needs a pin the
   file lacks (`gmat-pin-malformed`, `base-pin-missing`) or declares a component that cannot be declared
   (`declared-component-refused`) refuse the same way.

8. **(question 250(a), wheel libraries)** a wheel of the viewer's closure bundles a GPL-family library, so the
   kit does not carry it (`KIT_MANIFEST.wheels.operator_supplied`) and the operator's copy is not what its
   pin says, each with a `wheel-*` code: `wheel-dir-missing` (no `--operator-wheels`; names the file, its
   pinned sha256 and where it comes from), `wheel-dir-not-a-directory`, `wheel-missing` (lists the wheels
   found), `wheel-unreadable`, `wheel-hash-differs` (expected and found sha256), `wheel-kit-entry-differs`
   (the kit was built against another pin than this installer carries), `wheel-pin-missing`. Also refused:
   a `wheels` section without the bundled-library inventory (a kit built before it), and a kit that
   carries a wheel whose inventory shows a GPL-family library. See "An operator-supplied wheel", above.

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
from typing import Callable

# An installer that runs from inside a kit must not add files to it: the modules imported below would
# otherwise leave `__pycache__/` next to them in `<kit>/installer/`, and the kit would no longer verify
# against its own `KIT_MANIFEST` (found running the host-side image step on a real kit, question 250(a)).
sys.dont_write_bytecode = True

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))  # so `import manifest` finds scripts/kit/manifest.py

import image_build  # noqa: E402  (question 250(a): the host-side image step this module dispatches to)
import manifest as kit_manifest  # noqa: E402  (path insert must precede this import)
import operator_check  # noqa: E402  (question 250(a), task 3: what the kit needs of the operator, and the printed list)

#: The kit pack the operator supplies (question 250(a), task 2); `manifest.OPERATOR_SUPPLIED_PACKS` names its pin.
GMAT_PACK = "gmat"

#: The two flags `main` hands to `image_build.main` instead of running the files install.
IMAGE_STEP_FLAGS = ("--build-images", "--probe-offline-build")

#: The flag that verifies the operator-supplied components this step can reach and prints the list,
#: without installing (question 250(a), task 3).
CHECK_FLAG = "--check-operator-supplied"

OPERATOR_WHEELS_HELP = (
    "a directory holding the wheels this kit does not carry because they bundle a GPL-family library "
    "(KIT_MANIFEST wheels.operator_supplied; a python-wheel component of the pins file): <DIR>/<filename> must have "
    "the pinned sha256. Needed only by a kit that lists such a wheel"
)

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
    "images", "sboms", "packs", "vendor", "wheels", "binaries", "runs", "gaps", "operator_supplied_declared",
)

#: The viewer's own required distributions (normalized dist names) -- `scripts/kit/
#: build_kit.py::VIEWER_RUNTIME_ROOTS`'s own keys (seven since question 250's round added `httpx` and `pyyaml`), plus `altavista` itself (the package
#: `python -m altavista` -- the viewer server's own entry point -- actually is). Hard-coded here,
#: deliberately NOT imported from `build_kit` (which pulls in `packaging`, a third-party
#: dependency that a bare `python:3.13-slim` container -- this installer's own proof platform --
#: does not have installed before this installer runs): this installer needs to know these five
#: names to check "is the viewer's own wheel set complete", nothing more, so it names them
#: directly rather than dragging in a dependency it would then need a wheel for just to check
#: whether it has wheels.
_REQUIRED_WHEEL_DIST_NAMES = ("altavista", "fastapi", "uvicorn", "websockets", "numpy", "protobuf", "httpx", "pyyaml")

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


class OperatorSuppliedRefused(InstallRefused):
    """An `InstallRefused` about an operator-supplied component, with a stable `code` (see this
    module's top doc, refusal 7) tests and callers can match on; the message starts with it."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code


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
    operator = wheels.get("operator_supplied")
    if not isinstance(operator, list):
        raise InstallRefused(
            "KIT_MANIFEST's wheels section has no operator_supplied list -- this kit was built before the "
            "wheel step inventoried the native libraries its wheels bundle (question 250(a)), so nothing says "
            "whether a wheel it carries bundles a GPL-family library. Rebuild the kit with --with-wheels."
        )
    carried = wheels.get("fetched", [])
    for entry in [*carried, *operator]:
        if not isinstance(entry.get("bundled_libraries"), list):
            raise InstallRefused(
                f"KIT_MANIFEST's wheels entry for {entry.get('filename')!r} has no bundled_libraries inventory -- "
                f"a kit built before the wheel step inventoried bundled native libraries (question 250(a)). "
                f"Rebuild the kit with --with-wheels."
            )
    for entry in carried:
        gpl = sorted({lib.get("library") for lib in entry["bundled_libraries"] if lib.get("gpl_family")})
        if gpl:
            raise InstallRefused(
                f"the kit carries {entry.get('filename')!r}, which bundles {gpl} under a GPL-family licence: such a wheel "
                f"is operator-supplied and is listed in wheels.operator_supplied, never carried "
                f"(docs/kit-operator-supplied.md). This kit was not built by this repository's builder."
            )
    fetched_names = {_normalize_dist_name(f["name"]) for f in [*carried, *operator]}
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


def _install_wheels_into_venv(kit_dir: Path, target_dir: Path, extra_wheels: "list[Path] | tuple[Path, ...]" = ()) -> dict:
    """`python -m venv <target>/venv` (offline: ensurepip's own wheels ship inside the
    interpreter) then `<target>/venv/bin/pip install --no-index --find-links <kit>/wheels
    altavista` -- installs `altavista` and, transitively, every real runtime dependency
    `build_kit.viewer_runtime_closure` walked and the kit carries a wheel for (`--no-index` means
    pip can resolve ONLY from `--find-links`, never a real index, so an incomplete closure is a
    hard, visible `pip` failure here -- never a silent partial install). `extra_wheels` are the verified
    operator-supplied wheels (module doc, "An operator-supplied wheel"), staged under the target: each is
    given to pip by its exact path, so pip resolves `numpy` (say) from it and from nothing else."""
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
            "altavista", *[str(w) for w in extra_wheels],
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


def _gmat_not_needed_record(gmat_root: "str | None") -> dict:
    record = {
        "required": False, "verified": False,
        "reason": "the kit has no packs.gmat, so its contents do not need GMAT; nothing was checked",
    }
    if gmat_root is not None:
        record["root_given_but_unused"] = gmat_root
    return record


def _tree_verifier(doc: dict, gmat_root: "str | None", gmat_platform: "str | None"):
    """The `data-tree` verifier `operator_check.verify_all` takes: GMAT, from the operator's own tree."""
    def verify(pin_id: str, pin: dict, requirement: "operator_check.Requirement") -> "operator_check.Verification":
        return operator_check.verify_gmat_tree(
            pin_id, pin, requirement, entry=doc["packs"].get(pin["pack"]), root=gmat_root, platform=gmat_platform,
        )
    return verify


def _wheel_verifier(doc: dict, operator_wheels: "str | None"):
    """The `python-wheel` verifier `operator_check.verify_all` takes: the operator's copy of a wheel the kit does not carry."""
    def verify(pin_id: str, pin: dict, requirement: "operator_check.Requirement") -> "operator_check.Verification":
        entry = next((e for e in operator_check.kit_operator_wheels(doc) if e.get("component") == pin_id), None)
        return operator_check.verify_python_wheel(pin_id, pin, requirement, entry=entry, directory=operator_wheels)
    return verify


def verify_operator_gmat(
    doc: dict, *, gmat_root: "str | None", gmat_platform: "str | None", pins_file: "Path | None" = None,
) -> dict:
    """GMAT's operator-supplied check (module doc, "GMAT is operator-supplied"): returns what was
    found and verified, for `INSTALL_RECORD.installed.operator_supplied.gmat`, or raises
    `OperatorSuppliedRefused`. Reads nothing but `gmat_root`, which it never writes or copies; the
    pins come from the installer's own `operator_supplied.json` (`pins_file` for tests). The check
    itself is `operator_check.verify_gmat_tree`; this is the single-component form of what `install`
    runs for all six components.

    Refusal order, cheapest first, the hash last: the kit's marker, the pin, no root, root not a
    directory, no platform, no pin for the platform, then the measurement."""
    entry = doc["packs"].get(GMAT_PACK)
    if entry is None:
        return _gmat_not_needed_record(gmat_root)
    component_id = kit_manifest.OPERATOR_SUPPLIED_PACKS[GMAT_PACK]
    pins_path = pins_file or kit_manifest.PINS_FILE
    try:
        pin = kit_manifest.load_operator_pins(pins_path)[component_id]
        pin["pack"]  # a component without the pack it pins is malformed, not merely incomplete
    except (OSError, ValueError, KeyError, TypeError) as e:
        raise OperatorSuppliedRefused(
            "gmat-pin-malformed",
            f"the installer's pins file {pins_path} has no usable {component_id!r} component "
            f"({type(e).__name__}: {e})",
        ) from e
    result = operator_check.verify_gmat_tree(
        component_id, pin, operator_check.Requirement(True, f"required (packs.{GMAT_PACK})"),
        entry=entry, root=gmat_root, platform=gmat_platform,
    )
    if not result.verified:
        raise OperatorSuppliedRefused(result.code, result.reason)
    return result.detail["record"]


def verify_operator_supplied(
    doc: dict, *, gmat_root: "str | None" = None, gmat_platform: "str | None" = None,
    pins_file: "Path | None" = None, out: "Callable[[str], None] | None" = None,
    operator_wheels: "str | None" = None,
) -> "list[operator_check.Verification]":
    """The files install's half of question 250(a), task 3: every component of the pins file, in item
    order, printed through `out` one line each; the trees (GMAT) and the operator-supplied wheels this step can
    reach are verified, the container images are listed as the host-side image step's. Returns the results; raises
    `OperatorSuppliedRefused` (code and message from the refused result) at the first required
    component that is missing or different, after printing the lines up to and including it."""
    pins_path = pins_file or kit_manifest.PINS_FILE
    try:
        pins = kit_manifest.load_operator_pins(pins_path)
        problems = operator_check.pins_problems(pins)
    except (OSError, ValueError, KeyError, TypeError) as e:
        raise OperatorSuppliedRefused(
            "pins-malformed", f"the installer's pins file {pins_path} could not be read ({type(e).__name__}: {e})",
        ) from e
    if problems:
        raise OperatorSuppliedRefused("pins-malformed", f"{pins_path}: " + "; ".join(problems))
    try:
        return operator_check.verify_all(
            operator_check.STEP_FILES, doc, pins,
            verifiers={
                operator_check.KIND_TREE: _tree_verifier(doc, gmat_root, gmat_platform),
                operator_check.KIND_WHEEL: _wheel_verifier(doc, operator_wheels),
            },
            out=out,
        )
    except operator_check.ComponentRefused as e:
        raise OperatorSuppliedRefused(e.result.code, e.result.reason) from e


def _operator_supplied_record(results: "list[operator_check.Verification]", doc: dict, gmat_root: "str | None") -> dict:
    """`INSTALL_RECORD.installed.operator_supplied`: `gmat` (this step's detailed record of the one tree
    it verifies, as before) and `components`, the structured form of the printed list."""
    gmat = next((r for r in results if r.kind == operator_check.KIND_TREE and r.verified), None)
    return {
        "gmat": gmat.detail["record"] if gmat is not None else _gmat_not_needed_record(gmat_root),
        "step": "files install",
        "declared": list(doc["operator_supplied_declared"]),
        "components": [r.to_record() for r in results],
        "note": (
            "container images are verified by the host-side image step (install.sh --build-images), which writes "
            "IMAGE_BUILD_RECORD; this install holds no Docker socket and does not read that record"
        ),
    }


def _stage_operator_wheels(results: "list[operator_check.Verification]", target_dir: Path) -> list[dict]:
    """Copies each verified operator-supplied wheel to `<target>/operator-wheels/` and re-hashes the copy:
    pip installs from the copy, so what is installed is what was hashed (module doc, "An operator-supplied
    wheel"). Returns `INSTALL_RECORD.installed.operator_wheels`."""
    staged: list[dict] = []
    verified = [r for r in results if r.kind == operator_check.KIND_WHEEL and r.verified]
    if not verified:
        return staged
    dest_dir = target_dir / "operator-wheels"
    dest_dir.mkdir()
    for r in sorted(verified, key=lambda r: r.component):
        source, filename, sha256 = Path(r.detail["path"]), r.detail["filename"], r.detail["sha256"]
        dest = dest_dir / filename
        shutil.copyfile(source, dest)
        copied = operator_check.sha256_of_file(dest)
        if copied != sha256:
            raise OperatorSuppliedRefused(
                "wheel-changed-during-install",
                f"{source} had sha256:{sha256} when it was verified and its copy has sha256:{copied}: the operator's "
                f"file changed during the install. Nothing was installed from it.",
            )
        staged.append({
            "component": r.component, "filename": filename, "sha256": sha256,
            "operator_path": str(source), "installed_from": f"operator-wheels/{filename}",
        })
    return staged


def _images_record(doc: dict, kit_manifest_sha256: str) -> dict:
    """What `INSTALL_RECORD` says about images: this install built none (it has no path to the
    daemon), which step does, and what that step must reproduce for each image."""
    components = {}
    for component, entry in sorted(doc["images"].items()):
        components[component] = {
            "collected": entry.get("collected", False),
            "base_reference": (entry.get("base") or {}).get("reference"),
            "expected_runtime_content_hash": entry.get("runtime_content_hash"),
            "default_tag": image_build.default_tag(component, kit_manifest_sha256),
        }
    return {
        "built": False,
        "built_by": None,
        "reason": (
            "the files install runs without the Docker socket or DOCKER_HOST (question 250(a)); images are built on "
            "the Docker host by the kit's own image step, from bases the operator loaded"
        ),
        "build_step": "<kit>/installer/install.sh --build-images <kit> [--image-tag COMPONENT=REF ...]",
        "components": components,
    }


def check_operator_supplied(
    kit_dir: Path, *, gmat_root: "str | None" = None, gmat_platform: "str | None" = None,
    pins_file: "Path | None" = None, out: "Callable[[str], None] | None" = None,
    operator_wheels: "str | None" = None,
) -> "list[operator_check.Verification]":
    """`install.sh --check-operator-supplied`: verifies the kit, then the operator-supplied components
    it needs (this step's share of them), printing the list through `out`, and installs nothing. Raises
    `InstallRefused` exactly as `install` would for the same inputs."""
    kit_dir = kit_dir.resolve()
    doc = _load_manifest_or_refuse(kit_dir)
    _check_required_sections(doc)
    return verify_operator_supplied(
        doc, gmat_root=gmat_root, gmat_platform=gmat_platform, pins_file=pins_file, out=out,
        operator_wheels=operator_wheels,
    )


def install(
    kit_dir: Path, target_dir: Path, *, gmat_root: "str | None" = None, gmat_platform: "str | None" = None,
    pins_file: "Path | None" = None, out: "Callable[[str], None] | None" = None,
    operator_wheels: "str | None" = None,
) -> dict:
    """Runs the whole install: verify, refuse-checks (every operator-supplied component the kit needs
    that this step can reach, GMAT today), copy, chmod, pip install, INSTALL_RECORD.
    Returns the INSTALL_RECORD dict (also written to `<target_dir>/INSTALL_RECORD`). Raises
    `InstallRefused` for every named refusal condition -- callers (this module's own `main`, and
    tests) get one typed exception, never a bare traceback for a condition this function itself
    detected. `out`, when given, receives the operator-supplied list line by line as it is produced
    (`main` prints it), so a refusal still shows what was found up to it."""
    kit_dir = kit_dir.resolve()
    target_dir = target_dir.resolve()

    doc = _load_manifest_or_refuse(kit_dir)
    kit_manifest_sha256 = kit_manifest.sbom.sha256_file(kit_dir / "KIT_MANIFEST")
    _check_required_sections(doc)
    _check_required_wheels(doc)
    _check_viewer_assets_in_wheel(kit_dir, doc)
    # Question 250(a), tasks 2 and 3: before the target directory exists, so a refusal leaves nothing behind.
    operator_results = verify_operator_supplied(
        doc, gmat_root=gmat_root, gmat_platform=gmat_platform, pins_file=pins_file, out=out,
        operator_wheels=operator_wheels,
    )
    _check_target_dir(target_dir)

    operator_wheels_staged = _stage_operator_wheels(operator_results, target_dir)
    installed_kit_dir = _copy_kit_tree(kit_dir, target_dir)
    binaries_made_executable = _make_binaries_executable(installed_kit_dir)
    if operator_wheels_staged:
        wheels_info = _install_wheels_into_venv(
            kit_dir, target_dir, [target_dir / w["installed_from"] for w in operator_wheels_staged],
        )
    else:
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
            # Only packs whose bytes the kit carried are installed; a descriptor-only pack (data-time by
            # default, and GMAT always: operator-supplied) is named separately, so the record never says
            # a pack was installed that was not.
            "packs": sorted(n for n, e in doc["packs"].items() if e.get("copied")),
            "packs_descriptor_only": sorted(n for n, e in doc["packs"].items() if not e.get("copied")),
            "wheels": wheels_info,
            # The wheels the kit does not carry (a GPL-family library inside) and the operator supplied, as installed.
            "operator_wheels": operator_wheels_staged,
            "vendor_collected": doc["vendor"]["collected"],
            "sboms": sorted(doc["sboms"].keys()),
            "runs": sorted(doc["runs"].keys()),
            "suite_site_files": ["suite.merged.toml", "secsite.merged.toml"],
        },
        "gaps": doc["gaps"],
    }
    record["installed"]["images"] = _images_record(doc, kit_manifest_sha256)
    # One entry per component of the pins file, each the result of its own verification function.
    record["installed"]["operator_supplied"] = _operator_supplied_record(operator_results, doc, gmat_root)
    record_path = target_dir / "INSTALL_RECORD"
    record_path.write_text(json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")
    return record


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("kit_dir", type=Path, help="an existing kit directory (KIT_MANIFEST + content)")
    p.add_argument("target_dir", type=Path, help="a fresh or empty directory to install into")
    p.add_argument(
        "--gmat-root", metavar="PATH", default=None,
        help="the operator's unpacked GMAT R2026a; required when the kit has packs.gmat. Verified against the "
             "pin for --gmat-platform, never copied",
    )
    p.add_argument(
        "--gmat-platform", metavar="KEY", default=None,
        help="the platform of that GMAT, <os>-<uname -m> (e.g. macos-arm64); required with --gmat-root and never "
             "guessed from this host, which may be a Linux container over a macOS tree",
    )
    p.add_argument("--operator-wheels", metavar="DIR", default=None, help=OPERATOR_WHEELS_HELP)
    return p


def _print_line(line: str) -> None:
    print(line, flush=True)


def build_check_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="install.sh --check-operator-supplied",
        description="Verify the operator-supplied components this step can reach (GMAT) and print the whole list "
                    "(operator_check.py), without installing anything.",
    )
    p.add_argument("kit_dir", type=Path, help="an existing kit directory (KIT_MANIFEST + content)")
    p.add_argument("--gmat-root", metavar="PATH", default=None, help="the operator's unpacked GMAT R2026a")
    p.add_argument("--gmat-platform", metavar="KEY", default=None, help="the platform of that GMAT, <os>-<uname -m>")
    p.add_argument("--operator-wheels", metavar="DIR", default=None, help=OPERATOR_WHEELS_HELP)
    return p


def main(argv: "list[str] | None" = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv and argv[0] in IMAGE_STEP_FLAGS:
        return image_build.main(argv)
    if argv and argv[0] == CHECK_FLAG:
        args = build_check_parser().parse_args(argv[1:])
        try:
            check_operator_supplied(
                args.kit_dir, gmat_root=args.gmat_root, gmat_platform=args.gmat_platform, out=_print_line,
                operator_wheels=args.operator_wheels,
            )
        except InstallRefused as e:
            print(f"scripts/kit/install.py {CHECK_FLAG}: REFUSED -- {e}", file=sys.stderr)
            return 2
        return 0
    args = build_parser().parse_args(argv)
    try:
        record = install(
            args.kit_dir, args.target_dir, gmat_root=args.gmat_root, gmat_platform=args.gmat_platform, out=_print_line,
            operator_wheels=args.operator_wheels,
        )
    except InstallRefused as e:
        print(f"scripts/kit/install.py: REFUSED -- {e}", file=sys.stderr)
        return 2
    print(f"installed kit {args.kit_dir} -> {args.target_dir}")
    print(f"kit manifest sha256: {record['kit_manifest_sha256']}")
    gmat = record["installed"]["operator_supplied"]["gmat"]
    if gmat["verified"]:
        print(f"GMAT at {gmat['root']} verified against the {gmat['platform']} pin (not copied)")
    else:
        print("GMAT not needed by this kit")
    for w in record["installed"]["operator_wheels"]:
        print(f"operator-supplied wheel {w['filename']} (sha256:{w['sha256'][:12]}…) verified and installed from {w['installed_from']}")
    print(f"wrote {args.target_dir / 'INSTALL_RECORD'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
