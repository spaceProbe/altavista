"""D3's second half (docs/p5-plan.md, P5 track round 2 tasks 3b and 3c): the zero-egress install
proof. Task 3c closed the one gap task 3b's own harness had to work around by hand -- see "The
viewer's own static assets now come from the kit" below for what changed and what genuinely
remains.

Modelled directly on `tests/test_edge_plugin_container.py` (read that file first) -- the same
labelled `--internal` bridge network, the same `ResourceGuard`/`prune_stale_labelled_resources`/
`lock_docker_tests` discipline, the same raw-connect egress probe idiom, and the same
`_wait_for_container_stdout_lines`-style bounded polling. Its conventions are reused; its text is
not -- every helper below is written fresh for this file's own topology (a kit installed from
scratch inside the enclave, not an already-built plugin image run directly).

# What this file proves, and in what order

1. **Image provenance (question 212).** `docker load -i` each image tarball
   `out/kit/proof/images/*.tar` carries, on the HOST, under `lock_docker_tests()`, and assert the
   loaded image id equals the digest `KIT_MANIFEST` records for that component -- an image is
   trusted only after that comparison, and both digests are printed (question 148).
2. A freshly created, labelled **`--internal`** bridge network -- no route off this host, proven
   (the egress probe below), not merely named.
3. The egress probe, from inside that network, BEFORE any install activity and again AFTER --
   both times a raw connect to two real public addresses fails immediately (< 1s) with an
   ENETUNREACH-shaped error. Naming a public address on an `--internal` network touches no actual
   network (there is no default route to even attempt sending a packet), which is why this
   satisfies question 154 despite the addresses being real.
4. **The install**, inside a labelled container on that same network, with the kit bind-mounted
   **read-only**: `scripts/kit/install.sh <kit> <fresh-dir-in-a-labelled-volume>`, using
   `--no-index` for the one `pip install` it runs. Must succeed with the container's OWN network
   already proven to have no route out.
5. **The demo path**, run from the just-installed tree, inside the same network, in order:
   a. the viewer server, started from the INSTALLED wheels (`altavista.server.create_app`, the
      package `python -m altavista` itself is), proven alive by a real HTTP GET over loopback from
      a second container sharing its network namespace;
   b. `av-ingest-server`, started from the kit's own cross-built `binaries/av-ingest-server`,
      proven alive by its own `GRPC_LISTENING`/`ADMIN_LISTENING` readiness lines;
   c. the edge plugin, from the image this file itself just `docker load`-ed in step 1 (not
      whatever tag happened to already be on the host), joined to the ingest's network namespace
      (`--network container:<ingest>`), delivering the pinned demo-ground-segment batch set that
      `crates/av-edge/tests/fixtures/ground_segment/README.md` documents and
      `tests/test_edge_plugin_container.py` already proves the identical numbers for;
   d. the recorded kernel runs (`tests/fixtures/*.runproducts.bin`, carried in the kit's own
      `runs/`), re-hashed INSIDE the installed tree and compared to what `KIT_MANIFEST` records,
      decoded far enough (`build_kit.read_run_provenance`, reused, not reimplemented) to assert
      each fixture's own `config_hash` field, not merely its file hash;
   e. **`av-command` is a named gap of the kit itself** (its cross-build fails against the pinned
      `rust:1.85-bookworm` toolchain -- `regorus` 0.12.0 needs a newer `const fn` feature; see the
      kit's own `KIT_MANIFEST["gaps"]` entry `av-command-binary` for the real compiler error). This
      test does not attempt to fix that (not this task's job) -- it reads and reports the kit's own
      recorded reason, and does not start a command service.
6. **Two kits built from the same commit have the same manifest hash** -- a separate, UNGATED,
   fast test (`test_two_fast_kits_from_the_same_commit_have_the_same_manifest_hash`, below): builds
   two minimal kits (default pack only, no gated flags) into `tmp_path` and compares their
   `KIT_MANIFEST` bytes. Deliberately not the expensive, image/wheel/vendor/binary-bearing kit this
   file's main test consumes -- that one is built once, out-of-band, and reused (see "Gating",
   below), exactly the way `tests/test_kit_manifest.py::
   test_two_image_bearing_kits_from_the_same_commit_have_the_same_manifest_and_tarball_hashes`
   already treats its own expensive case separately from the cheap default-gate one.

# The viewer's own static assets now come from the kit (task 3c) -- what was fixed, and what
# genuinely remains

Task 3b's own first cut of this test proved everything EXCEPT the viewer's own static/config
assets came from the kit alone: `altavista/server.py::create_app` eagerly constructs `starlette.
staticfiles.StaticFiles(directory=web_dir)` for the frontend, and `altavista/profile.py`'s
`PROFILES_DIR` is a fixed constant (`Path(__file__).resolve().parent.parent / "profiles"`).
Neither `web/` nor `profiles/` was part of the `altavista` wheel then, or now (`pyproject.toml`'s
`[tool.setuptools.packages.find]` only ever includes `altavista*` -- confirmed directly: `python
-m altavista serve` from a wheel-only install is unable to even construct its own ASGI app,
`StaticFiles.__init__` raising `RuntimeError: Directory '.../web' does not exist` before a single
route is registered). Task 3b's own harness worked around this by bind-mounting THIS WORKTREE's
own `web/`/`profiles/` into the viewer's container, read-only, and said so honestly in this same
place -- a real, if narrow, hole in the round's headline "the demo path runs from the installed
kit alone" claim, since a genuinely air-gapped install with no access to this worktree could not
have started the viewer that way.

**Task 3c closes that hole at the kit/install layer**: `scripts/kit/manifest.py` gained two new
named packs, `web` and `profiles` (`manifest.PACKS`), copied into every kit UNCONDITIONALLY
(`manifest.ALWAYS_COPY_PACKS` -- no flag, not even `--copy-pack`, since an installed viewer
cannot start without them and the combined cost is trivial: 3.7 MB / 117 files / 2 pack-internal
symlinks for `web`, 52 KB / 7 files for `profiles`, measured). `scripts/kit/install.py` now
refuses a kit that somehow lacks either (`_check_required_viewer_assets`, mirroring its existing
wheels refusal) and names both in `INSTALL_RECORD["installed"]["viewer_assets"]`
(`{"web_dir": "kit/packs/web", "profiles_dir": "kit/packs/profiles"}`). Below,
`_run_viewer_from_installed_kit` now points `web_dir`/`PROFILES_DIR` at those INSTALLED paths --
`/target/kit/packs/web`, `/target/kit/packs/profiles`, both already reachable through the
`install_volume` mount every step here already uses -- and bind-mounts nothing from this worktree
at all. Every byte the viewer serves, and every line of code that runs, is now the kit's own.

**What remains, and is NOT this task's job to fix** (`altavista/**` and `pyproject.toml` are
explicitly out of scope for this worker -- a decision for the lead): the `altavista` WHEEL itself
still ships no `web/`/`profiles/` (the same `pyproject.toml` gap, unchanged), and `altavista.
profile.PROFILES_DIR` is still a fixed module constant with no parameter or CLI flag -- there is
no other way to point it anywhere but monkeypatching it, so `_viewer_bootstrap_script` still does
exactly that, now against the INSTALLED copy rather than this worktree's. `web_dir`, by contrast,
is a real, already-supported `create_app` parameter, used here for exactly its intended purpose
(an externally-configured deploy-time static root, precisely like `--textures` already is) -- not
a workaround, just its ordinary calling convention. Closing the wheel-packaging gap for good
(shipping `web/`/`profiles/` as package data, or giving the CLI a `--profiles-dir` flag matching
`web_dir`'s own shape) is a `pyproject.toml`/`altavista/__main__.py`/`altavista/profile.py` fix,
recorded here for the lead, not attempted by this task.

# Gating (question 194) -- computed once, at import time

`_compute_skip_reason()` requires: Docker; the `python:3.13-slim` probe image already present
(never pulled here, question 154); and the EXPENSIVE proof kit already built, out-of-band, at
`out/kit/proof` (`_KIT_DIR`), with its `images`/`wheels`/`binaries`/`packs` sections all actually
collected and its own manifest verifying clean. Building that kit takes real minutes (a Rust cross-
build, `cargo vendor`, a `pip download` against PyPI, two `docker save`s) and reaches the network
(question 154's one build-time exception) -- doing that inside a test would violate "no network at
test time" outright, so this test never builds it; it only ever consumes an already-built one and
skips, by name, with the exact command that builds it, when it is missing. This mirrors `tests/
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
"""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
import time
import uuid
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import manifest as kit_manifest  # noqa: E402
import sbom  # noqa: E402

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

