"""D3's second half (docs/p5-plan.md, P5 track round 2 tasks 3b and 3c, round 3 question
217(b)): the zero-egress install proof. Task 3c closed the one gap task 3b's own harness had to
work around by hand; round 3 closed task 3c's OWN remaining gap in turn -- see "The viewer's own
static assets now come from the WHEEL alone" below for the full history and what is true now.

Modelled directly on `tests/test_edge_plugin_container.py` (read that file first) -- the same
labelled `--internal` bridge network, the same `ResourceGuard`/`prune_stale_labelled_resources`/
`lock_docker_tests` discipline, the same raw-connect egress probe idiom, and the same
`_wait_for_container_stdout_lines`-style bounded polling. Its conventions are reused; its text is
not -- every helper below is written fresh for this file's own topology (a kit installed from
scratch inside the enclave, not an already-built plugin image run directly).

# What this file proves, and in what order

1. **Base provenance (question 250(a)).** The kit carries no image and no base layer: only each
   image's application layer (`images/<component>/`). The two operator-supplied BASE images come
   from separately saved tarballs OUTSIDE the kit (`out/kit/proof-bases/`, made on the connected
   machine by `scripts/kit/save_base_images.py`, which `docker save`s each pinned base BY DIGEST and
   writes a sha256 list). The test re-checks that list, loads each tarball on the HOST under
   `lock_docker_tests()` through `ImageTagGuard.load_base_verified` (no host tag moves; a pinned
   image already on the host is verified, not re-loaded), and asserts the id equals the pin. It also
   asserts the kit holds no `images/*.tar` and no file whose sha256 equals a (non-empty) file of
   either base image (a quick per-file check; task 7 writes the full inventory test).
2. A freshly created, labelled **`--internal`** bridge network -- no route off this host, proven
   (the egress probe below), not merely named.
3. The egress probe, from inside that network, BEFORE any install activity and again AFTER --
   both times a raw connect to two real public addresses fails immediately (< 1s) with an
   ENETUNREACH-shaped error. Naming a public address on an `--internal` network touches no actual
   network (there is no default route to even attempt sending a packet), which is why this
   satisfies question 154 despite the addresses being real.
4. **The files install**, inside a labelled container on that same network, with the kit
   bind-mounted **read-only**: `<kit>/installer/install.sh <kit> <fresh-dir-in-a-labelled-volume>`,
   using `--no-index` for the one `pip install` it runs. Must succeed with the container's OWN
   network already proven to have no route out. **The container never gets the Docker socket,
   `DOCKER_HOST` or any other path to the daemon** (the lead's ruling: such a container could start
   a networked container and defeat the `--internal` isolation this proof rests on):
   `_assert_no_docker_daemon_access` checks the very argument list handed to `docker run`, the
   daemon's own record of the container (`docker inspect`: binds, env, privileged) is checked
   afterwards, and a probe container with the same mounts looks for a socket and `DOCKER_HOST`.
4b. **The image step**, on the HOST, from the kit's OWN bundled installer
   (`<kit>/installer/install.py --build-images`, not this repository's `scripts/kit/`): it builds
   both images from the application layer on the loaded bases with `docker build --network none
   --pull=false`, and trusts each only after its runtime-content hash (question 185), file modes
   and `.Config` equal what the kit records. Proved around it: `docker events` over the whole
   window shows no `pull` and no `start` of any container the step created; a probe build through
   the same code path whose `RUN` tries the network fails with the network-unreachable error
   (`--probe-offline-build`); each built id did not exist before; and the runtime-content hash is
   recomputed here by a DIFFERENT implementation (`sha256sum` inside the image, as the
   `build-image.sh` scripts do, formatted and sorted by this file) and compared with the recorded
   one; `.Config` is compared with the recorded `:local` images'. Two perturbations: a kit whose
   pinned base digest names an image the host does not hold is refused before any build (a typed
   `base-image-missing` refusal; the build is never issued, so nothing can reach a registry); and an
   attacker-consistent kit (one application file changed, every hash in `KIT_MANIFEST` re-computed so
   it verifies) builds and is then refused on the runtime-content hash, naming both hashes, leaving
   no tag behind.
5. **The demo path**, run from the just-installed tree, inside the same network, in order:
   a. the viewer server, started from the INSTALLED wheels (`altavista.server.create_app`, the
      package `python -m altavista` itself is), proven alive by a real HTTP GET over loopback from
      a second container sharing its network namespace;
   b. `av-ingest-server`, started from the kit's own cross-built `binaries/av-ingest-server`,
      proven alive by its own `GRPC_LISTENING`/`ADMIN_LISTENING` readiness lines;
   c. the edge plugin, from the image the kit's own image step just built (step 4b; not
      whatever tag happened to already be on the host), joined to the ingest's network namespace
      (`--network container:<ingest>`), delivering the pinned demo-ground-segment batch set that
      `crates/av-edge/tests/fixtures/ground_segment/README.md` documents and
      `tests/test_edge_plugin_container.py` already proves the identical numbers for;
   d. the recorded kernel runs (`tests/fixtures/*.runproducts.bin`, carried in the kit's own
      `runs/`), re-hashed INSIDE the installed tree and compared to what `KIT_MANIFEST` records,
      decoded far enough (`build_kit.read_run_provenance`, reused, not reimplemented) to assert
      each fixture's own `config_hash` field, not merely its file hash;
   e. **`av-command`, round 3 (question 217(a)): no longer a named gap.** It used to be -- its
      cross-build failed against the pinned `rust:1.85-bookworm` toolchain (`regorus` 0.12.0's own
      MSRV is 1.87). `scripts/kit/build_kit.py::PREBUILD_BASE_IMAGE` moved to `rust:1.90-bookworm`
      this round (a review defect: every OTHER cross-build pin in this workspace had already moved
      there, question 215) and `av-command` now cross-builds cleanly. This test proves the binary
      genuinely RUNS -- invoked with no arguments inside the enclave, checked for its own
      well-defined missing-OIDC-config refusal (`crates/av-command/src/bin/av-command.rs`, A2.1) --
      never merely that it is present. It still does not stand up the full gRPC/admin service (that
      needs a real OIDC issuer/public key this test does not have, and this task's own rules forbid
      editing `crates/av-command` to add one) -- if the kit's own cross-build ever fails again for
      some other reason, this section reads and reports the kit's own recorded gap instead, rather
      than assuming success.
6. **Two kits built from the same commit have the same manifest hash** -- a separate, UNGATED,
   fast test (`test_two_fast_kits_from_the_same_commit_have_the_same_manifest_hash`, below): builds
   two minimal kits (default pack only, no gated flags) into `tmp_path` and compares their
   `KIT_MANIFEST` bytes. Deliberately not the expensive, image/wheel/vendor/binary-bearing kit this
   file's main test consumes -- that one is built once, out-of-band, and reused (see "Gating",
   below), exactly the way `tests/test_kit_manifest.py::
   test_two_image_bearing_kits_from_the_same_commit_have_the_same_manifest_and_tarball_hashes`
   already treats its own expensive case separately from the cheap default-gate one.

# The viewer's own static assets now come from the WHEEL alone (round 3, question 217(b)) -- the
# full history

Task 3b's own first cut of this test proved everything EXCEPT the viewer's own static/config
assets came from the kit alone: `altavista/server.py::create_app` eagerly constructs `starlette.
staticfiles.StaticFiles(directory=web_dir)` for the frontend, and (at the time) `altavista/
profile.py`'s `PROFILES_DIR` was a fixed constant (`Path(__file__).resolve().parent.parent /
"profiles"`). Neither `web/` nor `profiles/` was part of the `altavista` wheel then
(`pyproject.toml`'s `[tool.setuptools.packages.find]` only ever included `altavista*` --
confirmed directly: `python -m altavista serve` from a wheel-only install was unable to even
construct its own ASGI app, `StaticFiles.__init__` raising `RuntimeError: Directory '.../web'
does not exist` before a single route was registered). Task 3b's own harness worked around this
by bind-mounting THIS WORKTREE's own `web/`/`profiles/` into the viewer's container, read-only --
a real, if narrow, hole in that round's headline "the demo path runs from the installed kit
alone" claim, since a genuinely air-gapped install with no access to this worktree could not have
started the viewer that way.

**Task 3c** closed that hole at the kit/install layer instead of the packaging layer:
`scripts/kit/manifest.py` gained two new named packs, `web` and `profiles`, copied into every kit
UNCONDITIONALLY, and `_run_viewer_from_installed_kit` pointed `web_dir`/`PROFILES_DIR` at those
INSTALLED pack paths. That closed the "bind-mounts this worktree" hole, but left the underlying
gap on record for the lead: the `altavista` WHEEL itself still shipped no `web/`/`profiles/`, and
`altavista.profile.PROFILES_DIR` was still a fixed module constant with no parameter or CLI flag
-- monkeypatching it was the only way to point it anywhere else, so task 3c's own version of
`_viewer_bootstrap_script` did exactly that, against the installed pack copy.

**Round 3 (question 217(b), the lead's ruling) closes the gap for real, at the packaging layer**:
`pyproject.toml`/`setup.py` (see both files' own docs) now ship `web/` and `profiles/` INSIDE the
`altavista` wheel itself, as `altavista/web/`/`altavista/profiles/`; `altavista/profile.py` gained
an ordered `resolve_profiles_dir` search (explicit argument, `ALTAVISTA_PROFILES_DIR` env var,
the module constant if assigned, the packaged copy, the in-repo copy -- see that module's own
doc) that finds the packaged copy automatically with NO override of any kind; `altavista/
server.py`'s `WEB_DIR` gained the equivalent packaged-copy-first default. Because of this, the two
`web`/`profiles` packs task 3c added are GONE again (`manifest.PACKS`, `kit_format` bumped 2 -> 3
-- see `manifest.py`'s own top doc) and `_run_viewer_from_installed_kit` below no longer
monkeypatches `altavista.profile.PROFILES_DIR` or passes `web_dir=` pointing at a kit pack at
all: it calls `create_app()` with neither argument, and the installed wheel's own default
resolution finds both entirely on its own. `INSTALLED_WEB_DIR`/`INSTALLED_PROFILES_DIR` (the old
pack-path constants) are gone along with the packs they named. The proof is strengthened to
match: a real `GET /index.html`/`GET /` and a real `GET /node_modules/three/three.module.js`
(status 200, non-trivial body length -- what actually proves `web/` came out of the wheel,
symlinks included, not merely that SOME static root exists), plus a real profile
(`profiles/design.yaml`) loaded through `altavista.profile`'s own loader, inside the container,
against nothing but the installed wheel.

Nothing from this worktree, and nothing from `kit/packs/`, is bind-mounted or referenced for the
viewer anywhere in this file any more -- every byte it serves, and every line of code that runs,
is the installed wheel's own.

# The kit now carries its own installer too (round 3, question 217(c)) -- what changed here

Every task 3b/3c/round-3(b) revision of this file, up to and including this paragraph's own
predecessor, bind-mounted THIS repository's own `scripts/kit/` (read-only) into the installing
container alongside the kit itself, then ran `/repo/scripts/kit/install.sh /kit /target` --
`scripts/kit` was never part of the kit, so a genuinely air-gapped party holding only the kit
(never this repository) could not have run this exact install. Round 2's own decision 9 recorded
that gap openly; question 217(c) closes it. `build_kit.py::assemble_installer` now copies
`install.sh`/`install.py` and the local modules they import into `<kit>/installer/`,
unconditionally, every kit (`kit_format` bumped 3 -> 4, `manifest.py`'s own top doc). This file's
own install step below now runs `/kit/installer/install.sh /kit /target` from the read-only KIT
MOUNT ALONE -- `scripts/kit/` (`KIT_DIR`) is never bind-mounted into the container at all any more
(it is still used, exactly as before, to `sys.path.insert` this TEST's own host-side import of
`build_kit`/`manifest`/`sbom`, which has nothing to do with what the container receives).
`_assert_no_repo_source_mounted`, below, is the positive proof: it inspects the real `-v` argument
list handed to `docker run` for that container and asserts no host path in it is this repository's
own live source tree, with the kit artifact itself (`_PROOF_KIT_DIR`, already hash-verified before
this point) the one named, deliberate exception. See `manifest.py`'s own top doc, "P5 track round
3, question 217(c)", and `install.py`'s own module doc for exactly what a kit carrying its own
installer proves (internal consistency) and does not prove (authenticity -- that anchor is
`KIT_MANIFEST`'s own SHA-256, held independently of the kit, which is why this file always prints
and cross-checks it rather than trusting anything the kit says about itself).

# Gating (question 194) -- computed once, at import time

`_compute_skip_reason()` requires: Docker; the `python:3.13-slim` probe image already present
(never pulled here, question 154); the EXPENSIVE proof kit already built, out-of-band, at
`out/kit/proof` (`_KIT_DIR`), with its `images`/`wheels`/`binaries`/`packs` sections all actually
collected and its own manifest verifying clean; the saved base-image tarballs at
`out/kit/proof-bases/` (`_PROOF_BASES_CMD` makes them); and both recorded `:local` images on the
host at their recorded ids (their `.Config` is what the built images are compared with). Building
the kit takes real minutes (a Rust cross-build, `cargo vendor`, a `pip download` against PyPI) and
reaches the network (question 154's one build-time exception) -- doing that inside a test would
violate "no network at test time" outright, so this test never builds it; it only ever consumes an
already-built one and skips, by name, with the exact command that builds it, when it is missing. This mirrors `tests/
test_edge_plugin_container.py::_compute_skip_reason` precisely: run for real, or skip visibly
naming why, never silently pass (question 194).

# Question 199 (no test mutates the process environment)

Nothing below writes `os.environ`. Every subprocess call either forwards no extra environment at
all, or receives values as CLI arguments / bind-mounted files -- never a mutated copy of this
process's own environment handed to `env=`.

# Question 156/207 (labels, prune, host-wide lock)

Every container, network, and volume this file creates carries `av.test`/`av.test.run_id`
(`ResourceGuard`, below -- the identical shape `tests/test_edge_plugin_container.py::
ResourceGuard` already establishes); `lock_docker_tests()` is held for the whole test body;
`prune_stale_labelled_resources()` runs before creating anything; `guard.assert_nothing_left()`
runs after `guard.cleanup()`, proving nothing labelled by this run survives.

# Question 234 (round 5) -- `docker load` never leaves a repository tag this run did not own

`docker load -i` restores the tarball's OWN repository tag, host-wide, unconditionally --
`_load_and_verify_images` (B.1, above) `docker load`s this worktree's own proof kit, built at a
point in time that can predate whatever the host's own tags currently point at (the lead's own
root-caused incident: this test's `docker load` moved `av-edge-plugin:local`/
`altavista-cfs-lockstep:local` onto stale images from an earlier build, silently defeating another
track's `IMAGE_DIGEST.md` provenance gate, question 212(a), until the lead restored the tags by
hand -- a test mutating host state outside itself, question 199). `_load_and_verify_images` now
brackets every load with a `docker image ls`-based tag snapshot (`docker_tag_snapshot`, never
free-text `docker images` parsing) and, immediately after each load, undoes exactly the
`repository:tag -> id` binding(s) that load created or changed: `docker rmi <repository:tag>`
(untag only -- never a bare id, never `-f`; see `_load_and_verify_images`'s own doc comment on the
measured `docker rmi -f <ID>` behaviour that rules that out) when the binding did not exist
before, or `docker tag <previous_id> <repository:tag>` to restore it when the load moved an
existing one. The one component this file still references by tag afterward (`edge-plugin-image`,
used by both `_run_ingest_from_installed_kit` and `_run_edge_plugin_from_kit_image`) is re-tagged
under a test-only name (`ImageTagGuard.test_only_tag`) BEFORE that undo step runs, so undoing the host tag
never risks deleting the only reference to the image this test still needs -- that test-only tag
is what `av-edge-plugin:local` used to be handed to those two functions as, literally on the host,
and it is removed in `ResourceGuard.cleanup()` like any other resource this run created.
`ImageTagGuard.assert_host_tags_unchanged`, the test's own closing proof, runs inside the outer `finally` (so
it runs on the failure path too, not only on success -- exactly the case the lead hit: a test that
moves tags and then fails for an unrelated reason) and asserts every tag present before this run's
first `docker load` still points at the same id, and that no test-only tag this run created
survives.

Question 239: the snapshot, load-and-verify-by-id, restore and closing assertion are one
implementation, `scripts/kit/docker_image_tags.py::ImageTagGuard`, shared with
`scripts/kit/live_evidence.py` (which `docker load`s the same tarballs and had none of this); this
file keeps only the kit-specific parts (which components, the test-only tag prefix, `KIT_MANIFEST`).
The restore now also runs on the failure paths of a single load (load failed, tag missing, digest
mismatch), not only after a verified one.

Question 250(a): the kit carries no image tarball any more, so there is no kit tarball to `docker
load`; what is loaded is the two operator-supplied BASE tarballs (`ImageTagGuard.load_base_verified`,
which loads nothing already on the host and names nothing), and the images are built by the kit's
own installer under test-only tags (`_TEST_TAG_PREFIX/<component>:<run id>`), registered with both
`ResourceGuard` (removed by name at the end) and `ImageTagGuard.test_only_tags` (so
`assert_host_tags_unchanged` proves they are gone and that no other tag moved). Each built id is
checked absent from the host's image table before the build, so removing the tag removes only what
this run created.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tarfile
import time
import uuid
import zipfile
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import manifest as kit_manifest  # noqa: E402
import sbom  # noqa: E402
import save_base_images  # noqa: E402  (question 250(a): the operator's side of the bases)
from docker_image_tags import ImageTagGuard, docker_image_table  # noqa: E402  (question 239: shared with live_evidence.py)

from altavista.container_hardening import (  # noqa: E402
    TEST_LABEL_KEY,
    TEST_LABEL_VALUE,
    prune_stale_labelled_resources,
)
from altavista.docker_test_lock import lock_docker_tests  # noqa: E402

#: The already-built, expensive proof kit this test consumes -- see this module's own "Gating"
#: doc for why this test never builds it itself. `scripts/kit/README.md`'s own "A full kit"
#: example, with `--with-binaries` added and the (large, unnecessary for this proof) `gmat` pack
#: deliberately left out -- the demo path below never re-runs the kernel, only re-hashes its
#: already-recorded output, so the multi-hundred-MB GMAT pack buys this proof nothing.
_PROOF_KIT_DIR = REPO_ROOT / "out" / "kit" / "proof"
_PROOF_KIT_BUILD_CMD = (
    ".venv/bin/python scripts/kit/build_kit.py --out out/kit/proof "
    "--with-images --with-vendor --with-wheels --with-binaries --copy-pack data-time"
)

#: Question 250(a): the operator-supplied base images, saved by digest on the connected machine
#: (`scripts/kit/save_base_images.py`) -- NOT part of the kit, which carries no base layer.
_PROOF_BASES_DIR = REPO_ROOT / "out" / "kit" / "proof-bases"
_PROOF_BASES_CMD = ".venv/bin/python scripts/kit/save_base_images.py --out out/kit/proof-bases"

PROBE_IMAGE = "python:3.13-slim"

# Round 3 (question 217(b)): the marker paths inside the `altavista` wheel's own zip namelist that
# prove it carries the viewer's static/config assets -- identical to
# `scripts/kit/install.py::_VIEWER_ASSET_WHEEL_MARKERS`, restated here (not imported -- this file
# reuses that check's CONVENTION, not its text, exactly like every other helper in this module)
# because `_proof_kit_missing_sections` needs to skip visibly, by name, if the proof kit's own
# altavista wheel predates the round-3 packaging change, rather than failing deep inside
# `_run_viewer_from_installed_kit` with a confusing 404. Task 3c's `INSTALLED_WEB_DIR`/
# `INSTALLED_PROFILES_DIR` pack-path constants are gone along with the `web`/`profiles` packs
# themselves (see this module's own top doc) -- there is no separate installed path to name any
# more, only the wheel.
VIEWER_ASSET_WHEEL_MARKERS = ("altavista/web/index.html", "altavista/profiles/design.yaml")

GROUND_SEGMENT_FIXTURES = REPO_ROOT / "crates" / "av-edge" / "tests" / "fixtures" / "ground_segment"
SIGNING_KEY_PEM = GROUND_SEGMENT_FIXTURES.parent / "test_signing_key.pem"
VERIFY_PUB_PEM = GROUND_SEGMENT_FIXTURES.parent / "test_signing_key.pub.pem"

# Pinned demo-DRM values (crates/av-edge/tests/fixtures/ground_segment/README.md) -- the SAME
# constants `tests/test_edge_plugin_container.py` and `crates/av-ingest/tests/plugin_wire.rs`
# already pin, restated here (not imported from that file -- this module reuses its CONVENTIONS,
# not its text) because this proof drives the identical fixture through the kit's own installed
# ingest binary and the kit's own loaded plugin image.
PRODUCER_ID = "demo-ground-segment-flight-plugin"
SHARD_KEY = "ground-segment-demo"
EXPECTED_BATCH_COUNT = 900
EXPECTED_CHAIN_HEAD_HEX = "d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698"
CLEARANCE_LADDER = "UNCLASSIFIED,CUI"
MAX_BATCH_AGE_NS = 10_000_000_000_000
CLOCK_TAI_NS = 1_767_225_637_000_000_000

# See tests/test_edge_plugin_container.py's own module doc, "Why every bind-mount source lives
# under .av-test-tmp/, not tempfile/tmp_path" -- Colima mounts only $HOME; a bind-mount source
# outside it silently becomes an empty directory inside the container, never an error. Every
# scratch file this test bind-mounts (the viewer bootstrap script, the run-rehash script) lives
# under this, never under tempfile/tmp_path.
SCRATCH_ROOT = REPO_ROOT / ".av-test-tmp" / "kit_zero_egress_install"


# =================================================================================================
# Gating (question 194)
# =================================================================================================

def _docker_unavailable_reason() -> "str | None":
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"docker is not installed or could not be launched: {e}"
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip().splitlines()
        return f"`docker info` exited {result.returncode}: {detail[-1] if detail else '(no stderr)'}"
    return None


def _image_present(tag: str) -> bool:
    result = subprocess.run(["docker", "image", "inspect", tag, "--format", "{{.Id}}"], capture_output=True, timeout=30)
    return result.returncode == 0


def _proof_kit_missing_sections(doc: dict, kit_dir: Path) -> list[str]:
    missing = []
    if doc.get("kit_format") != kit_manifest.KIT_FORMAT:
        missing.append(f"kit_format (proof kit is {doc.get('kit_format')!r}, this worktree builds {kit_manifest.KIT_FORMAT!r})")
    for component in sbom.IMAGE_COMPONENTS:
        entry = doc.get("images", {}).get(component, {})
        if not entry.get("collected"):
            missing.append(f"images.{component}")
        elif not entry.get("runtime_set") or not entry.get("config"):
            missing.append(f"images.{component}.runtime_set/config (the installer's image step needs both)")
    if not doc.get("wheels", {}).get("collected"):
        missing.append("wheels")
    if not doc.get("vendor", {}).get("collected"):
        missing.append("vendor")
    if not doc.get("binaries", {}).get("results", {}).get("av-ingest-server", {}).get("included"):
        missing.append("binaries.av-ingest-server")
    pack = doc.get("packs", {}).get("data-time")
    if not pack or not pack.get("copied"):
        missing.append("packs.data-time (copied)")
    # Round 3 (question 217(b)): task 3c's `web`/`profiles` packs are gone (see this module's own
    # top doc) -- the equivalent check is now on the `altavista` WHEEL itself, the same one
    # `scripts/kit/install.py::_check_viewer_assets_in_wheel` runs at install time, restated here
    # (not imported -- see `VIEWER_ASSET_WHEEL_MARKERS`'s own comment) so a proof kit built before
    # round 3's packaging change (or with a stale/unpackaged wheel for any other reason) is
    # skipped with a clear, named reason rather than failing deep inside
    # `_run_viewer_from_installed_kit`.
    altavista_entry = next(
        (f for f in doc.get("wheels", {}).get("fetched", []) if f["name"].lower() == "altavista"),
        None,
    )
    if altavista_entry is None:
        missing.append("wheels.fetched (no altavista entry)")
    else:
        wheel_path = kit_dir / "wheels" / altavista_entry["filename"]
        try:
            with zipfile.ZipFile(wheel_path) as zf:
                names = set(zf.namelist())
        except (OSError, zipfile.BadZipFile):
            names = set()
        for marker in VIEWER_ASSET_WHEEL_MARKERS:
            if marker not in names:
                missing.append(f"altavista wheel missing {marker!r} (round 3 packaging change)")
    return missing


def _compute_skip_reason() -> "str | None":
    reason = _docker_unavailable_reason()
    if reason is not None:
        return f"Docker not available: {reason}"
    if not _image_present(PROBE_IMAGE):
        return (
            f"probe image {PROBE_IMAGE!r} is not present locally, and this test never pulls an "
            f"image itself (question 154: no network at test time). Run `docker pull "
            f"{PROBE_IMAGE}` once, on a host with network access, then re-run this test."
        )
    manifest_path = _PROOF_KIT_DIR / "KIT_MANIFEST"
    if not manifest_path.is_file():
        return (
            f"the zero-egress proof kit has not been built at {_PROOF_KIT_DIR} -- this test only "
            f"installs from an already-built kit, it never builds one itself (building one uses "
            f"the network -- question 154's one build-time exception -- and takes real minutes, "
            f"neither of which belongs in a test). Build it with: {_PROOF_KIT_BUILD_CMD}"
        )
    try:
        doc = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        return f"{manifest_path} could not be read/parsed ({e}) -- rebuild with: {_PROOF_KIT_BUILD_CMD}"
    missing = _proof_kit_missing_sections(doc, _PROOF_KIT_DIR)
    if missing:
        return (
            f"the kit at {_PROOF_KIT_DIR} exists but is missing expensive section(s) {missing!r} "
            f"this proof needs -- rebuild with: {_PROOF_KIT_BUILD_CMD}"
        )
    findings = kit_manifest.verify_manifest(_PROOF_KIT_DIR)
    if findings:
        return (
            f"the kit at {_PROOF_KIT_DIR} does NOT verify against its own KIT_MANIFEST "
            f"({len(findings)} finding(s)) -- rebuild with: {_PROOF_KIT_BUILD_CMD}"
        )
    try:
        save_base_images.verify_sums(_PROOF_BASES_DIR)
        wanted = {f"{pin_id}.tar" for pin_id in save_base_images.base_pins()}
        have = {n for n in (_PROOF_BASES_DIR / "SHA256SUMS").read_text().split() if n.endswith(".tar")}
    except (save_base_images.SaveRefused, OSError) as e:
        return (
            f"the saved base-image tarballs are not at {_PROOF_BASES_DIR} ({e}) -- the kit carries no base "
            f"layer (question 250(a)), so this proof loads the operator's bases from tarballs saved by digest "
            f"outside it. Make them on a machine that holds the pinned images with: {_PROOF_BASES_CMD}"
        )
    if wanted != have:
        return f"{_PROOF_BASES_DIR}/SHA256SUMS lists {sorted(have)}, expected {sorted(wanted)} -- re-make them with: {_PROOF_BASES_CMD}"
    for component in sbom.IMAGE_COMPONENTS:
        entry = doc["images"][component]
        got = subprocess.run(["docker", "image", "inspect", entry["tag"], "--format", "{{.Id}}"], capture_output=True, text=True, timeout=30)
        if got.returncode != 0 or got.stdout.strip() != entry["recorded_digest"]:
            return (
                f"the recorded image {entry['tag']} is not on this host at its recorded id {entry['recorded_digest']} "
                f"(found {got.stdout.strip() or got.stderr.strip()!r}); the built images' .Config is compared with it. "
                f"Rebuild it with its services/*/build-image.sh (under the docker lock) and re-record."
            )
    return None


_SKIP_REASON = _compute_skip_reason()

_MISSING_IMAGE_MARKER = "Unable to find image"


# =================================================================================================
# Small docker helpers -- no process-environment mutation anywhere (question 199).
# =================================================================================================

def _docker(*args: str, timeout: float = 60.0, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        if _MISSING_IMAGE_MARKER in (result.stderr or ""):
            pytest.skip(
                f"an image this test depends on vanished from this host mid-run (host-level "
                f"image garbage collection under disk pressure -- see tests/"
                f"test_edge_plugin_container.py's own module doc, question 196(d), for the "
                f"measured phenomenon) while running `docker {' '.join(args[:3])}` -- not a "
                f"defect in what is under test. Docker's own words: "
                f"{result.stderr.strip().splitlines()[0] if result.stderr.strip() else '(no stderr)'}"
            )
        pytest.fail(f"`docker {' '.join(args)}` failed (rc={result.returncode}):\n--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}")
    return result


def _labelled_id(run_id: str, kind: str) -> str:
    return f"av-kit-install-test-{kind}-{run_id}"


class ResourceGuard:
    """The identical shape `tests/test_edge_plugin_container.py::ResourceGuard` already
    establishes (see that file's own doc) -- tracks exactly what this run created and removes
    exactly that, cross-checked by label afterward."""

    def __init__(self, run_id: str):
        self.run_id = run_id
        self.containers: list[str] = []
        self.networks: list[str] = []
        self.volumes: list[str] = []
        #: Round 5 (question 234): test-only image TAGS this run created (`ImageTagGuard.test_only_tag`,
        #: `_load_and_verify_images`) -- always a `repository:tag` NAME, never a bare id, so
        #: cleanup below can only ever untag, never trigger the `docker rmi -f <ID>` behaviour
        #: `_load_and_verify_images`'s own doc comment records (strips every tag on that id).
        self.images: list[str] = []

    def label_args(self) -> list[str]:
        return ["--label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--label", f"av.test.run_id={self.run_id}"]

    def track_container(self, name: str) -> None:
        self.containers.append(name)

    def track_network(self, name: str) -> None:
        self.networks.append(name)

    def track_volume(self, name: str) -> None:
        self.volumes.append(name)

    def track_image(self, tag: str) -> None:
        self.images.append(tag)

    def cleanup(self) -> None:
        for name in self.containers:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=30)
        for tag in self.images:
            subprocess.run(["docker", "rmi", tag], capture_output=True, timeout=30)
        for name in self.networks:
            subprocess.run(["docker", "network", "rm", name], capture_output=True, timeout=30)
        for name in self.volumes:
            subprocess.run(["docker", "volume", "rm", name], capture_output=True, timeout=30)

    def assert_nothing_left(self) -> None:
        label_filter = f"label=av.test.run_id={self.run_id}"
        remaining_containers = _docker("ps", "-a", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_containers == "", f"container(s) labelled {label_filter} still exist after cleanup: {remaining_containers!r}"
        remaining_networks = _docker("network", "ls", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_networks == "", f"network(s) labelled {label_filter} still exist after cleanup: {remaining_networks!r}"
        remaining_volumes = _docker("volume", "ls", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_volumes == "", f"volume(s) labelled {label_filter} still exist after cleanup: {remaining_volumes!r}"
        # The images the kit's installer builds carry the kit's own labels, not this run's `av.test`
        # labels, and are removed by name above (`images`); confirm none carries this run's label
        # rather than silently assuming it.
        remaining_images = _docker("images", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_images == "", f"no image should ever carry {label_filter}, but found: {remaining_images!r}"


def _wait_for_container_log_substring(name: str, substrings: tuple[str, ...], timeout_s: float) -> list[str]:
    """Polls `docker logs` (bounded deadline, never a bare `sleep N` guess) until every one of
    `substrings` has appeared SOMEWHERE in the accumulated log text -- a substring check, not
    `tests/test_edge_plugin_container.py::_wait_for_container_stdout_lines`'s own line-PREFIX
    check, because uvicorn's own log lines carry no fixed prefix this file controls."""
    deadline = time.monotonic() + timeout_s
    while True:
        result = _docker("logs", name, check=False)
        # `docker logs` preserves the container's own stdout/stderr split -- uvicorn's own INFO
        # lines (e.g. "Uvicorn running on ...") go to the container's stderr via Python logging's
        # default handler, so both streams must be checked, not just stdout.
        logs = result.stdout + result.stderr
        if all(s in logs for s in substrings):
            return logs.splitlines()
        if time.monotonic() > deadline:
            pytest.fail(f"container {name} never printed all of {substrings} within {timeout_s}s -- logs so far:\n{logs}")
        time.sleep(0.1)


_RAW_CONNECT_PROBE_SCRIPT = (
    "import socket, time, json\n"
    "targets = [('8.8.8.8', 53), ('1.1.1.1', 443)]\n"
    "out = []\n"
    "for host, port in targets:\n"
    "    t0 = time.monotonic()\n"
    "    try:\n"
    "        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n"
    "        s.settimeout(3)\n"
    "        s.connect((host, port))\n"
    "        out.append((host, port, {'failed': False, 'elapsed_s': time.monotonic() - t0, 'message': 'connected (unexpected)'}))\n"
    "    except OSError as e:\n"
    "        out.append((host, port, {'failed': True, 'elapsed_s': time.monotonic() - t0, 'message': str(e), 'errno': e.errno}))\n"
    "print(json.dumps(out))\n"
)

_EVIDENCE_FETCH_SCRIPT = (
    "import urllib.request, json\n"
    "data = json.loads(urllib.request.urlopen('http://127.0.0.1:50071/admin/api/evidence', timeout=5).read())\n"
    "print(json.dumps(data))\n"
)


def _assert_no_repo_source_mounted(mount_args: list[str]) -> None:
    """Question 217(c)'s own positive assertion: `mount_args` is the exact list of `-v` arguments
    about to be handed to `docker run` for the installing container, built in ONE place (the call
    site, below) so this function is checking the real thing, never a description of it. Parses
    every `-v <host>:<container>[:ro]` pair and asserts no HOST-side path is this repository's own
    live source tree -- most importantly, that `scripts/kit/` (`KIT_DIR`) is never among them: task
    3b's own harness used to bind-mount it read-only into this same container (round 2's decision
    9, an openly recorded gap), which is exactly what question 217(c) closes.

    `_PROOF_KIT_DIR` is the one deliberate exception, not a loophole: it is the already-built kit
    ARTIFACT this test consumes (under this repo's own `.gitignore`d `out/` build-output directory
    on disk, but not source this repository tracks or ships), already proven trustworthy by
    `_load_and_verify_images` and by the kit's own `KIT_MANIFEST` SHA-256 before this ever runs --
    see `manifest.py`'s own top doc, "P5 track round 3, question 217(c)", for exactly what that
    proves and does not prove. A docker VOLUME name (e.g. the install target) is not a host
    filesystem path at all and is skipped rather than mis-resolved against this process's own cwd."""
    for i, arg in enumerate(mount_args):
        if arg != "-v":
            continue
        host_spec = mount_args[i + 1].split(":")[0]
        if not host_spec.startswith("/"):
            continue  # a docker volume NAME, not a host filesystem path
        host_path = Path(host_spec).resolve()
        if host_path == _PROOF_KIT_DIR.resolve():
            continue  # the one deliberate exception -- see this function's own doc
        assert host_path != REPO_ROOT and REPO_ROOT not in host_path.parents, (
            f"a container mount points inside this repository's own source tree ({host_path}) -- "
            f"question 217(c): the installing container must get everything it needs from the "
            f"kit alone, never a live bind-mount of this repo"
        )


def _run_egress_probe(guard: ResourceGuard, run_id: str, stage: str, network_name: str) -> list:
    probe_container = _labelled_id(run_id, f"egress-probe-{stage}")
    guard.track_container(probe_container)
    raw = _docker(
        "run", "--rm", "--name", probe_container, "--network", network_name, *guard.label_args(),
        PROBE_IMAGE, "python3", "-c", _RAW_CONNECT_PROBE_SCRIPT,
    )
    result = json.loads(raw.stdout.strip().splitlines()[-1])
    for host, port, outcome in result:
        assert outcome["failed"], f"[{stage}] a raw connect to {host}:{port} should fail on an --internal network, got: {outcome}"
        assert outcome["elapsed_s"] < 1.0, f"[{stage}] connect to {host}:{port} took {outcome['elapsed_s']:.3f}s -- should be immediate (no route exists to even attempt)"
        assert "unreachable" in outcome["message"].lower() or outcome.get("errno") == 101, f"[{stage}] expected an ENETUNREACH-shaped failure for {host}:{port}, got: {outcome}"
    return result


# =================================================================================================
# The test.
# =================================================================================================

@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_zero_egress_install_and_demo_from_the_kit_alone():
    with lock_docker_tests():
        _run_zero_egress_install_and_demo_from_the_kit_alone()


#: Round 5 (question 234): a name no other track's own image tag can collide with -- `run_id` is
#: this test run's own `uuid.uuid4().hex[:12]`. Question 250(a): the names the kit's own image step
#: builds the two images under (`_test_image_tag`), never the kit's recorded `:local` tags.
_TEST_TAG_PREFIX = "av-kit-zero-egress-test"

#: The kit's OWN bundled installer, run on the HOST for the image step (never this repository's
#: `scripts/kit/`); `-I` (isolated mode) so that nothing but the kit's directory can supply a module.
_KIT_INSTALLER_PY = Path("installer") / "install.py"


def _test_image_tag(run_id: str, component: str) -> str:
    return f"{_TEST_TAG_PREFIX}/{component}:{run_id}"


def _load_bases(image_tags: ImageTagGuard) -> list[dict]:
    """B.1 (question 250(a)): the operator-supplied base images come from the separately saved
    tarballs, not from the kit. Each tarball's sha256 is re-checked against the saved list, then
    `ImageTagGuard.load_base_verified` loads it on the host (or, when the pinned image is already
    there, verifies that one rather than adding a reference to it) and the id is compared with the
    pin; the digest-qualified reference the kit's Dockerfiles start FROM must resolve to the pin."""
    save_base_images.verify_sums(_PROOF_BASES_DIR)
    evidence = []
    for pin_id, pin in save_base_images.base_pins().items():
        tarball = _PROOF_BASES_DIR / f"{pin_id}.tar"
        assert tarball.is_file(), tarball
        loaded = image_tags.load_base_verified(component=pin_id, tarball=tarball, expected_id=pin["digest"])
        assert loaded["host_id"] == pin["digest"], loaded
        by_reference = _docker("image", "inspect", pin["reference"], "--format", "{{.Id}}").stdout.strip()
        assert by_reference == pin["digest"], f"{pin['reference']} resolves to {by_reference}, not the pinned {pin['digest']}"
        evidence.append({**loaded, "reference": pin["reference"], "tarball_sha256": sbom.sha256_file(tarball)})
    return evidence


def _tarball_file_hashes(tarball: Path) -> set[str]:
    """sha256 of every non-empty regular file in every layer of an OCI-layout `docker save`
    tarball (stdlib only; the saved bases carry gzip layers)."""
    hashes: set[str] = set()
    with tarfile.open(tarball) as outer:
        def blob(digest: str) -> "bytes | None":
            try:
                return outer.extractfile(f"blobs/sha256/{digest.split(':', 1)[1]}").read()
            except KeyError:
                return None

        def walk(descriptor: dict) -> None:
            data = blob(descriptor["digest"])
            if data is None:
                return
            doc = json.loads(data)
            for sub in doc.get("manifests", []):
                walk(sub)
            for layer in doc.get("layers", []):
                media_type = layer.get("mediaType", "")
                if "tar" not in media_type and "rootfs.diff" not in media_type:
                    continue   # an attestation (in-toto) descriptor, not a filesystem layer
                assert "zstd" not in media_type, f"zstd layer in {tarball}: this check reads tar and tar+gzip only"
                layer_bytes = blob(layer["digest"])
                if layer_bytes is None:
                    continue
                with tarfile.open(fileobj=io.BytesIO(layer_bytes), mode="r:*") as layer_tar:
                    for member in layer_tar:
                        if member.isreg() and member.size > 0:
                            hashes.add(hashlib.sha256(layer_tar.extractfile(member).read()).hexdigest())

        for descriptor in json.load(outer.extractfile("index.json"))["manifests"]:
            walk(descriptor)
    assert hashes, f"no file read from any layer of {tarball}"
    return hashes


def _assert_kit_carries_no_base_content(manifest_doc: dict) -> dict:
    """The kit holds no `images/*.tar`, and none of its (non-empty) files is byte-identical to a file
    of either base image: every file `KIT_MANIFEST` lists (their hashes were just re-verified by the
    gate) against every file of the two saved bases. A quick per-file check; task 7 of this round
    writes the full inventory test."""
    tars = sorted(p.relative_to(_PROOF_KIT_DIR).as_posix() for p in (_PROOF_KIT_DIR / "images").rglob("*.tar"))
    assert tars == [], f"the kit carries image tarball(s): {tars}"
    base_hashes: dict[str, set[str]] = {}
    for pin_id in save_base_images.base_pins():
        base_hashes[pin_id] = _tarball_file_hashes(_PROOF_BASES_DIR / f"{pin_id}.tar")
    overlap = {
        pin_id: sorted((f["path"], f["role"]) for f in manifest_doc["files"] if f["size"] > 0 and f["sha256"] in hashes)
        for pin_id, hashes in base_hashes.items()
    }
    # One measured, deliberate exception: `images/edge-plugin-image/libssl3.copyright` is the carried
    # `.deb`'s copyright NOTICE (role `image-package-licence`, kept so the recorded package's licence is
    # in the kit) and is byte-identical (sha256 6a7da622...) to Ubuntu 22.04's own
    # /usr/share/doc/libssl3/copyright, because both packages ship the same upstream OpenSSL notice. A
    # licence text is not base-image userland content; any other role is a failure.
    allowed_roles = {"image-package-licence"}
    # A second measured exception (manager, question 250's round): the canonical Apache License 2.0
    # TEXT. `cargo vendor` carries crates whose LICENSE-APACHE is byte-identical to the base images'
    # /usr/share/common-licenses/Apache-2.0 (owned by `base-files`); first seen when question 246's
    # serde-saphyr brought `encoding_rs` and `multiversion_no_op` into the vendor tree. A permissive
    # licence's own text is not base-image userland. Exempt by exact sha256 only; the same bytes do
    # not exempt any other content, and any other overlap fails.
    allowed_licence_texts = {
        "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30": "/usr/share/common-licenses/Apache-2.0",
    }
    sha_by_path = {f["path"]: f["sha256"] for f in manifest_doc["files"]}

    def accepted(path: str, role: str) -> bool:
        return role in allowed_roles or sha_by_path[path] in allowed_licence_texts

    unexpected = {pin_id: [p for p, role in found if not accepted(p, role)] for pin_id, found in overlap.items()}
    assert not any(unexpected.values()), f"the kit carries file(s) byte-identical to a base-image file: {unexpected}"
    return {
        "kit_files_checked": sum(1 for f in manifest_doc["files"] if f["size"] > 0),
        "base_files_hashed": {pin_id: len(h) for pin_id, h in base_hashes.items()},
        "overlap_other_than_licence_notices": 0,
        "licence_notices_identical_to_a_base_file": {pin_id: [p for p, role in found if accepted(p, role)] for pin_id, found in overlap.items()},
    }


class _ImageEvents:
    """`docker events` (image and container events) over a window, to a file under the run scratch.
    The image step is the only thing that runs inside it, so what the daemon logged is what the step
    caused."""

    def __init__(self, path: Path):
        self.path = path
        self._proc = None
        self._fh = None

    def __enter__(self):
        self._fh = self.path.open("w")
        self._proc = subprocess.Popen(
            ["docker", "events", "--format", "{{json .}}", "--filter", "type=image", "--filter", "type=container"],
            stdout=self._fh, stderr=subprocess.STDOUT,
        )
        time.sleep(1.5)
        return self

    def __exit__(self, *exc):
        time.sleep(1.5)
        self._proc.terminate()
        self._proc.wait(timeout=10)
        self._fh.close()
        return False

    def events(self) -> list[dict]:
        return [json.loads(line) for line in self.path.read_text().splitlines() if line.startswith("{")]


def _events_summary(events: list[dict], run_id: str) -> dict:
    counts: dict[str, int] = {}
    for e in events:
        key = f"{e.get('Type')}:{str(e.get('Action')).split(':')[0]}"
        counts[key] = counts.get(key, 0) + 1
    ours = [
        e for e in events
        if e.get("Type") == "container" and (e.get("Actor") or {}).get("Attributes", {}).get("av.test.run_id") == run_id
        and str((e.get("Actor") or {}).get("Attributes", {}).get("image", "")).startswith(_TEST_TAG_PREFIX + "/")
    ]
    return {
        "event_count": len(events), "by_type_and_action": dict(sorted(counts.items())),
        "pulls": [e for e in events if e.get("Type") == "image" and e.get("Action") == "pull"],
        "extract_container_events": sorted({str(e["Action"]).split(":")[0] for e in ours}),
        "extract_containers_started": [e for e in ours if str(e["Action"]).startswith("start")],
    }


def _run_kit_installer(kit: Path, args: list[str], *, timeout: float = 900.0) -> subprocess.CompletedProcess:
    """The KIT's bundled installer (`<kit>/installer/install.py`), isolated mode, on the host."""
    return subprocess.run(
        [sys.executable, "-I", str(kit / _KIT_INSTALLER_PY), *args], capture_output=True, text=True, timeout=timeout,
    )


#: The in-image shell implementations of the runtime-content set -- the commands
#: `services/cfs/build-image.sh` (2b) and `services/edge-plugin/build-image.sh` (3a) run inside the
#: image. This file only formats and sorts their output (its own code, not the installer's).
_IN_IMAGE_HASH_SHELL = {
    "cfs-image": (
        "find /cfs/cpu1 -type f -exec sha256sum {} + ; "
        "sha256sum /cfs/av-lockstep-shim /cfs/av-lockstep-relay /cfs/container-entrypoint.sh"
    ),
    "edge-plugin-image": (
        "find /opt/edge-plugin -type f -exec sha256sum {} +\n"
        "sha256sum /usr/local/bin/av-edge-plugin\n"
        "dpkg -L libssl3 | while IFS= read -r p; do\n"
        "    if [ -f \"$p\" ] && [ ! -L \"$p\" ]; then sha256sum \"$p\"; fi\n"
        "done"
    ),
}


def _independent_runtime_hash(guard: ResourceGuard, image_ref: str, component: str) -> tuple[str, int]:
    """A second implementation of question 185's hash: `sha256sum` inside the image (no `docker
    cp`, no tar parsing), then this file's own sort and format. Run only after the installer's own
    check passed."""
    out = _docker(
        "run", "--rm", "--network", "none", *guard.label_args(), "--entrypoint", "sh", image_ref, "-c",
        _IN_IMAGE_HASH_SHELL[component],
    ).stdout
    pairs = {}
    for line in out.splitlines():
        if line.strip():
            digest, _, path = line.partition("  ")
            pairs[path] = digest
    assert pairs, f"{component}: the in-image sha256sum printed nothing"
    text = "".join(f"{line}\n" for line in sorted(f"{path} {digest}" for path, digest in pairs.items()))
    return "sha256:" + hashlib.sha256(text.encode("utf-8")).hexdigest(), len(pairs)


_CONFIG_KEYS = ("Entrypoint", "Cmd", "User", "WorkingDir", "Volumes", "Labels", "Env")


def _config_view(image_ref: str) -> dict:
    config = json.loads(_docker("image", "inspect", image_ref, "--format", "{{json .Config}}").stdout)
    view = {k: config.get(k) or None for k in _CONFIG_KEYS}
    if view["Labels"]:
        view["Labels"] = {k: v for k, v in view["Labels"].items() if not k.startswith("org.altavista.kit-")} or None
    return view


def _run_image_step_and_verify(guard: ResourceGuard, image_tags: ImageTagGuard, run_id: str, run_scratch: Path, manifest_doc: dict) -> tuple[dict, dict]:
    """B.2b (question 250(a)): the host-side image step, from the kit's own installer, with the
    proofs listed in this module's doc, item 4b. Returns `(image tag by component, evidence)`."""
    tags = {component: _test_image_tag(run_id, component) for component in sbom.IMAGE_COMPONENTS}
    table_before = docker_image_table()
    record_path = run_scratch / "IMAGE_BUILD_RECORD"
    build_args = ["--build-images", str(_PROOF_KIT_DIR), "--record", str(record_path)]
    for component, tag in tags.items():
        build_args += ["--image-tag", f"{component}={tag}"]
    build_args += ["--extract-container-label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--extract-container-label", f"av.test.run_id={run_id}"]

    with _ImageEvents(run_scratch / "docker-events.jsonl") as events:
        probe = _run_kit_installer(_PROOF_KIT_DIR, ["--probe-offline-build", str(_PROOF_KIT_DIR)])
        assert probe.returncode == 0, f"the offline probe build failed:\n{probe.stdout}\n{probe.stderr}"
        build = _run_kit_installer(_PROOF_KIT_DIR, build_args)
    assert build.returncode == 0, f"the kit's image step failed (rc={build.returncode}):\n--- stdout ---\n{build.stdout}\n--- stderr ---\n{build.stderr}"
    summary = _events_summary(events.events(), run_id)
    assert summary["pulls"] == [], f"docker logged a pull while the images were built: {summary['pulls']}"
    assert summary["extract_containers_started"] == [], f"a container the step created was started: {summary['extract_containers_started']}"
    assert "create" in summary["extract_container_events"], f"the step's extract containers left no docker event -- the window is not observing: {summary}"
    print(f"\n--- offline probe builds, from the kit's installer (question 154) ---\n{probe.stdout}")
    print(f"\n--- image step, from the kit's own installer, stdout ---\n{build.stdout}")
    print(f"\n--- docker events over the probe and the builds ---\n{json.dumps({k: v for k, v in summary.items() if k != 'pulls'}, indent=2)}")

    record = json.loads(record_path.read_text(encoding="utf-8"))
    assert record["kit_manifest_sha256"] == sbom.sha256_file(_PROOF_KIT_DIR / "KIT_MANIFEST")
    evidence: dict = {"probe": probe.stdout.strip().splitlines(), "events": {k: v for k, v in summary.items() if k != "pulls"}, "images": {}}
    for component in sbom.IMAGE_COMPONENTS:
        r = record["images"][component]
        entry = manifest_doc["images"][component]
        assert r["tag"] == tags[component]
        # Each built id did not exist before this run: removing the tag later removes only what it made.
        assert r["image_id"] not in table_before and r["image_id_existed_before"] is False, f"{component}: {r['image_id']} existed before the build"
        guard.track_image(tags[component])
        image_tags.test_only_tags.append(tags[component])
        assert r["equal"] is True and r["runtime_content_hash_computed"] == r["runtime_content_hash_recorded"] == entry["runtime_content_hash"]
        assert r["modes_equal"] and r["config_equal"]
        assert r["base"]["digest"] == entry["base"]["digest"] and r["base"]["verified_by"] in ("Id", "RepoDigests")
        assert r["build"]["flags"][:3] == ["--network", "none", "--pull=false"]
        assert _docker("image", "inspect", tags[component], "--format", "{{.Id}}").stdout.strip() == r["image_id"]
        # The recorded hash, recomputed by a DIFFERENT implementation (sha256sum inside the image).
        independent, n_files = _independent_runtime_hash(guard, tags[component], component)
        assert independent == r["runtime_content_hash_recorded"], (
            f"{component}: sha256sum inside the built image gives {independent}, the kit records {r['runtime_content_hash_recorded']}"
        )
        # `.Config` against the RECORDED `:local` image's (the gate checked it is at its recorded id).
        built_view, recorded_view = _config_view(tags[component]), _config_view(entry["tag"])
        assert built_view == recorded_view, f"{component}: .Config differs from {entry['tag']}'s: built {built_view} vs recorded {recorded_view}"
        evidence["images"][component] = {
            "tag": tags[component], "built_image_id": r["image_id"], "recorded_image_id": entry["recorded_digest"],
            "runtime_content_hash_recorded": r["runtime_content_hash_recorded"], "installer_computed": r["runtime_content_hash_computed"],
            "independent_in_image_sha256sum": independent, "files": n_files, "config_compared_with": entry["tag"], "config": built_view,
        }
    print(f"\n--- built images, checked (question 185/250(a)) ---\n{json.dumps(evidence['images'], indent=2)}")
    return tags, evidence


def _assert_no_docker_daemon_access(run_args: list[str]) -> None:
    """The lead's ruling (question 250(a)): the enclave container never holds the Docker socket,
    `DOCKER_HOST`, or any other path to the daemon -- a container that controls the daemon can start
    a networked container and defeat the `--internal` isolation the proof rests on. `run_args` is the
    exact `docker run` argument list for the installing container, built in one place."""
    daemon_words = ("docker.sock", "docker_engine", "podman.sock", "containerd.sock", "DOCKER_HOST", "unix://", "tcp://")
    forbidden_flags = {"--privileged", "--pid", "--ipc", "--userns", "--device", "--cap-add", "--security-opt", "--volumes-from"}
    for i, arg in enumerate(run_args):
        assert not any(w in arg for w in daemon_words), f"the installing container's docker run arguments name a path to the daemon: {arg!r}"
        assert arg.split("=")[0] not in forbidden_flags, f"the installing container's docker run arguments include {arg!r}"
        if arg in ("-v", "--volume", "--mount"):
            spec = run_args[i + 1]
            source = spec.split(":")[0] if arg != "--mount" else next((kv.split("=", 1)[1] for kv in spec.split(",") if kv.startswith(("source=", "src="))), "")
            if source.startswith("/"):
                resolved = Path(source)
                assert not (resolved.exists() and stat.S_ISSOCK(resolved.stat().st_mode)), f"{source} is a socket"
                assert not source.endswith(".sock") and not source.startswith(("/var/run", "/run")), f"{source} looks like a runtime socket path"
        if arg in ("-e", "--env", "--env-file"):
            value = run_args[i + 1]
            if arg == "--env-file":
                value += Path(value).read_text() if Path(value).is_file() else ""
            assert "DOCKER" not in value.upper(), f"the installing container is given {value!r}"


_NO_DAEMON_PROBE_SCRIPT = (
    "import glob, json, os, socket\n"
    "found = [p for p in glob.glob('/var/run/*') + glob.glob('/run/*') + ['/var/run/docker.sock', '/run/docker.sock'] if os.path.exists(p)]\n"
    "socks = [p for p in found if p.endswith('.sock')]\n"
    "s = socket.socket(socket.AF_UNIX)\n"
    "try:\n"
    "    s.connect('/var/run/docker.sock'); connect = 'CONNECTED (unexpected)'\n"
    "except OSError as e:\n"
    "    connect = type(e).__name__\n"
    "print(json.dumps({'sockets': socks, 'docker_env': sorted(k for k in os.environ if 'DOCKER' in k.upper()), 'connect_to_docker_sock': connect}))\n"
)


def _prove_no_daemon_inside(guard: ResourceGuard, run_id: str, network_name: str, install_volume: str, installer_container: str, mount_args: list[str]) -> dict:
    """The daemon's own record of the installing container, and a probe container with the same
    mounts that looks for a socket and `DOCKER_HOST` from the inside."""
    inspected = json.loads(_docker("inspect", installer_container).stdout)[0]
    host_config, config = inspected["HostConfig"], inspected["Config"]
    binds = host_config.get("Binds") or []
    assert not any("docker" in b.lower() and "sock" in b.lower() for b in binds) and not any(b.split(":")[0].endswith(".sock") for b in binds), binds
    assert not [e for e in (config.get("Env") or []) if "DOCKER" in e.upper()], config.get("Env")
    assert host_config.get("Privileged") is False and not host_config.get("PidMode") and host_config.get("NetworkMode") == network_name, host_config
    probe = _docker(
        "run", "--rm", "--network", network_name, *guard.label_args(), *mount_args,
        PROBE_IMAGE, "python3", "-c", _NO_DAEMON_PROBE_SCRIPT,
    )
    inside = json.loads(probe.stdout.strip().splitlines()[-1])
    assert inside["sockets"] == [] and inside["docker_env"] == [] and inside["connect_to_docker_sock"] != "CONNECTED (unexpected)", inside
    return {"binds": binds, "privileged": host_config.get("Privileged"), "network_mode": host_config.get("NetworkMode"), "inside": inside}


def _run_zero_egress_install_and_demo_from_the_kit_alone():
    run_id = uuid.uuid4().hex[:12]
    run_scratch = SCRATCH_ROOT / run_id
    run_scratch.mkdir(parents=True, exist_ok=True)
    guard = ResourceGuard(run_id)

    prune_stale_labelled_resources()

    manifest_doc = json.loads((_PROOF_KIT_DIR / "KIT_MANIFEST").read_text(encoding="utf-8"))
    kit_manifest_sha256 = sbom.sha256_file(_PROOF_KIT_DIR / "KIT_MANIFEST")

    reached: dict[str, bool] = {"4a_viewer": False, "4b_ingest": False, "4c_plugin": False, "4d_runs": False}

    # Round 5 (question 234): `ImageTagGuard` takes the tag snapshot before this run's first
    # `docker load`, anywhere -- its `assert_host_tags_unchanged` (the outer `finally`, below)
    # compares against it. Constructed here, outside the `try`, so it is never undefined even if
    # `_load_and_verify_images` itself raises partway through its own loop. Question 239: the
    # implementation moved to `scripts/kit/docker_image_tags.py`, shared with `live_evidence.py`.
    image_tags = ImageTagGuard(run_id, _TEST_TAG_PREFIX, on_test_tag=guard.track_image)

    try:
        # ---------------------------------------------------------------------------------
        # B.1 -- base-image provenance, on the host, before anything else.
        # ---------------------------------------------------------------------------------
        base_evidence = _load_bases(image_tags)
        print(f"\n--- base-image provenance, from the saved tarballs (question 250(a)) ---\n{json.dumps(base_evidence, indent=2)}")
        no_base_content = _assert_kit_carries_no_base_content(manifest_doc)
        print(f"\n--- the kit carries only application layers (question 250(a)) ---\n{json.dumps(no_base_content)}")

        # ---------------------------------------------------------------------------------
        # The labelled --internal network every later step shares.
        # ---------------------------------------------------------------------------------
        network_name = _labelled_id(run_id, "net")
        _docker("network", "create", "--internal", *guard.label_args(), network_name)
        guard.track_network(network_name)

        # ---------------------------------------------------------------------------------
        # B.3 (before) -- prove the network has no route out BEFORE any install activity.
        # ---------------------------------------------------------------------------------
        probe_before = _run_egress_probe(guard, run_id, "before-install", network_name)
        print(f"\n--- egress probe, before install (question 154) ---\n{json.dumps(probe_before)}")

        # ---------------------------------------------------------------------------------
        # B.2 -- install inside the enclave: the kit bind-mounted read-only, install.sh run
        # inside a container on the --internal network, into a fresh labelled volume.
        # ---------------------------------------------------------------------------------
        install_volume = _labelled_id(run_id, "install-vol")
        _docker("volume", "create", *guard.label_args(), install_volume)
        guard.track_volume(install_volume)

        # Question 217(c): the kit now carries its own installer (`build_kit.py::
        # assemble_installer`) -- built in ONE place so `_assert_no_repo_source_mounted` checks
        # the real argument list, never a description of it. `scripts/kit/` (`KIT_DIR`) is no
        # longer mounted at all; the container gets only the kit itself (read-only) and its own
        # fresh install target volume.
        install_mount_args = [
            "-v", f"{_PROOF_KIT_DIR}:/kit:ro",
            "-v", f"{install_volume}:/target",
        ]
        installer_container = _labelled_id(run_id, "installer")
        guard.track_container(installer_container)
        install_run_args = [
            "run", "--name", installer_container, "--network", network_name, *guard.label_args(),
            *install_mount_args,
            PROBE_IMAGE, "/kit/installer/install.sh", "/kit", "/target",
        ]
        _assert_no_repo_source_mounted(install_run_args)
        # Question 250(a), the lead's ruling: the enclave container gets no Docker socket, no
        # DOCKER_HOST, no path to the daemon at all -- asserted over the real argument list.
        _assert_no_docker_daemon_access(install_run_args)
        install_start = time.monotonic()
        install_run = _docker(*install_run_args, timeout=180.0)
        install_elapsed_s = time.monotonic() - install_start
        no_daemon_evidence = _prove_no_daemon_inside(guard, run_id, network_name, install_volume, installer_container, install_mount_args)
        print(f"\n--- no path to the Docker daemon in the enclave (question 250(a)) ---\n{json.dumps(no_daemon_evidence, indent=2)}")
        assert "installed kit" in install_run.stdout, f"install.sh did not report success:\n{install_run.stdout}"
        assert "--no-index" not in install_run.stderr, "unexpected: --no-index itself should never appear as an error"
        print(f"\n--- install.sh, inside the enclave (question 148) ---\nelapsed={install_elapsed_s:.2f}s\n{install_run.stdout}")
        subprocess.run(["docker", "rm", "-f", installer_container], capture_output=True, timeout=30)

        # Cross-check the sha256 install.sh itself printed against this test's own, independent
        # computation on the host -- two independent ways of asking the same question.
        assert kit_manifest_sha256 in install_run.stdout, (
            f"install.sh's own printed manifest sha256 does not match this test's own "
            f"independent computation ({kit_manifest_sha256}):\n{install_run.stdout}"
        )

        install_record_raw = _docker(
            "run", "--rm", "--network", network_name, *guard.label_args(),
            "-v", f"{install_volume}:/target:ro",
            PROBE_IMAGE, "cat", "/target/INSTALL_RECORD",
        )
        install_record = json.loads(install_record_raw.stdout)
        assert install_record["kit_manifest_sha256"] == kit_manifest_sha256
        assert install_record["installed"]["wheels"]["installed_packages"], "INSTALL_RECORD reports no installed Python packages"
        installed_names = {p["name"].lower().replace("_", "-") for p in install_record["installed"]["wheels"]["installed_packages"]}
        assert "altavista" in installed_names, f"altavista itself was not installed: {installed_names}"
        # Round 3 (question 217(b)): INSTALL_RECORD no longer names a separate "viewer_assets"
        # path -- task 3c's `web`/`profiles` packs are gone, and the installed wheel already
        # carries both (`scripts/kit/install.py::install`'s own comment on why that key was
        # removed). `_run_viewer_from_installed_kit`, below, is this test's own proof that the
        # viewer actually finds them with no such path at all.
        images_record = install_record["installed"]["images"]
        assert images_record["built"] is False and images_record["built_by"] is None, images_record
        assert "--build-images" in images_record["build_step"]
        for component in sbom.IMAGE_COMPONENTS:
            assert images_record["components"][component]["expected_runtime_content_hash"] == manifest_doc["images"][component]["runtime_content_hash"]
        assert "viewer_assets" not in install_record["installed"], (
            f"INSTALL_RECORD still names a viewer_assets path -- task 3c's web/profiles packs "
            f"were supposed to be fully removed (round 3, question 217(b)): "
            f"{install_record['installed'].get('viewer_assets')!r}"
        )
        print(f"\n--- INSTALL_RECORD (question 148) ---\n{json.dumps(install_record, indent=2)[:2000]}")

        # ---------------------------------------------------------------------------------
        # B.3 (after) -- prove the network STILL has no route out, after the install ran.
        # ---------------------------------------------------------------------------------
        probe_after = _run_egress_probe(guard, run_id, "after-install", network_name)
        print(f"\n--- egress probe, after install (question 154) ---\n{json.dumps(probe_after)}")

        # ---------------------------------------------------------------------------------
        # B.2b -- the images, built on the HOST by the kit's own installer (question 250(a)):
        # offline, on the bases loaded above, checked against the recorded runtime-content hash.
        # ---------------------------------------------------------------------------------
        image_tag_by_component, _image_step_evidence = _run_image_step_and_verify(guard, image_tags, run_id, run_scratch, manifest_doc)
        edge_plugin_image_ref = image_tag_by_component["edge-plugin-image"]

        # ---------------------------------------------------------------------------------
        # B.4a -- the viewer server, started from the INSTALLED wheels.
        # ---------------------------------------------------------------------------------
        viewer_evidence = _run_viewer_from_installed_kit(guard, run_id, network_name, install_volume, run_scratch)
        reached["4a_viewer"] = True
        print(f"\n--- viewer server, from the installed wheels (question 148) ---\n{json.dumps(viewer_evidence)}")

        # ---------------------------------------------------------------------------------
        # B.4b -- av-ingest-server, started from the kit's own cross-built binary.
        # ---------------------------------------------------------------------------------
        ingest_container, ingest_evidence = _run_ingest_from_installed_kit(guard, run_id, network_name, install_volume, edge_plugin_image_ref)
        reached["4b_ingest"] = True
        print(f"\n--- av-ingest-server, from the kit's own binary (question 148) ---\n{json.dumps(ingest_evidence)}")

        # ---------------------------------------------------------------------------------
        # B.4c -- the edge plugin, from the image the kit's own installer built in B.2b.
        # ---------------------------------------------------------------------------------
        plugin_evidence = _run_edge_plugin_from_kit_image(guard, run_id, ingest_container, run_scratch, edge_plugin_image_ref)
        reached["4c_plugin"] = True
        print(f"\n--- edge plugin, from the image the kit's installer built (question 148) ---\n{json.dumps(plugin_evidence)}")

        # ---------------------------------------------------------------------------------
        # B.4d -- the recorded kernel runs, re-hashed and re-decoded from the INSTALLED tree.
        # ---------------------------------------------------------------------------------
        run_evidence = _rehash_and_decode_runs(guard, run_id, network_name, install_volume, manifest_doc)
        reached["4d_runs"] = True
        print(f"\n--- recorded kernel runs, re-hashed from the installed tree (question 148) ---\n{json.dumps(run_evidence, indent=2)}")

        # ---------------------------------------------------------------------------------
        # B.4e -- av-command: reads the kit's own cross-build result, whichever it is. Round 3's
        # question 217(a) fix (build_kit.py's PREBUILD_BASE_IMAGE moved rust:1.85-bookworm ->
        # rust:1.90-bookworm) is expected to close the cross-build gap this section used to
        # hard-assert -- if it does, this proves the binary actually RUNS (a real risk specific
        # to cross-compiling: a corrupt or wrongly-linked binary would fail here, not merely be
        # absent) by invoking it with NO arguments and checking for its own well-defined refusal
        # (crates/av-command/src/bin/av-command.rs::parse_args: "--oidc-issuer, --oidc-audience
        # and --oidc-public-key-path are all required" -- A2.1, no default issuer). This does NOT
        # start the full gRPC/admin service end to end -- av-command needs a real OIDC issuer and
        # public key this test does not have, and this task's own rules forbid editing
        # crates/av-command to add one; "the binary this kit cross-built genuinely executes and
        # reaches its own real argument-parsing logic" is the proportionate proof here, not "the
        # service is live". If the gap is still present for some OTHER reason, it is read and
        # reported, never silently ignored either way (question 148: an exit code/gap name alone
        # is not evidence -- the real reason is always quoted).
        # ---------------------------------------------------------------------------------
        av_command_gap = next((g for g in manifest_doc["gaps"] if g["name"] == "av-command-binary"), None)
        if av_command_gap is not None:
            print(f"\n--- av-command (named gap, not attempted -- question 148) ---\n{av_command_gap['reason'][-400:]}")
        else:
            av_command_result = manifest_doc["binaries"]["results"].get("av-command", {})
            assert av_command_result.get("included") is True, (
                f"av-command is not in KIT_MANIFEST's gaps list, but binaries.results also does "
                f"not show it included -- inconsistent kit: {av_command_result!r}"
            )
            av_command_run = _docker(
                "run", "--rm", "--network", network_name, *guard.label_args(),
                "-v", f"{install_volume}:/target:ro",
                PROBE_IMAGE, "/target/kit/binaries/av-command",
                check=False,
            )
            combined = av_command_run.stdout + av_command_run.stderr
            assert av_command_run.returncode != 0, (
                f"av-command with no arguments should refuse (no default OIDC config) -- "
                f"unexpectedly exited 0, which would mean it silently started a real service "
                f"with no auth configured:\n{combined}"
            )
            assert "oidc" in combined.lower(), (
                f"av-command exited (rc={av_command_run.returncode}) but not with the expected "
                f"missing-OIDC-config refusal -- possible cross-build/link problem, not simply a "
                f"missing-config one:\n{combined}"
            )
            print(f"\n--- av-command, cross-built by this kit and genuinely RUNS (question 148) ---\n"
                  f"rc={av_command_run.returncode}\n{combined[-800:]}")

        print(f"\n--- B.4 reached: {reached} ---")

    finally:
        guard.cleanup()
        shutil.rmtree(run_scratch, ignore_errors=True)
        # Round 5 (question 234): runs even on the failure path above -- if the `try` body already
        # raised, a violation found here chains onto it (both are visible in the traceback) rather
        # than being silently skipped, which is exactly the gap the lead's own incident exposed (a
        # test that moved tags and then failed for an unrelated reason).
        image_tags.assert_host_tags_unchanged()

    guard.assert_nothing_left()

    assert all(reached.values()), f"did not reach every step of the demo path: {reached}"


def _viewer_bootstrap_script() -> str:
    """Round 3 (question 217(b)) -- see this module's own top doc, "The viewer's own static
    assets now come from the WHEEL alone". Every line of code here is the installed wheel's own
    (`altavista.server.create_app`, `uvicorn`); `create_app()` is called with NEITHER `web_dir=`
    nor any profile override at all -- no bind-mount, no kit pack path, no monkeypatch of
    `altavista.profile.PROFILES_DIR` (task 3c's own last resort, gone along with the packaging
    gap it worked around). Both resolve entirely on their own, against the packaged copies the
    round-3 `pyproject.toml`/`setup.py` change put inside this very venv's own installed
    `altavista` package (`site-packages/altavista/{web,profiles}/`)."""
    return (
        "import uvicorn\n"
        "from altavista.server import create_app\n"
        "app = create_app(texture_dir='/nonexistent-textures')\n"
        "print('APP_CREATED_OK', flush=True)\n"
        "uvicorn.run(app, host='0.0.0.0', port=8765, log_level='info')\n"
    )


#: A minimal, stdlib-only probe body run INSIDE the installed venv (`/target/venv/bin/python3`,
#: never the bare `python:3.13-slim` interpreter -- this needs the installed `altavista` package
#: itself) that loads a real profile through `altavista.profile`'s own loader, proving the
#: packaged `profiles/` copy is genuinely reachable and readable from the installed wheel alone,
#: not merely present on disk somewhere this test never actually reads.
_PROFILE_LOAD_PROBE_SCRIPT = (
    "import json\n"
    "import altavista.profile as profile_mod\n"
    "resolved = profile_mod.resolve_profiles_dir()\n"
    "imagery = profile_mod.load_imagery_config('design')\n"
    "print(json.dumps({'resolved_profiles_dir': str(resolved), 'imagery': imagery}))\n"
)


def _run_viewer_from_installed_kit(guard: ResourceGuard, run_id: str, network_name: str, install_volume: str, run_scratch: Path) -> dict:
    bootstrap_path = run_scratch / "viewer_bootstrap.py"
    bootstrap_path.write_text(_viewer_bootstrap_script(), encoding="utf-8")

    viewer_container = _labelled_id(run_id, "viewer")
    guard.track_container(viewer_container)
    _docker(
        # Round 3 (question 217(b)): `install_volume` is STILL the only content mount here, but
        # for a different reason than task 3c's -- it carries the installed venv (`/target/
        # venv/`), whose own `site-packages/altavista/{web,profiles}/` the wheel itself put there.
        # No kit pack path is read or referenced at all any more -- there is no pack left to
        # reference (`manifest.PACKS` no longer has `web`/`profiles` entries).
        "run", "-d", "--name", viewer_container, "--network", network_name, *guard.label_args(),
        "-v", f"{install_volume}:/target:ro",
        "-v", f"{bootstrap_path}:/bootstrap.py:ro",
        "--entrypoint", "/target/venv/bin/python",
        PROBE_IMAGE, "/bootstrap.py",
    )
    log_lines = _wait_for_container_log_substring(viewer_container, ("APP_CREATED_OK", "Uvicorn running"), timeout_s=20.0)

    probe_container = _labelled_id(run_id, "viewer-http-probe")
    guard.track_container(probe_container)
    http = _docker(
        "run", "--rm", "--name", probe_container, "--network", f"container:{viewer_container}", *guard.label_args(),
        PROBE_IMAGE, "python3", "-c",
        "import urllib.request, json\n"
        "results = {}\n"
        "r = urllib.request.urlopen('http://127.0.0.1:8765/api/scenarios', timeout=5)\n"
        "results['scenarios'] = {'status': r.status, 'body': json.loads(r.read())}\n"
        # Round 3 (question 217(b)): the strengthened proof -- GET /api/scenarios returning 200
        # only proves an ASGI app started at all (its body comes from an in-memory Hub, never
        # from disk); a real GET for the viewer's own index page and a real GET for one of the
        # two symlinks-turned-real-files inside web/node_modules/three/ (setup.py's own doc on
        # what happened to them) is what actually proves web/ came out of the WHEEL.
        "r = urllib.request.urlopen('http://127.0.0.1:8765/index.html', timeout=5)\n"
        "results['index_html'] = {'status': r.status, 'body_len': len(r.read())}\n"
        "r = urllib.request.urlopen('http://127.0.0.1:8765/node_modules/three/three.module.js', timeout=5)\n"
        "results['three_module_js'] = {'status': r.status, 'body_len': len(r.read())}\n"
        "print(json.dumps(results))\n",
    )
    result = json.loads(http.stdout.strip().splitlines()[-1])
    assert result["scenarios"]["status"] == 200, f"GET /api/scenarios did not return 200: {result}"
    assert result["scenarios"]["body"] == {"names": []}, f"unexpected body from a freshly started viewer: {result}"
    assert result["index_html"]["status"] == 200, f"GET /index.html did not return 200 -- web/ did not come out of the wheel: {result}"
    assert result["index_html"]["body_len"] > 200, f"GET /index.html returned a suspiciously small body: {result}"
    assert result["three_module_js"]["status"] == 200, (
        f"GET /node_modules/three/three.module.js did not return 200 -- the dereferenced symlink "
        f"(setup.py's own build_py copy) did not make it into the wheel: {result}"
    )
    assert result["three_module_js"]["body_len"] > 100_000, (
        f"GET /node_modules/three/three.module.js returned a suspiciously small body (three.js "
        f"itself is hundreds of KB): {result}"
    )

    # Round 3 (question 217(b)): a real profile, loaded through altavista.profile's own loader,
    # from inside the installed venv -- proves the packaged profiles/ copy is genuinely reachable
    # from the wheel alone, not merely that SOME static root answered HTTP above.
    profile_probe_container = _labelled_id(run_id, "profile-load-probe")
    guard.track_container(profile_probe_container)
    profile_probe = _docker(
        "run", "--rm", "--name", profile_probe_container, "--network", network_name, *guard.label_args(),
        "-v", f"{install_volume}:/target:ro",
        "--entrypoint", "/target/venv/bin/python3",
        PROBE_IMAGE, "-c", _PROFILE_LOAD_PROBE_SCRIPT,
    )
    profile_result = json.loads(profile_probe.stdout.strip().splitlines()[-1])
    assert profile_result["resolved_profiles_dir"].endswith("altavista/profiles"), (
        f"altavista.profile.resolve_profiles_dir() did not resolve to the packaged copy inside "
        f"the installed wheel: {profile_result}"
    )
    assert "urlTemplate" in profile_result["imagery"], f"design profile's imagery section did not load: {profile_result}"

    return {"http": result, "profile_load": profile_result, "startup_log_tail": log_lines[-5:]}


def _run_ingest_from_installed_kit(guard: ResourceGuard, run_id: str, network_name: str, install_volume: str, edge_plugin_image: str) -> tuple[str, dict]:
    ingest_container = _labelled_id(run_id, "ingest")
    guard.track_container(ingest_container)
    _docker(
        "run", "-d", "--name", ingest_container, "--network", network_name, *guard.label_args(),
        "--user", "0:0",  # see tests/test_edge_plugin_container.py's own identical comment: the
                            # runtime base image bakes a non-root USER, under which --log-dir
                            # cannot be created on the image's own root filesystem.
        "-v", f"{install_volume}:/target:ro",
        "-v", f"{VERIFY_PUB_PEM}:/keys/verify.pub.pem:ro",
        "--entrypoint", "/target/kit/binaries/av-ingest-server",
        # Round 5 (question 234): this test's own test-only tag (`ImageTagGuard.test_only_tag`), not
        # `av-edge-plugin:local` -- that host tag is exactly what `_load_and_verify_images` no
        # longer trusts (it could be a different image by the time this container starts, on a
        # host running other tracks concurrently). Still the SAME image this test loaded from the
        # kit's own tarball in B.1, id-verified there before this test-only tag was ever created.
        edge_plugin_image,
        "--grpc-bind", "127.0.0.1:50070", "--admin-bind", "127.0.0.1:50071",
        "--log-dir", "/data/ingest-log",
        "--no-require-client-cert", "--verify-key", f"{PRODUCER_ID}:/keys/verify.pub.pem",
        "--clearance-ladder", CLEARANCE_LADDER, "--max-batch-age-ns", str(MAX_BATCH_AGE_NS),
        "--clock-tai-ns", str(CLOCK_TAI_NS),
    )
    log_lines = _wait_for_container_log_substring(ingest_container, ("GRPC_LISTENING", "ADMIN_LISTENING"), timeout_s=15.0)
    return ingest_container, {"readiness_log_tail": log_lines[-5:]}


def _run_edge_plugin_from_kit_image(guard: ResourceGuard, run_id: str, ingest_container: str, run_scratch: Path, edge_plugin_image: str) -> dict:
    plugin_container = _labelled_id(run_id, "plugin")
    guard.track_container(plugin_container)
    plugin_run = subprocess.run(
        [
            # Round 5 (question 234): `edge_plugin_image` is this test's own test-only tag -- see
            # `_run_ingest_from_installed_kit`'s own identical comment on why not `av-edge-plugin:local`.
            "docker", "run", "--name", plugin_container, "--network", f"container:{ingest_container}", *guard.label_args(),
            "-v", f"{SIGNING_KEY_PEM}:/keys/signing.pem:ro",
            edge_plugin_image,
            "--signing-key", "/keys/signing.pem",
            "--endpoint", "127.0.0.1:50070",
        ],
        capture_output=True, text=True, timeout=120,
    )
    assert plugin_run.returncode == 0, f"av-edge-plugin (from the kit's own loaded image) failed delivering to the kit's own ingest:\n--- stdout ---\n{plugin_run.stdout}\n--- stderr ---\n{plugin_run.stderr}"
    summary = json.loads(plugin_run.stdout.strip().splitlines()[-1])
    assert summary["any_rejected"] is False, summary
    assert summary["batch_count"] == EXPECTED_BATCH_COUNT, summary
    assert summary["measurement_count"] == EXPECTED_BATCH_COUNT, summary
    assert summary["chain_head_hex"] == EXPECTED_CHAIN_HEAD_HEX, summary
    subprocess.run(["docker", "rm", "-f", plugin_container], capture_output=True, timeout=30)

    evidence_probe_container = _labelled_id(run_id, "evidence-probe")
    guard.track_container(evidence_probe_container)
    evidence_raw = _docker(
        "run", "--rm", "--name", evidence_probe_container, "--network", f"container:{ingest_container}", *guard.label_args(),
        PROBE_IMAGE, "python3", "-c", _EVIDENCE_FETCH_SCRIPT,
    )
    evidence = json.loads(evidence_raw.stdout.strip().splitlines()[-1])
    assert evidence["accepted_total"] == EXPECTED_BATCH_COUNT, evidence
    assert evidence["rejected_total"] == 0, evidence
    producer = evidence["producers"][PRODUCER_ID]
    assert producer["chain_head"] == EXPECTED_CHAIN_HEAD_HEX, producer
    partition = evidence["partitions"][SHARD_KEY]
    assert partition["record_count"] == EXPECTED_BATCH_COUNT, partition

    return {
        "plugin_summary": {k: summary[k] for k in ("batch_count", "measurement_count", "any_rejected", "chain_head_hex")},
        "evidence_accepted_total": evidence["accepted_total"],
        "evidence_rejected_total": evidence["rejected_total"],
    }


#: A minimal, stdlib-only reimplementation of `build_kit.py`'s own `_read_varint`/`_last_length_
#: delimited_field`/`read_run_provenance` protobuf wire-walk (same algorithm, restated here) --
#: deliberately NOT `import build_kit` inside this probe container: that module imports
#: `packaging` at its own top level (needed only by its unrelated `viewer_runtime_closure`
#: function, for the ALSO-gated `--with-wheels` step), which a bare `python:3.13-slim` container
#: does not have installed and this test must not `pip install` into (question 154: no network at
#: test time, not even for the test's OWN tooling). This walk needs nothing beyond the stdlib.
_run_rehash_script = (
    "import json, pathlib, hashlib\n"
    "def sha256_file(p):\n"
    "    h = hashlib.sha256()\n"
    "    with open(p, 'rb') as f:\n"
    "        for chunk in iter(lambda: f.read(1 << 20), b''):\n"
    "            h.update(chunk)\n"
    "    return h.hexdigest()\n"
    "def read_varint(buf, pos):\n"
    "    result = 0; shift = 0\n"
    "    while True:\n"
    "        b = buf[pos]; pos += 1\n"
    "        result |= (b & 0x7F) << shift\n"
    "        if not (b & 0x80):\n"
    "            return result, pos\n"
    "        shift += 7\n"
    "def last_length_delimited(buf, field_num):\n"
    "    pos = 0; n = len(buf); result = None\n"
    "    while pos < n:\n"
    "        tag, pos = read_varint(buf, pos)\n"
    "        fn = tag >> 3; wt = tag & 0x7\n"
    "        if wt == 0:\n"
    "            _, pos = read_varint(buf, pos)\n"
    "        elif wt == 1:\n"
    "            pos += 8\n"
    "        elif wt == 2:\n"
    "            length, pos = read_varint(buf, pos)\n"
    "            value = buf[pos:pos + length]; pos += length\n"
    "            if fn == field_num:\n"
    "                result = value\n"
    "        elif wt == 5:\n"
    "            pos += 4\n"
    "        else:\n"
    "            raise ValueError(f'unsupported wire type {wt}')\n"
    "    return result\n"
    "runs_dir = pathlib.Path('/target/kit/runs')\n"
    "result = {}\n"
    "for f in sorted(runs_dir.glob('*.runproducts.bin')):\n"
    "    stem = f.name[: -len('.runproducts.bin')]\n"
    "    buf = f.read_bytes()\n"
    "    provenance_bytes = last_length_delimited(buf, 5)\n"
    "    config_hash = None\n"
    "    if provenance_bytes is not None:\n"
    "        ch = last_length_delimited(provenance_bytes, 4)\n"
    "        if ch is not None:\n"
    "            config_hash = ch.decode('utf-8')\n"
    "    result[stem] = {'sha256': sha256_file(f), 'config_hash': config_hash}\n"
    "print(json.dumps(result))\n"
)


def _rehash_and_decode_runs(guard: ResourceGuard, run_id: str, network_name: str, install_volume: str, manifest_doc: dict) -> dict:
    rehash_container = _labelled_id(run_id, "runs-rehash")
    guard.track_container(rehash_container)
    result = _docker(
        "run", "--rm", "--name", rehash_container, "--network", network_name, *guard.label_args(),
        "-v", f"{install_volume}:/target:ro",
        PROBE_IMAGE, "python3", "-c", _run_rehash_script,
    )
    observed = json.loads(result.stdout.strip().splitlines()[-1])

    recorded_hashes = {f["path"]: f["sha256"] for f in manifest_doc["files"] if f["path"].startswith("runs/")}
    assert observed, "no run fixtures found in the installed tree at all"
    for stem, obs in observed.items():
        recorded_path = f"runs/{stem}.runproducts.bin"
        assert recorded_path in recorded_hashes, f"{stem}: not in KIT_MANIFEST's own files list at all"
        assert obs["sha256"] == recorded_hashes[recorded_path], (
            f"{stem}: re-hashed inside the installed tree as {obs['sha256']}, but KIT_MANIFEST "
            f"records {recorded_hashes[recorded_path]}"
        )
        recorded_run = manifest_doc["runs"][stem]
        assert obs["config_hash"] == recorded_run["config_hash"], (
            f"{stem}: decoded config_hash {obs['config_hash']!r} from the installed copy does not "
            f"match KIT_MANIFEST's own runs.{stem}.config_hash {recorded_run['config_hash']!r}"
        )
        assert obs["config_hash"], f"{stem}: config_hash decoded to a falsy value -- expected a real 64-hex-char SHA-256"
    return observed


# =================================================================================================
# B.5 -- the image step's refusals, on perturbed copies of the proof kit (question 250(a)).
# Each runs the KIT'S OWN installer on the host, under the docker lock, on a hard-linked copy of the
# kit under the run scratch (files are replaced, never written through a link, so the shared proof
# kit is untouched), and ends with the host's image table exactly as it began.
# =================================================================================================

def _hardlink_copy_kit(src: Path, dst: Path) -> None:
    shutil.copytree(src, dst, symlinks=True, copy_function=os.link)


def _replace_file(path: Path, data: bytes) -> None:
    mode = path.stat().st_mode & 0o7777
    path.unlink()   # never write through a hard link shared with the proof kit
    path.write_bytes(data)
    path.chmod(mode)


def _rewrite_kit_manifest(kit: Path, edit) -> None:
    """Applies `edit(doc)` to `KIT_MANIFEST` and rewrites it with every file hash recomputed: a kit
    whose manifest verifies against its (changed) content -- what someone who can change the kit can
    always produce (`manifest.py`'s top doc: consistency is not authenticity)."""
    doc = json.loads((kit / "KIT_MANIFEST").read_text(encoding="utf-8"))
    edit(doc)
    fresh = kit_manifest.build_manifest(
        kit_root=kit, git_commit=doc["git_commit"], git_dirty=doc["git_dirty"], git_status=doc["git_status"],
        images=doc["images"], sboms=doc["sboms"], packs=doc["packs"], vendor=doc["vendor"], wheels=doc["wheels"],
        binaries=doc["binaries"], runs=doc["runs"], gaps=doc["gaps"],
    )
    (kit / "KIT_MANIFEST").unlink()
    kit_manifest.write_manifest(fresh, kit)
    assert kit_manifest.verify_manifest(kit) == []


def _perturbation_test_body(body) -> None:
    run_id = uuid.uuid4().hex[:12]
    run_scratch = SCRATCH_ROOT / f"perturb-{run_id}"
    run_scratch.mkdir(parents=True, exist_ok=True)
    guard = ResourceGuard(run_id)
    prune_stale_labelled_resources()
    image_tags = ImageTagGuard(run_id, _TEST_TAG_PREFIX, on_test_tag=guard.track_image)
    try:
        _load_bases(image_tags)
        table_before = docker_image_table()
        body(run_id, run_scratch, table_before)
        assert docker_image_table() == table_before, "the host's image table changed across a refused image step"
    finally:
        guard.cleanup()
        shutil.rmtree(run_scratch, ignore_errors=True)
        image_tags.assert_host_tags_unchanged()
    guard.assert_nothing_left()


def _step_args(kit: Path, run_id: str, record: Path) -> tuple[list[str], dict[str, str]]:
    tags = {component: _test_image_tag(run_id, component) for component in sbom.IMAGE_COMPONENTS}
    args = ["--build-images", str(kit), "--record", str(record)]
    for component, tag in tags.items():
        args += ["--image-tag", f"{component}={tag}"]
    args += ["--extract-container-label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--extract-container-label", f"av.test.run_id={run_id}"]
    return args, tags


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_image_step_refuses_a_pinned_base_the_host_does_not_hold_before_any_build():
    """Perturbation 1: the kit pins the edge-plugin base at a digest no image on this host has (the
    Dockerfile's FROM, the installer's own pin and `KIT_MANIFEST.images` all say so, and the manifest
    is re-hashed so the kit verifies). The step must refuse with the typed `base-image-missing`,
    naming the expected reference and digest, BEFORE any build is issued -- so nothing can reach for
    the base on a registry. A build is never attempted here, because `docker build` on a base that
    is absent would contact one; the proof is that the pre-build digest check stands in front of it:
    no build, no image event, no container, no pull."""
    fake_digest = "sha256:" + hashlib.sha256(b"av-fabricated-base-image").hexdigest()

    def body(run_id: str, run_scratch: Path, table_before: dict) -> None:
        kit = run_scratch / "kit-fabricated-base"
        _hardlink_copy_kit(_PROOF_KIT_DIR, kit)
        pin = save_base_images.base_pins()["debian-bookworm-slim-base-image"]
        old_ref, old_digest = pin["reference"], pin["digest"]
        new_ref = old_ref.replace(old_digest, fake_digest)
        for rel in ("images/edge-plugin-image/Dockerfile", "installer/operator_supplied.json"):
            path = kit / rel
            text = path.read_text(encoding="utf-8")
            assert old_digest in text, rel
            _replace_file(path, text.replace(old_digest, fake_digest).encode("utf-8"))

        def edit(doc):
            base = doc["images"]["edge-plugin-image"]["base"]
            assert base["digest"] == old_digest
            base["digest"], base["reference"] = fake_digest, new_ref
        _rewrite_kit_manifest(kit, edit)
        assert _docker("image", "inspect", new_ref, check=False).returncode != 0, "the fabricated base unexpectedly exists on this host"

        args, tags = _step_args(kit, run_id, run_scratch / "record")
        with _ImageEvents(run_scratch / "docker-events.jsonl") as events:
            result = _run_kit_installer(kit, args)
        assert result.returncode == 2, f"expected the typed refusal (exit 2), got rc={result.returncode}\n{result.stdout}\n{result.stderr}"
        assert "REFUSED [base-image-missing]" in result.stderr, result.stderr
        assert new_ref in result.stderr and fake_digest in result.stderr and "docker load" in result.stderr, result.stderr
        assert not (run_scratch / "record").exists()
        all_events = events.events()
        mentions = [e for e in all_events if fake_digest in json.dumps(e) or any(t in json.dumps(e) for t in tags.values())]
        assert mentions == [], f"docker logged events for the refused step: {mentions}"
        assert not [e for e in all_events if e.get("Type") == "image" and e.get("Action") == "pull"], "a pull happened"
        for tag in tags.values():
            assert _docker("image", "inspect", tag, check=False).returncode != 0, f"{tag} exists after a refused step"
        print(f"\n--- perturbation: pinned base absent from the host ---\nrc={result.returncode}\n{result.stderr.strip()}\n"
              f"docker events during the step touching the fabricated digest or the test tags: {len(mentions)}")

    with lock_docker_tests():
        _perturbation_test_body(body)


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_image_step_refuses_an_attacker_consistent_kit_on_the_runtime_content_hash():
    """Perturbation 2: one application file of the edge-plugin image is changed and every hash in
    `KIT_MANIFEST` is recomputed (its `files`, and the `images.*.files` entry of that file), so the
    kit verifies and the build succeeds. The step must then refuse on the runtime-content hash,
    naming the recorded one and the one it computed (here recomputed independently from the
    manifest's per-file hashes), and leave no tag behind -- the cFS image, which built fine first,
    is removed too."""
    def body(run_id: str, run_scratch: Path, table_before: dict) -> None:
        kit = run_scratch / "kit-changed-file"
        _hardlink_copy_kit(_PROOF_KIT_DIR, kit)
        component, image_path = "edge-plugin-image", "/opt/edge-plugin/plugin-config.default.json"
        path = kit / "images" / component / "rootfs" / image_path.lstrip("/")
        changed = path.read_bytes() + b" \n"
        _replace_file(path, changed)
        recorded = json.loads((kit / "KIT_MANIFEST").read_text(encoding="utf-8"))["images"][component]
        new_sha = hashlib.sha256(changed).hexdigest()

        def edit(doc):
            entry = next(f for f in doc["images"][component]["files"] if f["image_path"] == image_path)
            entry["sha256"], entry["size"] = new_sha, len(changed)
        _rewrite_kit_manifest(kit, edit)

        lines = {f["image_path"]: (new_sha if f["image_path"] == image_path else f["sha256"]) for f in recorded["files"]}
        expected_computed = "sha256:" + hashlib.sha256("".join(f"{line}\n" for line in sorted(f"{p} {d}" for p, d in lines.items())).encode()).hexdigest()
        assert expected_computed != recorded["runtime_content_hash"]

        args, tags = _step_args(kit, run_id, run_scratch / "record")
        result = _run_kit_installer(kit, args)
        assert result.returncode == 2, f"expected the typed refusal (exit 2), got rc={result.returncode}\n{result.stdout}\n{result.stderr}"
        assert "REFUSED [runtime-content-mismatch]" in result.stderr, result.stderr
        assert recorded["runtime_content_hash"] in result.stderr and expected_computed in result.stderr, (
            f"the refusal must name both hashes ({recorded['runtime_content_hash']} and {expected_computed}):\n{result.stderr}"
        )
        assert "internally inconsistent" in result.stderr, (
            "the refusal also says the kit's own per-file records do not hash to the recorded value (that is where the file shows up)"
        )
        assert tags["cfs-image"] in result.stdout, "the cFS image was built (and then removed) before the plugin image was refused"
        assert not (run_scratch / "record").exists()
        for tag in tags.values():
            assert _docker("image", "inspect", tag, check=False).returncode != 0, f"{tag} was left behind"
        print(f"\n--- perturbation: attacker-consistent kit, one application file changed ---\nrc={result.returncode}\n{result.stderr.strip()}\n"
              f"expected computed hash (independent): {expected_computed}")

    with lock_docker_tests():
        _perturbation_test_body(body)


def test_the_daemon_access_assertion_refuses_a_socket_a_docker_host_and_privilege(tmp_path):
    """The assertion that backs the lead's ruling must itself refuse what it exists to refuse."""
    clean = ["run", "--name", "n", "--network", "net", "-v", f"{tmp_path}:/kit:ro", "-v", "vol:/target", "img", "/kit/installer/install.sh"]
    _assert_no_docker_daemon_access(clean)
    for bad in (
        [*clean[:5], "-v", "/var/run/docker.sock:/var/run/docker.sock", *clean[5:]],
        [*clean[:5], "-v", "/run/user/1000/podman/podman.sock:/s", *clean[5:]],
        [*clean[:5], "-v", "/Users/someone/.colima/default/docker.sock:/s", *clean[5:]],
        [*clean[:5], "-e", "DOCKER_HOST=tcp://10.0.0.1:2375", *clean[5:]],
        [*clean[:5], "--env", "DOCKER_HOST", *clean[5:]],
        [*clean[:5], "--privileged", *clean[5:]],
        [*clean[:5], "--mount", "type=bind,source=/var/run/docker.sock,target=/d", *clean[5:]],
    ):
        with pytest.raises(AssertionError):
            _assert_no_docker_daemon_access(bad)


# =================================================================================================
# B.6 -- two kits from the same commit have the same manifest hash. Deliberately a SEPARATE,
# UNGATED, fast test (default pack only, no gated flags) -- the expensive, image/wheel/vendor/
# binary-bearing kit this file's main test consumes is built once, out-of-band (see this module's
# own "Gating" doc), and never rebuilt twice just to prove this.
# =================================================================================================

def test_two_fast_kits_from_the_same_commit_have_the_same_manifest_hash(tmp_path):
    site = REPO_ROOT / build_kit.DEFAULT_SITE
    _manifest_a, digest_a = build_kit.build(repo_root=REPO_ROOT, out=tmp_path / "a", site=site)
    _manifest_b, digest_b = build_kit.build(repo_root=REPO_ROOT, out=tmp_path / "b", site=site)
    assert digest_a == digest_b, "two fast kits built from the same commit produced different KIT_MANIFEST hashes"
    assert _manifest_a.read_bytes() == _manifest_b.read_bytes()
