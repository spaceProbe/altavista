"""E4b (`docs/edge-plan.md` milestone E4, "the plugin as a labelled container";
`docs/open-questions.md` questions 148/154/156/194/196(d)): proves that `av-edge-plugin:local`
(`services/edge-plugin/Dockerfile`, built by `services/edge-plugin/build-image.sh`) really
runs as a labelled container with `--network none` plus its allowed endpoint(s) -- not
merely that the Dockerfile parses.

Docker-gated, and gated the way `services/cfs/tests/test_image_digest.py` already does it
for a *Python* test (question 194): a module-level `_compute_skip_reason()` runs once at
import time, and `pytest.mark.skipif` on every gated test uses its result as the printed
reason. `tests/test_lockstep_ref.py`'s own comment on this repository's `pyproject.toml`
`-rs` setting is why this is sufficient here without the extra "nested cargo test
subprocess" plumbing `crates/av-lockstep/tests/docker_lifecycle.rs::
announce_gate_skip_is_actually_visible_in_a_real_cargo_test_subprocess_without_nocapture`
needed: that plumbing exists because Rust's libtest swallows a test's own captured stdout on
success and only a raw, unbuffered write survives -- a concern about `println!`/`eprintln!`
under `cargo test`'s harness, not about `pytest.skip`/`pytest.mark.skipif`, which `-rs`
already surfaces in the terminal summary reliably (verified directly for this exact
mechanism by `tests/test_lockstep_ref.py`'s own module doc). This module therefore mirrors
`test_image_digest.py`'s Python-native pattern precisely, not the Rust one.

# The network posture this file proves, and why it is built this way

Milestone E4 says the plugin runs "with `--network none` plus its two allowed endpoints."
Read literally this is self-contradictory: a container with no network namespace at all can
reach nothing, by definition -- there is no such thing as "`--network none` plus an
endpoint." Two facts, both verified below rather than assumed, resolve it:

1. **What the plugin binary actually dials.** Reading `crates/av-ingest-client/src/bin/
   av-edge-plugin.rs::Client::connect`/`run`: this binary opens exactly ONE outbound network
   connection -- `--endpoint` -- and reuses that single connection for both `Announce` and
   `Submit`. There is no second endpoint anywhere in its own code path (no DNS query beyond
   whatever the OS resolver does for the address string given, no telemetry call, no second
   RPC target). So "two allowed endpoints" is not two things *the plugin* dials.
2. **What "loopback-only" actually means here, verified the hard way.** Both
   `EdgeIngestClient::connect_plaintext` (the client side, `crates/av-ingest-client/src/
   lib.rs`) and `crate::server::bind_loopback` (the server side, `crates/av-ingest/src/
   server.rs`) *refuse a non-loopback address outright, before ever touching the network* --
   measured directly while building this test: binding `av-ingest-server`'s own
   `--grpc-bind`/`--admin-bind` to `0.0.0.0` (so a *different* container's IP could reach it)
   is refused immediately with "is not a loopback address -- refusing a plaintext bind
   (question 155/202)". This is deliberate, ADR-004-level policy, not a bug this test works
   around: the plaintext path is for one host (one network namespace); anything else goes
   through a service-owned nginx mTLS front (E3b), which is a separate, already-tested
   concern (`tests/test_edge_ingest_mtls.py`) this file does not re-prove.

Given both facts, the plugin and the ingest it talks to plaintext MUST share one network
namespace -- Docker's own `--network container:<name>` (the same primitive Kubernetes builds
"one pod, shared localhost" on) is exactly that, applied across two separate containers
(separate filesystems, separate process trees, ONE shared network device set). This test:

- Starts `av-ingest-server` (cross-built for Linux the same way `services/edge-plugin/
  build-image.sh` cross-builds the plugin -- see `_cross_build_ingest_server_binary` below)
  as its own container, attached to a freshly created, labelled, **`--internal`** bridge
  network (question 156: labelled; `--internal` because question 154 forbids network use at
  *test* time and this is the deliberate way to make "no route off this host" a *property*
  of the network, not a promise) -- bound to `127.0.0.1:50070` (gRPC) and `127.0.0.1:50071`
  (admin/evidence), the only two listeners this deployment has.
- Starts the plugin **joined to that same container's network namespace**
  (`--network container:<ingest>`) -- from the plugin's own point of view, `127.0.0.1:50070`
  really is loopback (it is the same network device the ingest bound), so
  `connect_plaintext`'s own loopback check passes for the *real* reason (one network
  namespace), not by accident.
- Reads `/admin/api/evidence` back the same way: a disposable `python:3.13-slim` container
  (already present locally per this host's own inventory -- never pulled here, question 154)
  **also** joined to the ingest's network namespace, since that admin port is
  `127.0.0.1`-bound for the identical reason the gRPC one is -- nothing outside that shared
  namespace can reach either one. **This is "the two allowed endpoints", concretely**: the
  ingest's own two listeners, both reachable *only* from inside that one shared, off-host-
  unreachable namespace -- never published to the Docker host, never reachable from any other
  container. The plugin itself uses only the first of the two; the second exists for the
  operator/evidence-collection role E3's own `/admin/api/evidence` charter already assigns it
  (question 63), reached here by the test harness standing in for that role.
- Proves the negative half with the real, unmodified `av-edge-plugin:local` image run with
  a bare `--network none` (no shared namespace, no bridge, nothing but `lo`): connecting
  anywhere non-loopback fails immediately, typed, never a hang -- exactly the brief's own
  suggested probe ("attempting to connect to an address and getting an immediate, typed
  network-unreachable error is a proof that needs no server on the other end"), corroborated
  by a raw OS-level socket connect from the same kind of network-less container, which is
  measured (not assumed) to fail with `OSError: [Errno 101] Network is unreachable` in
  effectively 0 seconds.
- Proves the `--internal` network's own "no route off this host" claim the same way, from
  *inside* the shared namespace this time: a raw connect to two well-known, unrelated public
  addresses fails immediately with the identical `ENETUNREACH`, never a hang and never any
  packet actually leaving (there is no default route to leave by) -- so this probe touches no
  real network despite naming public IPs, satisfying question 154's "no network at test
  time" by the same logic the brief itself gives for why this kind of probe needs no server
  on the other end.

# Why every bind-mount source lives under `.av-test-tmp/`, not `tempfile`/`tmp_path`

Measured directly while building this test (a real defect this task found and routes
around, not a theoretical concern): this host runs Docker through Colima, whose default
configuration mounts **only `$HOME`** into its VM ("Colima default behaviour: `$HOME` is
mounted as writable" -- `~/.colima/default/colima.yaml`'s own comment). A `docker run -v`
source outside `$HOME` -- which includes both Python's `tempfile` default directory and
pytest's own default `tmp_path`/`tmp_path_factory` base (`/var/folders/.../T/...` on macOS,
and this task's own scratch directory under `/private/tmp/...`) -- silently does **not**
bind-mount the file at all: Docker instead creates an **empty directory** at the container
side path with no error anywhere. A signing key or CA file mounted this way is silently
"present" as a directory, and every consumer of it fails downstream with a confusing
unrelated error. This test therefore never uses `tmp_path`/`tempfile` for anything that
will be bind-mounted -- only `SCRATCH_ROOT` below (`<repo>/.av-test-tmp/`, `.gitignore`d),
which is under `$HOME` by construction.

# Question 199 (no test mutates the process environment)

Every `docker`/subprocess call below either passes no extra environment at all (the
`docker`/`python3` CLIs this file shells out to need none) or, where a value must vary, passes
it as a command-line argument or a bind-mounted file -- never `os.environ[...] = ...` and
never a mutated copy handed to `env=` for anything that did not already need one. Unlike
`tests/test_edge_ingest_mtls.py` (which runs `cargo build` as a **host** subprocess and so
needs `PATH`/`GMAT_ROOT`/`CFS_MIRROR_DIR` on that subprocess's own `env=`), every Rust build
this file needs happens **inside a container** (`docker run rust:1.85-bookworm ...`), which
needs no host toolchain environment forwarded to it at all.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import time
import uuid
from pathlib import Path

import pytest

from altavista.docker_test_lock import lock_docker_tests

REPO_ROOT = Path(__file__).resolve().parents[1]
EDGE_PLUGIN_DIR = REPO_ROOT / "services" / "edge-plugin"
DOCKERFILE = EDGE_PLUGIN_DIR / "Dockerfile"
BUILD_SCRIPT = "services/edge-plugin/build-image.sh"
IMAGE_TAG = "av-edge-plugin:local"
PROBE_IMAGE = "python:3.13-slim"

FIXTURES_DIR = REPO_ROOT / "crates" / "av-edge" / "tests" / "fixtures" / "ground_segment"
SIGNING_KEY_PEM = REPO_ROOT / "crates" / "av-edge" / "tests" / "fixtures" / "test_signing_key.pem"
VERIFY_PUB_PEM = REPO_ROOT / "crates" / "av-edge" / "tests" / "fixtures" / "test_signing_key.pub.pem"

# Pinned demo-DRM values (crates/av-edge/tests/fixtures/ground_segment/README.md; the same
# constants crates/av-ingest/tests/plugin_wire.rs and crates/av-edge/tests/plugin_replay.rs
# already pin) -- this file's own proof that the container path reproduces them byte for byte
# is what makes this a milestone-E4 acceptance test, not merely a smoke test.
PRODUCER_ID = "demo-ground-segment-flight-plugin"
SHARD_KEY = "ground-segment-demo"
EXPECTED_BATCH_COUNT = 900
EXPECTED_CHAIN_HEAD_HEX = "d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698"
CLEARANCE_LADDER = "UNCLASSIFIED,CUI"
MAX_BATCH_AGE_NS = 10_000_000_000_000
# crates/av-ingest/tests/plugin_wire.rs's own `NOW` -- the fixture's own recorded creation
# time; the ingest's injected clock must be at or after every batch's own epoch, or every
# batch is rejected STALE (question 189's per-message epochs).
CLOCK_TAI_NS = 1_767_225_637_000_000_000

# crates/av-lockstep/src/docker.rs's own established convention (`TEST_LABEL_KEY`/
# `TEST_LABEL_VALUE`), reused verbatim here -- this is a *test* resource, not the persistent
# build artifact `services/edge-plugin/Dockerfile` produces (that one carries only
# `org.altavista.project`/`org.altavista.component`, never `av.test`), so cleanup below can
# never mistake one for the other.
TEST_LABEL_KEY = "av.test"
TEST_LABEL_VALUE = "1"

# See this module's own doc, "Why every bind-mount source lives under .av-test-tmp/, not
# tempfile/tmp_path" -- must be under $HOME for Colima to actually bind-mount it.
SCRATCH_ROOT = REPO_ROOT / ".av-test-tmp" / "edge_plugin_container"

PREBUILD_BASE_IMAGE = "rust:1.85-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4"


# =================================================================================================
# Gating (question 194): a typed reason, computed once, asserted-by-name via pytest.mark.skipif.
# =================================================================================================


def _docker_unavailable_reason() -> "str | None":
    """`None` iff `docker info` succeeds -- mirrors `services/cfs/tests/
    test_image_digest.py::docker_available` exactly, restated as a reason string rather than
    a bool so the skip names *why*, not just *that*."""
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


def _compute_skip_reason() -> "str | None":
    reason = _docker_unavailable_reason()
    if reason is not None:
        return f"Docker not available: {reason}"
    if not _image_present(IMAGE_TAG):
        return (
            f"image {IMAGE_TAG!r} has not been built on this host -- this test only inspects/runs an "
            f"already-built image, it never builds one (question 194: a test that finds no image and "
            f"returns is a defect, so this is a visible skip, not a silent pass). Run `{BUILD_SCRIPT}` "
            f"on a host with Docker and network access, then re-run this test."
        )
    if not _image_present(PROBE_IMAGE):
        return (
            f"probe image {PROBE_IMAGE!r} (used only to read back /admin/api/evidence and to run a "
            f"raw-socket network-reachability check from a second, disposable container -- never to "
            f"replace anything this task built) is not present locally, and this test never pulls an "
            f"image itself (question 154: no network at test time). Run `docker pull {PROBE_IMAGE}` "
            f"once, on a host with network access, then re-run this test."
        )
    return None


_SKIP_REASON = _compute_skip_reason()

# Missing-image text Docker itself emits when a `docker run` names a tag that is not local and
# cannot be pulled. Matched, rather than re-inspecting, so a disappearance is caught at the exact
# call that tripped over it.
_MISSING_IMAGE_MARKER = "Unable to find image"


def _skip_if_image_vanished_mid_run(stage: str, result: subprocess.CompletedProcess) -> None:
    """Turn an image that vanished *after* `_SKIP_REASON` was computed into a visible skip
    naming the stage it disappeared at, not an assertion failure.

    This is not laxity, it is this host's measured reality. `_SKIP_REASON` is evaluated once at
    collection time (`pytest.mark.skipif`'s own contract), and on this host the image is
    deleted from under a running test by a host-level image garbage collection -- the same
    phenomenon `docs/open-questions.md` question 196(d) records for the cFS image, observed
    twice against this image inside a single afternoon, once between collection and the first
    `docker run` and once between this test's own Part 1 and Part 2. The measured cause is disk
    pressure, not an actor running `docker image prune`: the Colima VM's container filesystem
    sits at 92% (4.2 GB free of 58.8 GB, with 41.85 GB reclaimable in an unrelated workload's
    volumes), and the deletions land inside another process's `docker build` layer-allocation
    window. Question 194's rule is "run for real or skip visibly, never pass silently", and a
    skip whose reason says the image was deleted mid-run is exactly that -- distinct, in wording
    and in meaning, from `_SKIP_REASON`'s "has not been built on this host", so `-rs` output
    never conflates the two. Rebuild with `services/edge-plugin/build-image.sh` and re-run."""
    if _MISSING_IMAGE_MARKER in (result.stderr or ""):
        pytest.skip(
            f"image {IMAGE_TAG!r} was present when this test was collected but had been deleted "
            f"from this host by the time stage {stage!r} ran it -- a host-level image garbage "
            f"collection under disk pressure (see this helper's own docstring and question "
            f"196(d)), not a defect in what is under test and not a silent pass. Rebuild with "
            f"`{BUILD_SCRIPT}` and re-run. Docker's own words: "
            f"{result.stderr.strip().splitlines()[0] if result.stderr.strip() else '(no stderr)'}"
        )


# =================================================================================================
# Small docker/subprocess helpers -- no process-environment mutation anywhere (question 199):
# every subprocess below either forwards no extra env at all, or receives values as CLI args /
# bind-mounted files.
# =================================================================================================


def _docker(*args: str, timeout: float = 60.0, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        _skip_if_image_vanished_mid_run(" ".join(args[:3]), result)
        pytest.fail(f"`docker {' '.join(args)}` failed (rc={result.returncode}):\n--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}")
    return result


def _labelled_id(run_id: str, kind: str) -> str:
    return f"av-edge-plugin-test-{kind}-{run_id}"


class ResourceGuard:
    """Tracks exactly the container/network names THIS test run created, and removes exactly
    those and nothing else (question 156: "the image guard removes only the exact tag it
    created", applied here to containers and a network instead of an image tag). Every
    resource also carries `TEST_LABEL_KEY`/`av.test.run_id` so `assert_nothing_left` can
    cross-check "removed by name" against "nothing with this run's label remains" -- two
    independent ways of asking the same question, deliberately not sharing one code path."""

    def __init__(self, run_id: str):
        self.run_id = run_id
        self.containers: list[str] = []
        self.networks: list[str] = []

    def label_args(self) -> list[str]:
        return ["--label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--label", f"av.test.run_id={self.run_id}"]

    def track_container(self, name: str) -> None:
        self.containers.append(name)

    def track_network(self, name: str) -> None:
        self.networks.append(name)

    def cleanup(self) -> None:
        for name in self.containers:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=30)
        for name in self.networks:
            subprocess.run(["docker", "network", "rm", name], capture_output=True, timeout=30)

    def assert_nothing_left(self) -> None:
        label_filter = f"label=av.test.run_id={self.run_id}"
        remaining_containers = _docker("ps", "-a", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_containers == "", f"container(s) labelled {label_filter} still exist after cleanup: {remaining_containers!r}"
        remaining_networks = _docker("network", "ls", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_networks == "", f"network(s) labelled {label_filter} still exist after cleanup: {remaining_networks!r}"
        # This test never builds or tags an image of its own (it only ever runs the
        # already-built IMAGE_TAG and the already-present PROBE_IMAGE) -- so there is, by
        # construction, no image for this guard to have created or to need removing. Stated
        # explicitly, not silently assumed: confirm neither carries this run's own label.
        remaining_images = _docker("images", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_images == "", f"no image should ever carry {label_filter} (this test creates none), but found: {remaining_images!r}"


def _cross_build_ingest_server_binary(dest_dir: Path) -> Path:
    """Cross-builds `av-ingest-server` (crates/av-ingest) for Linux, the identical way
    `services/edge-plugin/build-image.sh` cross-builds `av-edge-plugin` -- see that script's
    own header comment / `services/edge-plugin/Dockerfile`'s "Why this Dockerfile has no
    Rust builder stage" for why a bind-mounted `docker run` is required at all (the
    workspace's `spoore-cdm` path dependency). `av-ingest-server` is deliberately NOT part of
    `services/edge-plugin/Dockerfile`'s own deliverable image (that Dockerfile packages the
    plugin only) -- this test needs a real ingest to prove the plugin actually delivers
    batches somewhere, so it builds one for itself, independent of the plugin's own image."""
    scratch_target = REPO_ROOT / "target-docker-linux"  # already .gitignore'd/.dockerignore'd
    shutil.rmtree(scratch_target, ignore_errors=True)
    build = subprocess.run(
        [
            "docker", "run", "--rm",
            "-v", f"{REPO_ROOT}:/workspace",
            "-v", "/Users/probe/code/spoore:/Users/probe/code/spoore:ro",
            "-w", "/workspace",
            PREBUILD_BASE_IMAGE,
            "bash", "-c",
            "apt-get update -qq && apt-get install -y -qq --no-install-recommends "
            "protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && "
            "cargo build --release -p av-ingest --bin av-ingest-server --target-dir /workspace/target-docker-linux && "
            "strip /workspace/target-docker-linux/release/av-ingest-server",
        ],
        capture_output=True, text=True, timeout=600,
    )
    if build.returncode != 0:
        shutil.rmtree(scratch_target, ignore_errors=True)
        pytest.fail(f"cross-building av-ingest-server failed (rc={build.returncode}):\n--- stdout ---\n{build.stdout}\n--- stderr ---\n{build.stderr}")
    built = scratch_target / "release" / "av-ingest-server"
    assert built.is_file(), f"cross-build reported success but {built} does not exist"
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest = dest_dir / "av-ingest-server"
    shutil.copy2(built, dest)
    dest.chmod(0o755)
    shutil.rmtree(scratch_target, ignore_errors=True)
    return dest


def _wait_for_container_stdout_lines(name: str, prefixes: tuple[str, ...], timeout_s: float) -> list[str]:
    """Polls `docker logs` until every one of `prefixes` has appeared as the start of some
    line (bounded, never a bare `sleep N` guess) -- `av-ingest-server`'s own
    `GRPC_LISTENING`/`ADMIN_LISTENING` readiness lines, read back through `docker logs`
    rather than a piped stdout handle, since `docker run -d` detaches immediately."""
    deadline = time.monotonic() + timeout_s
    while True:
        logs = _docker("logs", name, check=False).stdout
        lines = logs.splitlines()
        if all(any(line.startswith(p) for line in lines) for p in prefixes):
            return lines
        if time.monotonic() > deadline:
            pytest.fail(f"container {name} never printed all of {prefixes} within {timeout_s}s -- logs so far:\n{logs}")
        time.sleep(0.1)


# =================================================================================================
# The test.
# =================================================================================================


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_network_none_denies_everything_and_the_internal_network_delivers_batches_to_a_real_ingest():
    # Question 207: every container this test creates carries `av.test`/`av.test.run_id`
    # (`ResourceGuard.label_args()` above) -- exactly the label
    # `av_lockstep::docker::prune_stale_test_resources` sweeps DAEMON-WIDE. A concurrent Rust
    # `cargo test` process in ANY worktree on this host calling that sweep mid-run could tear
    # this test's own containers out from under it -- the identical exposure question 207 found
    # on the Rust side. Held for this whole test body (not just around any one docker command),
    # via the SAME `$HOME`-rooted lock file the Rust side uses (`altavista.docker_test_lock`'s
    # own module doc names the exact path and the cross-language proof that both sides agree on
    # it).
    with lock_docker_tests():
        _run_network_none_denies_everything_and_the_internal_network_delivers_batches_to_a_real_ingest()


def _run_network_none_denies_everything_and_the_internal_network_delivers_batches_to_a_real_ingest():
    run_id = uuid.uuid4().hex[:12]
    run_scratch = SCRATCH_ROOT / run_id
    run_scratch.mkdir(parents=True, exist_ok=True)
    guard = ResourceGuard(run_id)

    try:
        # -----------------------------------------------------------------------------------
        # Part 1 -- the deny-all proof: the REAL, unmodified av-edge-plugin:local image, run
        # with a bare `--network none` (no bridge, no shared namespace -- only `lo`), attempts
        # its normal connect flow and fails immediately, typed, never a hang.
        #
        # The mTLS path (`--endpoint https://...`), not the plaintext one, is used here on
        # purpose: `connect_plaintext` refuses any non-loopback address BEFORE dialing at all
        # (a client-side policy check, not a network fact -- see this module's own doc), which
        # would prove nothing about `--network none` itself. `av_grpc::tls::connect` builds its
        # OpenSSL context (reading --server-ca) BEFORE attempting the TCP connect, so a
        # syntactically valid throwaway CA is required for the flow to ever reach the network
        # attempt -- it is never used to complete a handshake (the connection never gets that
        # far), only to get past that one eager parse step.
        # -----------------------------------------------------------------------------------
        throwaway_key = run_scratch / "throwaway_ca.key"
        throwaway_ca = run_scratch / "throwaway_ca.pem"
        _run_local_openssl_self_signed_ca(throwaway_key, throwaway_ca)

        deny_all_container = _labelled_id(run_id, "deny-all")
        guard.track_container(deny_all_container)
        start = time.monotonic()
        deny_all = subprocess.run(
            [
                "docker", "run", "--rm", "--network", "none", "--name", deny_all_container, *guard.label_args(),
                "-v", f"{SIGNING_KEY_PEM}:/keys/signing.pem:ro",
                "-v", f"{throwaway_ca}:/keys/ca.pem:ro",
                IMAGE_TAG,
                "--signing-key", "/keys/signing.pem",
                "--endpoint", "https://198.51.100.1:9443",  # RFC 5737 TEST-NET-2: reserved, never routed, needs no server
                "--server-ca", "/keys/ca.pem",
            ],
            capture_output=True, text=True, timeout=30,
        )
        elapsed_s = time.monotonic() - start

        _skip_if_image_vanished_mid_run("part 1, deny-all run", deny_all)
        assert deny_all.returncode != 0, f"a container with --network none must not be able to reach anywhere non-loopback, but av-edge-plugin exited 0:\n{deny_all.stdout}"
        assert elapsed_s < 10.0, f"the connect attempt under --network none took {elapsed_s:.3f}s -- a real network-unreachable failure must be immediate, not a hang (it took {elapsed_s:.1f}s, suspiciously close to a TCP connect timeout)"
        assert "connecting to" in deny_all.stderr, f"expected av-edge-plugin's own connect-failure message, got:\n--- stdout ---\n{deny_all.stdout}\n--- stderr ---\n{deny_all.stderr}"

        # Independent, OS-level corroboration of *why*: a raw socket connect from the same
        # kind of --network-none container, no TLS/gRPC involved at all, against real (but
        # unrelated, public) addresses -- this is what "typed" means concretely: not just a
        # non-zero exit, a specific errno.
        raw_probe_container = _labelled_id(run_id, "deny-all-raw-probe")
        guard.track_container(raw_probe_container)
        raw = _docker(
            "run", "--rm", "--network", "none", "--name", raw_probe_container, *guard.label_args(),
            PROBE_IMAGE, "python3", "-c", _RAW_CONNECT_PROBE_SCRIPT,
        )
        raw_result = json.loads(raw.stdout.strip().splitlines()[-1])
        for host, port, outcome in raw_result:
            assert outcome["failed"], f"a raw connect to {host}:{port} should fail under --network none, got: {outcome}"
            assert outcome["elapsed_s"] < 1.0, f"connect to {host}:{port} under --network none took {outcome['elapsed_s']:.3f}s -- should be immediate (no route exists to even attempt)"
            assert "unreachable" in outcome["message"].lower() or outcome.get("errno") == 101, f"expected an ENETUNREACH-shaped failure for {host}:{port}, got: {outcome}"

        # Question 148: an exit code is not evidence. Print what was actually observed, so a
        # reader of the gate output can see this ran against a real container rather than
        # inferring it from a green dot.
        print(f"\n--- deny-all proof (--network none), observed ---\n"
              f"av-edge-plugin exit={deny_all.returncode} after {elapsed_s:.3f}s; stderr: {deny_all.stderr.strip()[:300]}\n"
              f"raw socket probe: {json.dumps(raw_result)}")

        # -----------------------------------------------------------------------------------
        # Part 2 -- the allowed-endpoint proof. A labelled, `--internal` bridge network (no
        # default route out at all -- proven, not merely named, in Part 3 below), carrying
        # only the ingest (the plugin joins its network namespace directly, so it is never
        # independently "on" this network at all -- see this module's own doc).
        # -----------------------------------------------------------------------------------
        network_name = _labelled_id(run_id, "net")
        _docker("network", "create", "--internal", *guard.label_args(), network_name)
        guard.track_network(network_name)

        ingest_bin = _cross_build_ingest_server_binary(run_scratch / "bin")

        ingest_container = _labelled_id(run_id, "ingest")
        guard.track_container(ingest_container)
        _docker(
            "run", "-d", "--name", ingest_container, "--network", network_name, *guard.label_args(),
            "-v", f"{ingest_bin}:/usr/local/bin/av-ingest-server:ro",
            "-v", f"{VERIFY_PUB_PEM}:/keys/verify.pub.pem:ro",
            "--entrypoint", "/usr/local/bin/av-ingest-server",
            IMAGE_TAG,  # reuses the plugin's own already-built, already-pulled runtime base (libssl3 + debian bookworm-slim) -- never a second image pull at test time (question 154)
            "--grpc-bind", "127.0.0.1:50070", "--admin-bind", "127.0.0.1:50071",
            "--log-dir", "/data/ingest-log",
            "--no-require-client-cert", "--verify-key", f"{PRODUCER_ID}:/keys/verify.pub.pem",
            "--clearance-ladder", CLEARANCE_LADDER, "--max-batch-age-ns", str(MAX_BATCH_AGE_NS),
            "--clock-tai-ns", str(CLOCK_TAI_NS),
        )
        _wait_for_container_stdout_lines(ingest_container, ("GRPC_LISTENING", "ADMIN_LISTENING"), timeout_s=15.0)

        plugin_container = _labelled_id(run_id, "plugin")
        guard.track_container(plugin_container)
        plugin_run = subprocess.run(
            [
                "docker", "run", "--rm", "--name", plugin_container, "--network", f"container:{ingest_container}", *guard.label_args(),
                "-v", f"{SIGNING_KEY_PEM}:/keys/signing.pem:ro",
                IMAGE_TAG,
                "--signing-key", "/keys/signing.pem",
                "--endpoint", "127.0.0.1:50070",
            ],
            capture_output=True, text=True, timeout=120,
        )
        _skip_if_image_vanished_mid_run("part 2, allowed-endpoint plugin run", plugin_run)
        assert plugin_run.returncode == 0, f"av-edge-plugin failed delivering to the real ingest:\n--- stdout ---\n{plugin_run.stdout}\n--- stderr ---\n{plugin_run.stderr}"
        summary = json.loads(plugin_run.stdout.strip().splitlines()[-1])
        assert summary["any_rejected"] is False, summary
        assert summary["batch_count"] == EXPECTED_BATCH_COUNT, summary
        assert summary["measurement_count"] == EXPECTED_BATCH_COUNT, summary
        assert summary["chain_head_hex"] == EXPECTED_CHAIN_HEAD_HEX, summary

        # Read the evidence surface back independently -- a second, disposable container
        # joined to the SAME shared namespace (never published to the Docker host at all;
        # nothing outside that namespace can reach it, exactly like the gRPC port).
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
        assert producer["accepted"] == EXPECTED_BATCH_COUNT, producer
        assert producer["chain_head"] == EXPECTED_CHAIN_HEAD_HEX, producer
        partition = evidence["partitions"][SHARD_KEY]
        assert partition["record_count"] == EXPECTED_BATCH_COUNT, partition

        # Question 148 again: the delivered numbers as observed, not as asserted.
        # `summary["verdicts"]` is one entry per batch (900 of them); printing it whole would
        # bury the numbers that matter in 90 KB of identical accepted lines, so the counts and
        # the head are printed and the verdicts summarised.
        print(f"\n--- allowed-endpoint proof, observed ---\n"
              f"plugin summary: batch_count={summary['batch_count']} "
              f"measurement_count={summary['measurement_count']} "
              f"any_rejected={summary['any_rejected']} "
              f"chain_head_hex={summary['chain_head_hex']} "
              f"verdicts={len(summary['verdicts'])} all-accepted="
              f"{all(v['accepted'] for v in summary['verdicts'])}\n"
              f"evidence read back from inside the shared namespace: accepted_total="
              f"{evidence['accepted_total']} rejected_total={evidence['rejected_total']} "
              f"producer chain_head={producer['chain_head']} partition record_count="
              f"{partition['record_count']}")

        # -----------------------------------------------------------------------------------
        # Part 3 -- "no route off this host": prove it, from inside the shared namespace, with
        # the identical raw-connect probe Part 1 already established the meaning of. Naming
        # real public addresses touches no actual network (question 154): --internal means no
        # default route exists to even attempt sending a packet, exactly as Part 1's own
        # --network none case, and the assertions below require the same immediacy.
        # -----------------------------------------------------------------------------------
        no_route_probe_container = _labelled_id(run_id, "no-route-probe")
        guard.track_container(no_route_probe_container)
        no_route_raw = _docker(
            "run", "--rm", "--name", no_route_probe_container, "--network", f"container:{ingest_container}", *guard.label_args(),
            PROBE_IMAGE, "python3", "-c", _RAW_CONNECT_PROBE_SCRIPT,
        )
        no_route_result = json.loads(no_route_raw.stdout.strip().splitlines()[-1])
        for host, port, outcome in no_route_result:
            assert outcome["failed"], f"the --internal network must have no route off this host, but reaching {host}:{port} did not fail: {outcome}"
            assert outcome["elapsed_s"] < 1.0, f"connect to {host}:{port} from inside the --internal network took {outcome['elapsed_s']:.3f}s -- should be immediate"
            assert "unreachable" in outcome["message"].lower() or outcome.get("errno") == 101, f"expected an ENETUNREACH-shaped failure for {host}:{port}, got: {outcome}"

        # And, for completeness, that the plugin's OWN endpoint really is unreachable from
        # OUTSIDE the shared namespace -- a third disposable container on the SAME --internal
        # network (a peer, not a namespace-sharer) cannot reach 127.0.0.1:50070 (that address
        # means something different in ITS OWN namespace) nor the ingest's published surface
        # (there is none -- no `-p` was ever used). This is the "nothing else is reachable
        # from there" half of the two-allowed-endpoints claim.
        outsider_container = _labelled_id(run_id, "outsider")
        guard.track_container(outsider_container)
        outsider = _docker(
            "run", "--rm", "--name", outsider_container, "--network", network_name, *guard.label_args(),
            PROBE_IMAGE, "python3", "-c",
            "import socket,time,json\n"
            "t0=time.monotonic()\n"
            "try:\n"
            "    s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.settimeout(2)\n"
            "    s.connect(('127.0.0.1', 50070))\n"
            "    print(json.dumps({'connected': True}))\n"
            "except Exception as e:\n"
            "    print(json.dumps({'connected': False, 'elapsed_s': time.monotonic()-t0, 'error': str(e)}))\n",
        )
        outsider_result = json.loads(outsider.stdout.strip().splitlines()[-1])
        assert outsider_result["connected"] is False, f"a peer container on the same bridge (not sharing the ingest's own namespace) must not reach 127.0.0.1:50070 (that address is its OWN loopback, not the ingest's): {outsider_result}"

    finally:
        guard.cleanup()
        shutil.rmtree(run_scratch, ignore_errors=True)

    # Question 156: the guard removed exactly what it created, and nothing labelled by this
    # run remains -- checked only after cleanup has actually run (success or failure above).
    guard.assert_nothing_left()


def _run_local_openssl_self_signed_ca(key_path: Path, cert_path: Path) -> None:
    """A throwaway, self-signed EC P-384 CA (`services/gmat-service`'s own nginx-front test
    fixtures use the identical recipe, e.g. `tests/test_edge_ingest_mtls.py::
    _build_front_server_cert`) -- used only to get `av_grpc::tls::connect`'s own eager
    `SslConnector::set_ca_file` past its parse step for the deny-all proof (that connection
    never reaches a TLS handshake at all -- see this module's own comment at its call site).
    Entirely local (`openssl req -x509` touches no network), so this is not a question-154
    violation despite running at test time."""
    openssl = "/opt/homebrew/opt/openssl@3/bin/openssl"
    subprocess.run([openssl, "ecparam", "-name", "secp384r1", "-genkey", "-noout", "-out", str(key_path)], capture_output=True, timeout=10, check=True)
    subprocess.run(
        [openssl, "req", "-x509", "-new", "-key", str(key_path), "-sha384", "-days", "2", "-subj", "/CN=e4b-throwaway-never-trusted", "-out", str(cert_path)],
        capture_output=True, timeout=10, check=True,
    )


# Printed as one JSON line by a disposable python:3.13-slim container -- deliberately a tiny,
# dependency-free stdlib script (no pip install, question 154) rather than a second committed
# file, since its only two call sites are right above.
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