PROBE_IMAGE = "python:3.13-slim"

# (task 3c) Paths INSIDE the installed tree -- reachable through the `install_volume` mount every
# step here already uses (`-v f"{install_volume}:/target:ro"`), never a bind-mount of this
# worktree's own `web/`/`profiles/` (see this module's own top doc, "The viewer's own static
# assets now come from the kit"). Matches `INSTALL_RECORD["installed"]["viewer_assets"]` exactly.
INSTALLED_WEB_DIR = "/target/kit/packs/web"
INSTALLED_PROFILES_DIR = "/target/kit/packs/profiles"

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


def _proof_kit_missing_sections(doc: dict) -> list[str]:
    missing = []
    for component in sbom.IMAGE_COMPONENTS:
        if not doc.get("images", {}).get(component, {}).get("collected"):
            missing.append(f"images.{component}")
    if not doc.get("wheels", {}).get("collected"):
        missing.append("wheels")
    if not doc.get("vendor", {}).get("collected"):
        missing.append("vendor")
    if not doc.get("binaries", {}).get("results", {}).get("av-ingest-server", {}).get("included"):
        missing.append("binaries.av-ingest-server")
    pack = doc.get("packs", {}).get("data-time")
    if not pack or not pack.get("copied"):
        missing.append("packs.data-time (copied)")
    # (task 3c) `web`/`profiles` are copied unconditionally by any kit `build_kit.py` itself
    # writes (`manifest.ALWAYS_COPY_PACKS`) -- checked explicitly anyway, the same defence in
    # depth `install.py::_check_required_viewer_assets` applies, so a stale kit built before task
    # 3c (or one missing them for any other reason) is skipped with a clear reason rather than
    # failing deep inside `_run_viewer_from_installed_kit`.
    for name in ("web", "profiles"):
        pack = doc.get("packs", {}).get(name)
        if not pack or not pack.get("copied"):
            missing.append(f"packs.{name} (copied)")
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
    missing = _proof_kit_missing_sections(doc)
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

    def label_args(self) -> list[str]:
        return ["--label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--label", f"av.test.run_id={self.run_id}"]

    def track_container(self, name: str) -> None:
        self.containers.append(name)

    def track_network(self, name: str) -> None:
        self.networks.append(name)

    def track_volume(self, name: str) -> None:
        self.volumes.append(name)

    def cleanup(self) -> None:
        for name in self.containers:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=30)
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
        # This test builds no image of its own (it only ever loads the kit's own tarballs under
        # their already-recorded tags, and runs PROBE_IMAGE/those loaded tags) -- confirm none
        # carries this run's label rather than silently assuming it.
        remaining_images = _docker("images", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_images == "", f"no image should ever carry {label_filter} (this test creates none), but found: {remaining_images!r}"


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


def _load_and_verify_images(guard: ResourceGuard) -> dict:
    """B.1: `docker load -i` each image tarball the kit carries, on the host, and compare the
    loaded image id to the digest `KIT_MANIFEST` records for that component -- an image is
    trusted only after this comparison (question 212). Runs BEFORE any network/volume/container
    of this test's own is created; needs no label of its own (it never creates a container)."""
    manifest_doc = json.loads((_PROOF_KIT_DIR / "KIT_MANIFEST").read_text(encoding="utf-8"))
    evidence = {}
    for component in sbom.IMAGE_COMPONENTS:
        entry = manifest_doc["images"][component]
        tarball = _PROOF_KIT_DIR / "images" / f"{component}.tar"
        assert tarball.is_file(), f"{component}: kit claims images.{component}.collected but {tarball} is missing"
        load = subprocess.run(["docker", "load", "-i", str(tarball)], capture_output=True, text=True, timeout=120)
        assert load.returncode == 0, f"docker load -i {tarball} failed (rc={load.returncode}): {load.stderr}"
        loaded_id = subprocess.run(
            ["docker", "image", "inspect", entry["tag"], "--format", "{{.Id}}"],
            capture_output=True, text=True, timeout=30,
        )
        assert loaded_id.returncode == 0, f"docker image inspect {entry['tag']} failed after loading: {loaded_id.stderr}"
        actual_digest = loaded_id.stdout.strip()
        evidence[component] = {
            "tag": entry["tag"],
            "recorded_digest": entry["recorded_digest"],
            "loaded_digest": actual_digest,
            "docker_load_stdout": load.stdout.strip(),
        }
        assert actual_digest == entry["recorded_digest"], (
            f"{component}: KIT_MANIFEST records {entry['recorded_digest']}, but the image loaded "
            f"from {tarball} has id {actual_digest} -- refusing to trust it (question 212)"
        )
    return evidence


def _run_zero_egress_install_and_demo_from_the_kit_alone():
    run_id = uuid.uuid4().hex[:12]
    run_scratch = SCRATCH_ROOT / run_id
    run_scratch.mkdir(parents=True, exist_ok=True)
    guard = ResourceGuard(run_id)

    prune_stale_labelled_resources()

    manifest_doc = json.loads((_PROOF_KIT_DIR / "KIT_MANIFEST").read_text(encoding="utf-8"))
    kit_manifest_sha256 = sbom.sha256_file(_PROOF_KIT_DIR / "KIT_MANIFEST")

    reached: dict[str, bool] = {"4a_viewer": False, "4b_ingest": False, "4c_plugin": False, "4d_runs": False}

    try:
        # ---------------------------------------------------------------------------------
        # B.1 -- image provenance, on the host, before anything else.
        # ---------------------------------------------------------------------------------
        image_evidence = _load_and_verify_images(guard)
        print(f"\n--- image provenance (question 212) ---\n{json.dumps(image_evidence, indent=2)}")

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

        installer_container = _labelled_id(run_id, "installer")
        guard.track_container(installer_container)
        install_start = time.monotonic()
        install_run = _docker(
            "run", "--name", installer_container, "--network", network_name, *guard.label_args(),
            "-v", f"{_PROOF_KIT_DIR}:/kit:ro",
            "-v", f"{KIT_DIR}:/repo/scripts/kit:ro",
            "-v", f"{install_volume}:/target",
            PROBE_IMAGE, "/repo/scripts/kit/install.sh", "/kit", "/target",
            timeout=180.0,
        )
        install_elapsed_s = time.monotonic() - install_start
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
        # (task 3c) INSTALL_RECORD names where the viewer's own static/config assets landed --
        # the exact paths (relative to /target) INSTALLED_WEB_DIR/INSTALLED_PROFILES_DIR
        # hard-code, cross-checked here rather than merely assumed.
        assert install_record["installed"]["viewer_assets"] == {
            "web_dir": "kit/packs/web", "profiles_dir": "kit/packs/profiles",
        }, install_record["installed"]["viewer_assets"]
        print(f"\n--- INSTALL_RECORD (question 148) ---\n{json.dumps(install_record, indent=2)[:2000]}")

        # ---------------------------------------------------------------------------------
        # B.3 (after) -- prove the network STILL has no route out, after the install ran.
        # ---------------------------------------------------------------------------------
        probe_after = _run_egress_probe(guard, run_id, "after-install", network_name)
        print(f"\n--- egress probe, after install (question 154) ---\n{json.dumps(probe_after)}")

        # ---------------------------------------------------------------------------------
        # B.4a -- the viewer server, started from the INSTALLED wheels.
        # ---------------------------------------------------------------------------------
        viewer_evidence = _run_viewer_from_installed_kit(guard, run_id, network_name, install_volume, run_scratch)
        reached["4a_viewer"] = True
        print(f"\n--- viewer server, from the installed wheels (question 148) ---\n{json.dumps(viewer_evidence)}")

        # ---------------------------------------------------------------------------------
        # B.4b -- av-ingest-server, started from the kit's own cross-built binary.
        # ---------------------------------------------------------------------------------
        ingest_container, ingest_evidence = _run_ingest_from_installed_kit(guard, run_id, network_name, install_volume)
        reached["4b_ingest"] = True
        print(f"\n--- av-ingest-server, from the kit's own binary (question 148) ---\n{json.dumps(ingest_evidence)}")

        # ---------------------------------------------------------------------------------
        # B.4c -- the edge plugin, from the image this test itself loaded in B.1.
        # ---------------------------------------------------------------------------------
        plugin_evidence = _run_edge_plugin_from_kit_image(guard, run_id, ingest_container, run_scratch)
        reached["4c_plugin"] = True
        print(f"\n--- edge plugin, from the kit's own loaded image (question 148) ---\n{json.dumps(plugin_evidence)}")

        # ---------------------------------------------------------------------------------
        # B.4d -- the recorded kernel runs, re-hashed and re-decoded from the INSTALLED tree.
        # ---------------------------------------------------------------------------------
        run_evidence = _rehash_and_decode_runs(guard, run_id, network_name, install_volume, manifest_doc)
        reached["4d_runs"] = True
        print(f"\n--- recorded kernel runs, re-hashed from the installed tree (question 148) ---\n{json.dumps(run_evidence, indent=2)}")

        # ---------------------------------------------------------------------------------
        # B.4e -- av-command: a named gap of the KIT itself, read and reported, not fixed.
        # ---------------------------------------------------------------------------------
        av_command_gap = next((g for g in manifest_doc["gaps"] if g["name"] == "av-command-binary"), None)
        assert av_command_gap is not None, "expected the kit to record av-command-binary as a gap (its cross-build is known to fail against the pinned toolchain)"
        print(f"\n--- av-command (named gap, not attempted -- question 148) ---\n{av_command_gap['reason'][-400:]}")

        print(f"\n--- B.4 reached: {reached} ---")

    finally:
        guard.cleanup()
        shutil.rmtree(run_scratch, ignore_errors=True)

    guard.assert_nothing_left()

    assert all(reached.values()), f"did not reach every step of the demo path: {reached}"


def _viewer_bootstrap_script() -> str:
    """See this module's own top doc, "The viewer's own static assets now come from the kit" --
    every line of CODE here is the installed wheel's own (`altavista.server.create_app`,
    `uvicorn`); `web_dir` is a real, already-supported parameter used for its intended purpose,
    pointed at the INSTALLED `web/` pack (`INSTALLED_WEB_DIR`, reachable through the
    `install_volume` mount alone -- no bind-mount of this worktree's own `web/` anywhere), and
    `altavista.profile.PROFILES_DIR` is monkeypatched (the one piece of this path with no
    parameter at all -- a real, named packaging gap, see this module's own top doc) to point at
    the INSTALLED `profiles/` pack the same way."""
    return (
        "import pathlib\n"
        "import altavista.profile as _profile_mod\n"
        f"_profile_mod.PROFILES_DIR = pathlib.Path({INSTALLED_PROFILES_DIR!r})\n"
        "import uvicorn\n"
        "from altavista.server import create_app\n"
        f"app = create_app(web_dir={INSTALLED_WEB_DIR!r}, texture_dir='/nonexistent-textures')\n"
        "print('APP_CREATED_OK', flush=True)\n"
        "uvicorn.run(app, host='0.0.0.0', port=8765, log_level='info')\n"
    )


def _run_viewer_from_installed_kit(guard: ResourceGuard, run_id: str, network_name: str, install_volume: str, run_scratch: Path) -> dict:
    bootstrap_path = run_scratch / "viewer_bootstrap.py"
    bootstrap_path.write_text(_viewer_bootstrap_script(), encoding="utf-8")

    viewer_container = _labelled_id(run_id, "viewer")
    guard.track_container(viewer_container)
    _docker(
        # (task 3c) `install_volume` is the ONLY content mount here -- it already carries
        # `kit/packs/web` and `kit/packs/profiles` (INSTALLED_WEB_DIR/INSTALLED_PROFILES_DIR,
        # above) from the kit's own install, so no bind-mount of this worktree's own `web/` or
        # `profiles/` is needed, or present, anywhere in this test.
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
        "r = urllib.request.urlopen('http://127.0.0.1:8765/api/scenarios', timeout=5)\n"
        "print(json.dumps({'status': r.status, 'body': json.loads(r.read())}))\n",
    )
    result = json.loads(http.stdout.strip().splitlines()[-1])
    assert result["status"] == 200, f"GET /api/scenarios did not return 200: {result}"
    assert result["body"] == {"names": []}, f"unexpected body from a freshly started viewer: {result}"
    return {"http_status": result["status"], "body": result["body"], "startup_log_tail": log_lines[-5:]}


def _run_ingest_from_installed_kit(guard: ResourceGuard, run_id: str, network_name: str, install_volume: str) -> tuple[str, dict]:
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
        "av-edge-plugin:local",  # the SAME image this test loaded from the kit's own tarball in B.1
        "--grpc-bind", "127.0.0.1:50070", "--admin-bind", "127.0.0.1:50071",
        "--log-dir", "/data/ingest-log",
        "--no-require-client-cert", "--verify-key", f"{PRODUCER_ID}:/keys/verify.pub.pem",
        "--clearance-ladder", CLEARANCE_LADDER, "--max-batch-age-ns", str(MAX_BATCH_AGE_NS),
        "--clock-tai-ns", str(CLOCK_TAI_NS),
    )
    log_lines = _wait_for_container_log_substring(ingest_container, ("GRPC_LISTENING", "ADMIN_LISTENING"), timeout_s=15.0)
    return ingest_container, {"readiness_log_tail": log_lines[-5:]}


def _run_edge_plugin_from_kit_image(guard: ResourceGuard, run_id: str, ingest_container: str, run_scratch: Path) -> dict:
    plugin_container = _labelled_id(run_id, "plugin")
    guard.track_container(plugin_container)
    plugin_run = subprocess.run(
        [
            "docker", "run", "--name", plugin_container, "--network", f"container:{ingest_container}", *guard.label_args(),
            "-v", f"{SIGNING_KEY_PEM}:/keys/signing.pem:ro",
            "av-edge-plugin:local",
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
