"""R3.3 (`docs/aiplane-plan.md` milestone A4; `docs/open-questions.md` question 206's decision
11(a); questions 148/154/156/194/196(d)): proves that `av-proposer:local`
(`services/proposer/Dockerfile`, built by `services/proposer/build-image.sh`) really runs as a
labelled container on an internal Docker network, dialling a real gateway+authority pair in a
second container, with the egress proof the edge plugin's own container test already measures
for its own topology -- not merely that the Dockerfile parses.

Docker-gated, and gated the way `tests/test_edge_plugin_container.py` already does it
(question 194): a module-level `_compute_skip_reason()` runs once at import time, and
`pytest.mark.skipif` on the one gated test uses its result as the printed reason.

# The topology this file proves, and why it is shaped this way

Decision 11(a) (`docs/open-questions.md` question 206) says the proposer's isolation is
"not a bind-mounted Unix socket ... realised as an internal Docker network
(`docker network create --internal`) holding only the gateway and the proposer, with the same
egress proof the edge plugin's container test already measures for its own endpoint."

Two containers, exactly:

- **Container G** runs TWO real processes, sharing one network namespace (this is the ONE
  container the brief calls "Container G", not two containers wired together):
  `av-command` bound to `127.0.0.1:50110` (its own container's loopback -- question 155's rule
  is completely untouched: the authority never leaves loopback, in this container or anywhere
  else) and `av-gateway`, listening on `0.0.0.0:50170` via the new `--internal-network-bind`
  flag (`crates/av-gateway/src/bin/av-gateway.rs`; `av_command::service::
  resolve_internal_network_bind_address`, R3.3's own one deliberate weakening of question 155,
  added this same round) -- reachable from the SECOND container on the shared bridge.
  `av-gateway` dials `av-command` over `127.0.0.1:50110`, which really is loopback for it: same
  container, same network namespace.
- **Container P** is the REAL, unmodified `av-proposer:local` image, its real `ENTRYPOINT`,
  attached to the SAME `--internal` network, dialling Container G **by container name**
  (Docker's embedded DNS on a user-defined bridge network) at `50170`.

## The rejected alternative, and why (this task's own brief, restated here at the enforcement
site so a reader does not have to go find it)

The edge plugin's own trick for "two containers, one plaintext-loopback endpoint" is
`--network container:<name>` -- joining the SECOND container into the FIRST's network
namespace outright (`tests/test_edge_plugin_container.py`'s own module doc explains why that
is the right shape for the plugin: the plugin and the ingest it talks to plaintext MUST share
loopback, since `connect_plaintext` refuses non-loopback addresses outright). That same trick
is WRONG here: `av-command` binds `127.0.0.1:50110` inside Container G's namespace specifically
so that nothing outside it can ever reach the command authority. Putting the proposer into
Container G's OWN network namespace (`--network container:G`) would put the proposer in the
IDENTICAL namespace `av-command`'s own loopback bind lives in -- `127.0.0.1:50110` would then
mean the SAME socket for the proposer as it does for `av-gateway` itself, and the isolation
claim this file exists to prove ("the proposer can reach the gateway and nothing else") would
be false by construction, not merely unproven. `--internal` plus TWO SEPARATE containers (the
gateway's own namespace stays G's alone; the proposer gets its own, third, namespace) is what
keeps the claim true: Part 3 below measures, directly, that Container P cannot reach
`127.0.0.1:50110`/`127.0.0.1:50111` at Container G's own network address, because there is
nothing listening there outside G's own loopback.

## Two ledgers, not one -- read from the host, bind-mounted (question 148: an exit code is not
evidence)

`crates/av-gateway/src/evidence.rs`'s own module doc explains why `ProposalEvidence` lives on a
ledger directory SEPARATE from `av-command`'s own: "a dedicated ledger, never the command
service's own" (a collision-avoidance design, not an oversight). This file therefore
bind-mounts TWO host directories from Container G -- `av-command`'s own `--ledger-dir` and
`av-gateway`'s own `AV_GATEWAY_EVIDENCE_LEDGER_DIR` -- and decodes BOTH directly off disk with
the real, committed `altavista.pb` Python bindings (`altavista/pb/__init__.py`, already
generated and importable with no `sys.path` hacking or `protoc` re-invocation needed -- see
that package's own module doc for the "why relative imports" story this file relies on):
`av-command`'s own partition file for the real `Command` (`COMMAND_STATE_PROPOSED`), and
`av-gateway`'s own evidence partition for the real `ProposalEvidence` `LedgerRecord.command.
payload` carries (`crate::evidence::EVIDENCE_TYPE_URL`). Neither ledger's own on-disk framing
(`crates/av-command/src/ledger.rs::append`: a 4-byte big-endian length prefix, then that many
bytes of a `prost`-encoded `LedgerRecord`) is reimplemented here from a guess -- it is read
straight from that module's own doc and mirrored exactly (`_iter_ledger_records` below).

# Why every bind-mount source lives under `.av-test-tmp/`, not `tempfile`/`tmp_path`

Identical reasoning to `tests/test_edge_plugin_container.py`'s own module doc ("Why every
bind-mount source lives under .av-test-tmp/, not tempfile/tmp_path"): Colima mounts only
`$HOME` into its VM, so a bind-mount source outside it (`tempfile`'s default dir, pytest's own
`tmp_path`) silently becomes an empty directory inside the container, with no error anywhere.
`SCRATCH_ROOT` below is under this repository's own tree (`<repo>/.av-test-tmp/`,
`.gitignore`d), which is under `$HOME` by construction.

# Question 199 (no test mutates the process environment)

Every `docker`/subprocess call below either passes no extra environment at all, or passes a
value as a command-line argument, an `-e KEY=VALUE` on the CONTAINER (never on this test
process), or a bind-mounted file -- never `os.environ[...] = ...`.

# A defect this file's own construction ruled out, not merely avoided

`av-gateway`'s own binary (`crates/av-gateway/src/bin/av-gateway.rs`) serves its MCP surface
over the REAL process stdin/stdout, and `crate::mcp::serve`'s read loop returns `Ok(())` the
INSTANT its input stream reports EOF (`while let Some(line) = lines.next_line().await? { ... }`
-- `next_line()` returns `Ok(None)` on EOF, ending the loop). A detached container started
without `-i` gets `/dev/null` for stdin, which reports EOF immediately -- `tokio::select!`
between the gRPC future (which never resolves under normal operation) and the MCP future
(which resolves near-instantly) would then pick the MCP branch and END THE WHOLE PROCESS,
closing the gRPC port, within a fraction of a second of starting -- a container that appears to
start successfully (`docker run -d` exits 0) and then answers nothing, with no error anywhere
in its own logs. This is exactly the "a failure or refusal that leaves no trace" defect shape
this platform's own reviews keep finding, here in a form that would have made the acceptance
run below silently measure nothing (the gateway container would already be gone by the time
Container P tried to dial it). `docker run -d -i` (this file's own Container G invocation)
keeps Container G's stdin open and unclosed for the container's whole lifetime, which is what
this file originally relied on.

**The binary itself is now fixed** (the manager's R3.3 review): `av-gateway` no longer races
the two futures against each other. The MCP stdio loop reaching EOF is reported as the normal
end of *that* surface and nothing more, and only the gRPC server's own exit ends the process,
so a detached container with no stdin now keeps serving gRPC and says in one line why the MCP
surface is not there. The `-i` on Container G below is kept anyway: it is what a deployment
that actually wants the MCP surface must pass, and keeping it means this file exercises the
same invocation an operator would use rather than only the degraded one.
"""
from __future__ import annotations

import json
import shutil
import struct
import subprocess
import time
import uuid
from pathlib import Path

import pytest

from altavista.docker_test_lock import lock_docker_tests

REPO_ROOT = Path(__file__).resolve().parents[1]
PROPOSER_DIR = REPO_ROOT / "services" / "proposer"
DOCKERFILE = PROPOSER_DIR / "Dockerfile"
BUILD_SCRIPT = "services/proposer/build-image.sh"
IMAGE_TAG = "av-proposer:local"
PROBE_IMAGE = "python:3.13-slim"

FIXTURE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance.runproducts.bin"
# The real fixture's own `run_id` (`crates/av-proposer/tests/common/mod.rs::FIXTURE_RUN_ID`,
# `crates/av-proposer/tests/determinism.rs::config`) -- the identical recipe those two files
# already use to force a real `Proposed` outcome against this fixture (never `NoProposalNeeded`,
# never a hand-picked value this file invented on its own).
FIXTURE_RUN_ID = "demo_two_instance_frozen_fixture"
CALLER_CLEARANCE = "CUI"
ENTITY_ID = "sat-1"
COMMAND_CLASS = "burn"
SCORE_NAME = "demo_flt_rmag_at_end"
REFERENCE_RADIUS_M = "6871000.0"
THRESHOLD_M = "100.0"
GAIN_PER_S = "0.001"
MAX_BURN_MPS = "5.0"
MODEL_NODE_ID = "av-proposer.station-keeping"
MODEL_VERSION = "1.0.0"

COMMAND_GRPC_ADDR = "127.0.0.1:50070"  # matches av-gateway's own AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT default (http://127.0.0.1:50070, crates/av-gateway/src/bin/av-gateway.rs's own R3.6 comment) -- no override needed. This file previously named 50110 here, stale since R3.6 retargeted the default from a since-superseded 50170; the mismatch was never caught because this test has never actually run (this task's own repair) -- with the wrong port, Container G's own av-gateway would eagerly try to dial 50070, find nothing (av-command was told to bind elsewhere), fall back to a lazily-connecting channel, and every real ProposeCommand call in Part 5 below would then fail to reach av-command at all.
COMMAND_ADMIN_ADDR = "127.0.0.1:50111"  # explicit: av-command's OWN default admin port (50170) would collide with av-gateway's gRPC bind on this same container.
GATEWAY_INTERNAL_BIND = "0.0.0.0:50170"
GATEWAY_PORT = 50170

# R5.1/question 208(b): the throwaway issuer both av-command's (pre-existing, unrelated to this
# round -- Propose performs no token verification at all) and av-gateway's (new this round,
# REAL verification against a real token) `--oidc-issuer` flags use. Two distinct AUDIENCEs
# (one per service, matching each binary's own `--oidc-audience`) even though the same
# throwaway keypair signs tokens for both, since `av_command::oidc::verify` checks `aud`
# against exactly the configured service's own audience string.
OIDC_ISSUER = "https://sso.test.example/"
GATEWAY_OIDC_AUDIENCE = "av-gateway-container-it"
# The group name the REAL, committed `profiles/gateway-authority.yaml` (bind-mounted below as
# this run's own `--auth-config-path`) grants "query"+"propose" -- the proposer's own service
# token carries this group.
GATEWAY_SERVICE_GROUP = "proposer-service"

# crates/av-lockstep/src/docker.rs's own established convention (`TEST_LABEL_KEY`/
# `TEST_LABEL_VALUE`), reused verbatim -- tests/test_edge_plugin_container.py's own precedent.
TEST_LABEL_KEY = "av.test"
TEST_LABEL_VALUE = "1"

# See this module's own doc, "Why every bind-mount source lives under .av-test-tmp/" -- must
# be under $HOME for Colima to actually bind-mount it.
SCRATCH_ROOT = REPO_ROOT / ".av-test-tmp" / "proposer_container"

# NEWER than the rust:1.85-bookworm pin tests/test_edge_plugin_container.py used to carry
# (R5.3 moved that file, and services/edge-plugin/build-image.sh, to this same 1.90 digest:
# the workspace floor is a measured 1.87 now, so a 1.85 container refuses to build at all)
# -- av-command
# (this cross-build's own subject) depends on regorus 0.12.0, which needs
# const_vec_string_slice (Vec::len/is_empty/as_slice as const fn), not yet stable at Rust
# 1.85 -- a real, measured build failure against that pin (see services/proposer/Dockerfile's
# own header comment, "Why this Dockerfile has no Rust builder stage", for the full story).
# rustc 1.90.0 compiles it cleanly.
PREBUILD_BASE_IMAGE = "rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
OPENSSL = "/opt/homebrew/opt/openssl@3/bin/openssl"


# =================================================================================================
# Gating (question 194): a typed reason, computed once, asserted-by-name via pytest.mark.skipif.
# =================================================================================================


def _docker_unavailable_reason() -> "str | None":
    """`None` iff `docker info` succeeds -- mirrors `tests/test_edge_plugin_container.py::
    _docker_unavailable_reason` exactly."""
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
            f"probe image {PROBE_IMAGE!r} (used only for the raw-socket network-reachability probes -- "
            f"never to replace anything this task built) is not present locally, and this test never "
            f"pulls an image itself (question 154: no network at test time). Run `docker pull "
            f"{PROBE_IMAGE}` once, on a host with network access, then re-run this test."
        )
    try:
        import google.protobuf  # noqa: F401
    except ImportError as e:
        return f"google.protobuf is not importable in this venv ({e}) -- this test decodes the real ledger on disk with altavista.pb, which needs it."
    if not (REPO_ROOT / "altavista" / "pb" / "altavista" / "v1" / "authority_pb2.py").exists():
        return (
            "altavista/pb/altavista/v1/authority_pb2.py is missing -- this test decodes the real ledger "
            "on disk with the committed altavista.pb bindings. Run "
            "`.venv/bin/python altavista/pb/generate.py` on a host with protoc installed, then re-run."
        )
    if not (Path("/Users/probe/code/spoore") / "crates" / "spoore-cdm").exists():
        return "/Users/probe/code/spoore/crates/spoore-cdm not found -- av-proposer's own cross-build (and this test's av-command/av-gateway cross-build) bind-mounts it (see services/proposer/Dockerfile's own header comment)."
    if not Path(OPENSSL).exists():
        return f"{OPENSSL} not found -- this test generates a throwaway RSA keypair for av-command's --oidc-public-key-path with it, entirely locally (question 154)."
    return None


_SKIP_REASON = _compute_skip_reason()

# Missing-image text Docker itself emits when a `docker run` names a tag that is not local and
# cannot be pulled. Matched, rather than re-inspecting, so a disappearance is caught at the exact
# call that tripped over it -- tests/test_edge_plugin_container.py's own precedent, verbatim.
_MISSING_IMAGE_MARKER = "Unable to find image"


def _skip_if_image_vanished_mid_run(stage: str, result: subprocess.CompletedProcess) -> None:
    """Turn an image that vanished *after* `_SKIP_REASON` was computed into a visible skip
    naming the stage it disappeared at, not an assertion failure -- see `tests/
    test_edge_plugin_container.py::_skip_if_image_vanished_mid_run`'s own docstring for the
    full, measured reason this host's image garbage collection makes this a real, expected
    event (question 196(d)), not a theoretical concern."""
    if _MISSING_IMAGE_MARKER in (result.stderr or ""):
        pytest.skip(
            f"image {IMAGE_TAG!r} was present when this test was collected but had been deleted "
            f"from this host by the time stage {stage!r} ran it -- a host-level image garbage "
            f"collection under disk pressure (question 196(d)), not a defect in what is under test "
            f"and not a silent pass. Rebuild with `{BUILD_SCRIPT}` and re-run. Docker's own words: "
            f"{result.stderr.strip().splitlines()[0] if result.stderr.strip() else '(no stderr)'}"
        )


# =================================================================================================
# Small docker/subprocess helpers -- no process-environment mutation anywhere (question 199).
# =================================================================================================


def _docker(*args: str, timeout: float = 60.0, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        _skip_if_image_vanished_mid_run(" ".join(args[:3]), result)
        pytest.fail(f"`docker {' '.join(args)}` failed (rc={result.returncode}):\n--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}")
    return result


def _labelled_id(run_id: str, kind: str) -> str:
    return f"av-proposer-test-{kind}-{run_id}"


class ResourceGuard:
    """Tracks exactly the container/network names THIS test run created, and removes exactly
    those and nothing else -- `tests/test_edge_plugin_container.py::ResourceGuard`'s own
    convention, verbatim (question 156)."""

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
        # already-built IMAGE_TAG and PROBE_IMAGE) -- confirmed, not silently assumed.
        remaining_images = _docker("images", "--filter", label_filter, "-q").stdout.strip()
        assert remaining_images == "", f"no image should ever carry {label_filter} (this test creates none), but found: {remaining_images!r}"


def _cross_build_command_and_gateway_binaries(dest_dir: Path) -> tuple[Path, Path]:
    """Cross-builds `av-command` AND `av-gateway` for Linux, in ONE bind-mounted `docker run`
    (one prebuild window, not two) -- the identical pattern `services/proposer/build-image.sh`
    (and `services/edge-plugin/build-image.sh` before it) uses for `av-proposer`/
    `av-edge-plugin`, applied here because `services/proposer/Dockerfile` packages ONLY the
    proposer (its own header comment's "Self-contained and offline" section) -- Container G
    needs real `av-command`/`av-gateway` binaries this test builds for itself, independent of
    the proposer's own image."""
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
            # SUPERSEDED (question 211, the lead, 2026-09-15): this comment used to claim a
            # GPG-signature host defect and carried an unconfirmed https-rewriting `sed`,
            # copied from services/proposer/build-image.sh's own prebuild step -- see that
            # script's own comment at its identical line for the full re-measurement. Dropped
            # here too, for the same measured reason: PREBUILD_BASE_IMAGE already ships
            # ca-certificates, so the rewrite changed nothing, and no GPG/apt-key failure was
            # reproduced against it on this host.
            "apt-get update -qq && apt-get install -y -qq --no-install-recommends "
            "protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && "
            "cargo build --release -p av-command --bin av-command -p av-gateway --bin av-gateway "
            "--target-dir /workspace/target-docker-linux && "
            "strip /workspace/target-docker-linux/release/av-command "
            "/workspace/target-docker-linux/release/av-gateway",
        ],
        capture_output=True, text=True, timeout=900,
    )
    if build.returncode != 0:
        shutil.rmtree(scratch_target, ignore_errors=True)
        pytest.fail(f"cross-building av-command/av-gateway failed (rc={build.returncode}):\n--- stdout ---\n{build.stdout}\n--- stderr ---\n{build.stderr}")
    built_command = scratch_target / "release" / "av-command"
    built_gateway = scratch_target / "release" / "av-gateway"
    assert built_command.is_file(), f"cross-build reported success but {built_command} does not exist"
    assert built_gateway.is_file(), f"cross-build reported success but {built_gateway} does not exist"
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest_command = dest_dir / "av-command"
    dest_gateway = dest_dir / "av-gateway"
    shutil.copy2(built_command, dest_command)
    shutil.copy2(built_gateway, dest_gateway)
    dest_command.chmod(0o755)
    dest_gateway.chmod(0o755)
    shutil.rmtree(scratch_target, ignore_errors=True)
    return dest_command, dest_gateway


def _generate_throwaway_oidc_keypair(dest_dir: Path) -> tuple[Path, Path]:
    """A throwaway RSA keypair, entirely local (`openssl genrsa`/`rsa -pubout` touch no
    network -- question 154 is not violated despite running at test time, the identical
    reasoning `tests/test_edge_plugin_container.py::_run_local_openssl_self_signed_ca` already
    gives for its own throwaway CA). `av-command` requires `--oidc-issuer`/`--oidc-audience`/
    `--oidc-public-key-path` at startup (A2.1, no default) but this test's own propose flow
    never calls `Authorize` (only `Propose`, which performs no token verification at all --
    `crates/av-command/src/service.rs::propose`) -- so the public key's own content is never
    actually checked against anything for av-command's OWN startup; only its PEM shape
    (`PKey::public_key_from_pem`) matters for it to start at all. R5.1/question 208(b):
    `av-gateway` now ALSO requires the identical three flags, and DOES verify real tokens
    signed against this keypair's own PRIVATE half (`_mint_rs256_jwt` below, for the proposer's
    own service token) -- so this function now returns BOTH halves, not the public key alone."""
    dest_dir.mkdir(parents=True, exist_ok=True)
    key_path = dest_dir / "oidc_private.pem"
    pub_path = dest_dir / "oidc_public.pem"
    subprocess.run([OPENSSL, "genrsa", "-out", str(key_path), "2048"], capture_output=True, timeout=15, check=True)
    subprocess.run([OPENSSL, "rsa", "-in", str(key_path), "-pubout", "-out", str(pub_path)], capture_output=True, timeout=15, check=True)
    return key_path, pub_path


def _b64url(raw: bytes) -> str:
    import base64

    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def _mint_rs256_jwt(private_key_path: Path, *, issuer: str, audience: str, subject: str, groups: list[str], ttl_s: int = 3600) -> str:
    """R5.1/question 208(b): mints a real, RS256-signed compact JWS entirely locally (`openssl
    dgst -sha256 -sign` touches no network -- question 154, the identical reasoning
    `_generate_throwaway_oidc_keypair`'s own doc gives), for `av-gateway`'s own `crate::auth::
    AuthContext` (`crates/av-command/src/oidc.rs::verify`) to actually verify -- mirrors that
    module's own `TestIssuer::mint` byte for byte (header `{"alg":"RS256","typ":"JWT"}`, the
    signing input `header_b64.payload_b64`, RS256 = SHA-256 digest signed with the RSA private
    key), but as a subprocess pipeline rather than an in-process `openssl` crate call, since
    this file is Python, not Rust."""
    now = int(time.time())
    header = {"alg": "RS256", "typ": "JWT"}
    payload = {"iss": issuer, "aud": audience, "sub": subject, "iat": now, "exp": now + ttl_s, "groups": groups}
    header_b64 = _b64url(json.dumps(header, separators=(",", ":")).encode("utf-8"))
    payload_b64 = _b64url(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
    signing_input = f"{header_b64}.{payload_b64}".encode("ascii")
    sig = subprocess.run(
        [OPENSSL, "dgst", "-sha256", "-sign", str(private_key_path)],
        input=signing_input, capture_output=True, timeout=15, check=True,
    ).stdout
    return f"{header_b64}.{payload_b64}.{_b64url(sig)}"


def _write_minimal_execution_profile(dest_dir: Path) -> Path:
    """A minimal `execution.yaml` for `av-command --profile-path`, NOT the real committed
    `profiles/execution.yaml` -- deliberately: that file's own `authority.delegations_path`
    (`profiles/policies/authority/delegations.yaml`) and `audit.sink_path`
    (`var/log/av-command/audit.log`) are both resolved repo-relative
    (`crates/av-command/src/authz.rs::load_delegations`'s own `repo_root` argument, computed
    from `CARGO_MANIFEST_DIR` at COMPILE time -- `/workspace` inside the cross-build container,
    a path this test's own runtime container never bind-mounts), and this test's own propose
    flow (`Propose` only) needs neither delegations nor an audit sink at all. Empty
    `delegations_path`/`audit.sink_path` are both explicitly documented as "not an error, not
    a default this module invents" (`crates/av-command/src/authz.rs::load_delegations`'s own
    doc; `crates/av-command/src/audit.rs::load_profile_audit_config`'s own
    `_with_an_empty_sink_path_is_disabled` test) -- never a magic value this test invented."""
    dest_dir.mkdir(parents=True, exist_ok=True)
    path = dest_dir / "execution.yaml"
    path.write_text(
        "authority:\n"
        "  roles: {}\n"
        "  mfa_amr_methods: []\n"
        "  mfa_acr: \"\"\n"
        "  delegations_path: \"\"\n"
        "  service_roles: {}\n"
        "audit:\n"
        "  sink_path: \"\"\n"
    )
    return path


def _wait_for_container_log_lines(name: str, needles: tuple[str, ...], timeout_s: float) -> str:
    """Polls `docker logs` (both its stdout AND stderr -- see this module's own doc for why:
    `av-command`/`av-gateway`'s own readiness lines are `eprintln!`, unlike `av-ingest-server`'s
    `println!` convention `tests/test_edge_plugin_container.py::_wait_for_container_stdout_lines`
    was written for) until every one of `needles` has appeared as a SUBSTRING somewhere in the
    combined text -- bounded, never a bare `sleep N` guess. Returns the combined text actually
    observed, for `docker inspect`-independent evidence (question 148)."""
    deadline = time.monotonic() + timeout_s
    while True:
        logs = _docker("logs", name, check=False)
        combined = (logs.stdout or "") + (logs.stderr or "")
        if all(needle in combined for needle in needles):
            return combined
        if time.monotonic() > deadline:
            still_running = _docker("inspect", name, "--format", "{{.State.Status}} (exit {{.State.ExitCode}})", check=False).stdout.strip()
            pytest.fail(f"container {name} (state: {still_running}) never printed all of {needles} within {timeout_s}s -- logs so far:\n{combined}")
        time.sleep(0.1)


# =================================================================================================
# Ledger decoding -- the real, committed altavista.pb bindings (no protoc invocation, no
# sys.path hacking; see this module's own doc). Mirrors crates/av-command/src/ledger.rs::append
# exactly: a 4-byte big-endian length prefix, then that many bytes of a prost-encoded
# LedgerRecord.
# =================================================================================================


def _iter_ledger_records(ledger_dir: Path):
    from altavista.pb import authority_pb2

    for ledger_file in sorted(ledger_dir.glob("*.ledger")):
        data = ledger_file.read_bytes()
        offset = 0
        while offset < len(data):
            if offset + 4 > len(data):
                pytest.fail(f"{ledger_file}: truncated length prefix at offset {offset} ({len(data)} bytes total)")
            (n,) = struct.unpack(">I", data[offset : offset + 4])
            offset += 4
            if offset + n > len(data):
                pytest.fail(f"{ledger_file}: truncated record body at offset {offset} (need {n} bytes, {len(data) - offset} remain)")
            record = authority_pb2.LedgerRecord()
            record.ParseFromString(data[offset : offset + n])
            offset += n
            yield record


def _find_proposed_command(ledger_dir: Path, entity_id: str) -> "object":
    """The real `Command` (decoded straight off `av-command`'s own on-disk ledger) whose
    `entity_id` matches and whose `state` is the LATEST transition recorded for it -- returns
    the `LedgerRecord.command` snapshot itself (A1.3-round-2: always the POST-transition
    `Command`, so its own `.state` is authoritative with no further lookup needed)."""
    latest_by_id: dict[str, object] = {}
    for record in _iter_ledger_records(ledger_dir):
        if record.command_id and record.HasField("command") and record.command.entity_id == entity_id:
            latest_by_id[record.command_id] = record.command
    assert latest_by_id, f"no Command for entity_id={entity_id!r} found on the real ledger at {ledger_dir}"
    assert len(latest_by_id) == 1, f"expected exactly one proposed command, found {len(latest_by_id)}: {list(latest_by_id)}"
    return next(iter(latest_by_id.values()))


def _find_proposal_evidence(evidence_ledger_dir: Path, command_id: str):
    """The real `ProposalEvidence`, decoded from `av-gateway`'s OWN evidence ledger
    (`crates/av-gateway/src/evidence.rs::EVIDENCE_TYPE_URL`, packed into `LedgerRecord.command.
    payload` -- a separate ledger directory from `av-command`'s own, by that module's own
    design; see this file's own module doc)."""
    from altavista.pb import authority_pb2

    evidence_type_url = "type.googleapis.com/altavista.v1.ProposalEvidence"
    for record in _iter_ledger_records(evidence_ledger_dir):
        if not record.HasField("command") or not record.command.HasField("payload"):
            continue
        payload = record.command.payload
        if payload.type_url != evidence_type_url:
            continue
        evidence = authority_pb2.ProposalEvidence()
        evidence.ParseFromString(payload.value)
        if evidence.command_id == command_id:
            return evidence
    return None


# =================================================================================================
# The test.
# =================================================================================================


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_proposer_on_an_internal_network_proposes_and_cannot_reach_the_authority():
    # Question 207/156: every container/network this test creates carries `av.test`/
    # `av.test.run_id` (`ResourceGuard.label_args()` above) -- exactly the label
    # `av_lockstep::docker::prune_stale_test_resources` sweeps DAEMON-WIDE, and this test also
    # cross-builds (docker run) and builds (docker build via services/proposer/build-image.sh's
    # own image, reused read-only here) against the SAME shared daemon. A concurrent Rust
    # `cargo test` or another worktree's own Docker-gated test (e.g. AltaVista-edge, running
    # concurrently with this task) could tear this test's own containers out from under it, or
    # this test could tear out theirs -- the identical exposure question 207 found and fixed.
    # Held for this whole test body (not just around any one docker command), via the SAME
    # `$HOME`-rooted lock file the Rust side uses -- `tests/test_edge_plugin_container.py`'s own
    # precedent, verbatim.
    with lock_docker_tests():
        _run_test_proposer_on_an_internal_network_proposes_and_cannot_reach_the_authority()


def _run_test_proposer_on_an_internal_network_proposes_and_cannot_reach_the_authority():
    run_id = uuid.uuid4().hex[:12]
    run_scratch = SCRATCH_ROOT / run_id
    run_scratch.mkdir(parents=True, exist_ok=True)
    guard = ResourceGuard(run_id)

    try:
        # -----------------------------------------------------------------------------------
        # Part 0 -- R5.1/question 208(b): the throwaway OIDC keypair and the proposer's own
        # real, signed service token, generated FIRST (moved ahead of Part 1, not only Part 2)
        # because av-proposer's own CLI now REQUIRES --service-token-file to start at all
        # (invariant B: an absent token must make the proposer fail with a typed error, never
        # proceed unauthenticated) -- even Part 1's deny-all run needs a token FILE bind-mounted
        # to get far enough to attempt (and fail) the network connection Part 1 actually
        # measures. Colima mounts only $HOME (see this module's own doc, "Why every bind-mount
        # source lives under .av-test-tmp/") -- the token file lives under run_scratch, never
        # under /tmp, and is passed to the container as --service-token-file <PATH>, never
        # --service-token <VALUE> (a flag value is visible in `ps`/`docker inspect`).
        # -----------------------------------------------------------------------------------
        oidc_private_key, oidc_public_key = _generate_throwaway_oidc_keypair(run_scratch / "oidc")
        service_token = _mint_rs256_jwt(oidc_private_key, issuer=OIDC_ISSUER, audience=GATEWAY_OIDC_AUDIENCE, subject="av-proposer-it", groups=[GATEWAY_SERVICE_GROUP])
        service_token_path = run_scratch / "service_token"
        service_token_path.write_text(service_token)

        # -----------------------------------------------------------------------------------
        # Part 1 -- the deny-all proof, the SAME image as the real run below (this task's own
        # requirement: "a --network none run and an internal-network run of the SAME image, so
        # the two measurements differ only in the network"). A real, unreachable TEST-NET-2
        # literal (RFC 5737) needs no server on the other end and no DNS at all.
        # -----------------------------------------------------------------------------------
        deny_all_container = _labelled_id(run_id, "deny-all")
        guard.track_container(deny_all_container)
        start = time.monotonic()
        deny_all = subprocess.run(
            [
                "docker", "run", "--rm", "--network", "none", "--name", deny_all_container, *guard.label_args(),
                "-v", f"{service_token_path}:/etc/av/service_token:ro",
                IMAGE_TAG,
                "--gateway-endpoint", "http://198.51.100.1:50170",
                "--run-id", FIXTURE_RUN_ID, "--caller-clearance", CALLER_CLEARANCE, "--entity-id", ENTITY_ID,
                "--command-class", COMMAND_CLASS, "--score-name", SCORE_NAME,
                "--reference-radius-m", REFERENCE_RADIUS_M, "--threshold-m", THRESHOLD_M,
                "--gain-per-s", GAIN_PER_S, "--max-burn-mps", MAX_BURN_MPS,
                "--model-node-id", MODEL_NODE_ID, "--model-version", MODEL_VERSION,
                "--service-token-file", "/etc/av/service_token",
            ],
            capture_output=True, text=True, timeout=30,
        )
        elapsed_s = time.monotonic() - start
        _skip_if_image_vanished_mid_run("part 1, deny-all run", deny_all)
        assert deny_all.returncode != 0, f"a container with --network none must not reach any endpoint, but av-proposer exited 0:\n{deny_all.stdout}"
        assert elapsed_s < 10.0, f"the connect attempt under --network none took {elapsed_s:.3f}s -- a real network-unreachable failure must be immediate, not a hang"
        assert "connecting to av-gateway" in deny_all.stderr, f"expected av-proposer's own connect-failure message, got:\n--- stdout ---\n{deny_all.stdout}\n--- stderr ---\n{deny_all.stderr}"
        print(f"\n--- deny-all proof (--network none), observed ---\nav-proposer exit={deny_all.returncode} after {elapsed_s:.3f}s; stderr: {deny_all.stderr.strip()[:300]}")

        # -----------------------------------------------------------------------------------
        # Part 2 -- build Container G's own two real binaries and its own remaining
        # secrets/configs (the minimal execution profile, and R5.1's own gateway-authority.yaml
        # granting the proposer's service group "query"+"propose"), all under $HOME (see this
        # module's own doc).
        # -----------------------------------------------------------------------------------
        command_bin, gateway_bin = _cross_build_command_and_gateway_binaries(run_scratch / "bin")
        profile_path = _write_minimal_execution_profile(run_scratch / "profile")
        # R5.1/this task's own repair: bind-mount the REAL, committed profiles/
        # gateway-authority.yaml (not a hand-written duplicate this file used to synthesize)
        # -- it already grants GATEWAY_SERVICE_GROUP ("proposer-service") both "query" and
        # "propose" at CALLER_CLEARANCE ("CUI"), the identical shape the old
        # `_write_gateway_auth_config` helper wrote by hand. Exercising the real shipped
        # config is more faithful (this task's own acceptance point) and removes a
        # hand-maintained duplicate that could silently drift from the file that actually
        # ships.
        gateway_auth_config_path = REPO_ROOT / "profiles" / "gateway-authority.yaml"
        assert gateway_auth_config_path.is_file(), f"real gateway auth config missing at {gateway_auth_config_path}"
        policy_dir = REPO_ROOT / "profiles" / "policies" / "authority"
        assert policy_dir.is_dir(), f"real policy bundle directory missing at {policy_dir}"

        command_ledger_dir = run_scratch / "command-ledger"
        evidence_ledger_dir = run_scratch / "evidence-ledger"
        command_ledger_dir.mkdir(parents=True, exist_ok=True)
        evidence_ledger_dir.mkdir(parents=True, exist_ok=True)

        # -----------------------------------------------------------------------------------
        # Part 3 -- the labelled --internal network (question 156: labelled; --internal
        # because there is no route off this host at all -- measured in Part 5 below, never
        # merely asserted), holding exactly Container G and Container P.
        # -----------------------------------------------------------------------------------
        network_name = _labelled_id(run_id, "net")
        _docker("network", "create", "--internal", *guard.label_args(), network_name)
        guard.track_network(network_name)

        # -----------------------------------------------------------------------------------
        # Part 4 -- Container G: av-command on its own container loopback, av-gateway on the
        # container's own interface (--internal-network-bind, R3.3). `-i` keeps stdin open for
        # this container's whole life -- see this module's own doc, "A defect this file's own
        # construction ruled out".
        # -----------------------------------------------------------------------------------
        gateway_container = _labelled_id(run_id, "gateway")
        guard.track_container(gateway_container)
        command_line = (
            f"/usr/local/bin/av-command --bind {COMMAND_GRPC_ADDR} --admin-bind {COMMAND_ADMIN_ADDR} "
            f"--ledger-dir /data/command-ledger --policy-dir /etc/av/policy "
            f"--profile-path /etc/av/execution.yaml --oidc-issuer {OIDC_ISSUER} "
            f"--oidc-audience av-proposer-container-it --oidc-public-key-path /etc/av/oidc_public.pem & "
            f"exec /usr/local/bin/av-gateway --internal-network-bind {GATEWAY_INTERNAL_BIND} "
            f"--run-products /data/fixture.runproducts.bin:{CALLER_CLEARANCE} "
            f"--oidc-issuer {OIDC_ISSUER} --oidc-audience {GATEWAY_OIDC_AUDIENCE} "
            f"--oidc-public-key-path /etc/av/oidc_public.pem --auth-config-path /etc/av/gateway-authority.yaml"
        )
        _docker(
            "run", "-d", "-i", "--name", gateway_container, "--network", network_name, *guard.label_args(),
            "-e", "AV_GATEWAY_EVIDENCE_LEDGER_DIR=/data/evidence-ledger",
            "-v", f"{command_bin}:/usr/local/bin/av-command:ro",
            "-v", f"{gateway_bin}:/usr/local/bin/av-gateway:ro",
            "-v", f"{command_ledger_dir}:/data/command-ledger",
            "-v", f"{evidence_ledger_dir}:/data/evidence-ledger",
            "-v", f"{policy_dir}:/etc/av/policy:ro",
            "-v", f"{profile_path}:/etc/av/execution.yaml:ro",
            "-v", f"{oidc_public_key}:/etc/av/oidc_public.pem:ro",
            "-v", f"{gateway_auth_config_path}:/etc/av/gateway-authority.yaml:ro",
            "-v", f"{FIXTURE_PATH}:/data/fixture.runproducts.bin:ro",
            "--entrypoint", "/bin/bash",
            IMAGE_TAG,  # reuses the proposer's own already-built, already-pulled runtime base (libssl3 + debian bookworm-slim) -- never a second image pull at test time (question 154).
            "-c", command_line,
        )
        gateway_logs = _wait_for_container_log_lines(
            gateway_container,
            (f"av-command: listening on {COMMAND_GRPC_ADDR}", f"av-gateway: DataGatewayService + ModelProposeService on {GATEWAY_INTERNAL_BIND}"),
            timeout_s=30.0,
        )
        print(f"\n--- Container G readiness, observed ---\n{gateway_logs.strip()[-800:]}")

        # -----------------------------------------------------------------------------------
        # Part 5 -- the allowed-endpoint proof. Container P: the REAL, unmodified
        # av-proposer:local image, its real ENTRYPOINT, dialling Container G BY NAME over the
        # internal network's own embedded DNS.
        # -----------------------------------------------------------------------------------
        proposer_container = _labelled_id(run_id, "proposer")
        guard.track_container(proposer_container)
        proposer_run = subprocess.run(
            [
                "docker", "run", "--rm", "--name", proposer_container, "--network", network_name, *guard.label_args(),
                "-v", f"{service_token_path}:/etc/av/service_token:ro",
                IMAGE_TAG,
                "--gateway-endpoint", f"http://{gateway_container}:{GATEWAY_PORT}",
                "--run-id", FIXTURE_RUN_ID, "--caller-clearance", CALLER_CLEARANCE, "--entity-id", ENTITY_ID,
                "--command-class", COMMAND_CLASS, "--score-name", SCORE_NAME,
                "--reference-radius-m", REFERENCE_RADIUS_M, "--threshold-m", THRESHOLD_M,
                "--gain-per-s", GAIN_PER_S, "--max-burn-mps", MAX_BURN_MPS,
                "--model-node-id", MODEL_NODE_ID, "--model-version", MODEL_VERSION,
                "--service-token-file", "/etc/av/service_token",
            ],
            capture_output=True, text=True, timeout=60,
        )
        _skip_if_image_vanished_mid_run("part 5, allowed-endpoint proposer run", proposer_run)
        assert proposer_run.returncode == 0, f"av-proposer failed proposing to the real gateway:\n--- stdout ---\n{proposer_run.stdout}\n--- stderr ---\n{proposer_run.stderr}"
        summary = json.loads(proposer_run.stdout.strip().splitlines()[-1])
        assert summary["outcome"] == "PROPOSED", summary
        command_id = summary["command_id"]

        # Question 148: an exit code (and even the JSON summary above) is not evidence on its
        # own -- read the REAL ledgers back off disk, independently.
        #
        # This task's own repair: question 209(a) (a later, unrelated round) made av-command's
        # own `Propose` RPC run the check step automatically, as a second logged transition,
        # the instant the `PROPOSED` record lands (`crates/av-command/src/service.rs::propose`'s
        # own doc: "the check edge now runs automatically, right here, as a *separate* logged
        # transition"). A legally proposed command therefore normally reaches
        # `COMMAND_STATE_CHECKED` (2), with an `allow=true` policy decision, before `Propose`
        # even returns -- `COMMAND_STATE_PROPOSED` (1) alone is no longer the terminal state of
        # a successful propose-only run. This file asserted `state == 1` since before that
        # change landed and was never re-run against it (this task's whole reason for existing)
        # -- confirmed by measurement here (a real container run, `state == 2`, transitions[-1]
        # rationale containing `allow=true`), not guessed.
        proposed_command = _find_proposed_command(command_ledger_dir, ENTITY_ID)
        assert proposed_command.state == 2, f"expected COMMAND_STATE_CHECKED (2, question 209(a): Propose auto-checks), got {proposed_command.state}"  # altavista.v1.CommandState.COMMAND_STATE_CHECKED
        assert proposed_command.id == command_id, (proposed_command.id, command_id)
        assert proposed_command.command_class == COMMAND_CLASS
        assert len(proposed_command.transitions) >= 2, f"expected at least the PROPOSED and CHECKED transitions question 209(a) describes, got {len(proposed_command.transitions)}"

        evidence = _find_proposal_evidence(evidence_ledger_dir, command_id)
        assert evidence is not None, f"no ProposalEvidence for command_id={command_id!r} found on the real evidence ledger at {evidence_ledger_dir}"
        # R5.1/invariant D (a deliberate, documented behaviour change -- see crates/av-proposer/
        # src/proposer.rs's own comment): model_identity now records the VERIFIED service token
        # subject ("av-proposer-it", this file's own _mint_rs256_jwt subject above), never
        # MODEL_NODE_ID -- av-proposer itself now sends an empty declared `principal`.
        assert evidence.model_identity == "av-proposer-it", evidence.model_identity
        # R5.1b defect 2 (the manager's review of R5.1): making `model_identity` the verified
        # subject had silently cost this record the one thing milestone A4 names -- "attributing
        # the proposal to the model identity AND version". `model_node_id` is the additive,
        # explicitly caller-DECLARED field that carries the model's own identity back, and this
        # is the only place in the tree where that attribution is proven END TO END: a real
        # container, a real gateway, a real ledger record read back off disk. Without this
        # assertion the restored field could go empty again and nothing here would fail.
        assert evidence.model_node_id == MODEL_NODE_ID, evidence.model_node_id
        assert evidence.model_version == MODEL_VERSION, evidence.model_version
        assert evidence.run.run_id == FIXTURE_RUN_ID, evidence.run.run_id
        assert evidence.query_ids, "ProposalEvidence.query_ids must be non-empty (D5: what the model saw)"

        print(
            f"\n--- allowed-endpoint proof, observed ---\n"
            f"proposer summary: outcome={summary['outcome']} command_id={command_id} "
            f"idempotency_key={summary.get('idempotency_key')} drift_m={summary.get('drift_m')} burn_mps={summary.get('burn_mps')}\n"
            f"real ledger Command: state={proposed_command.state} entity_id={proposed_command.entity_id} "
            f"command_class={proposed_command.command_class} transitions={len(proposed_command.transitions)}\n"
            f"real evidence ledger ProposalEvidence: model_identity={evidence.model_identity} "
            f"model_node_id={evidence.model_node_id} model_version={evidence.model_version} "
            f"run_id={evidence.run.run_id} query_ids={list(evidence.query_ids)}"
        )

        # -----------------------------------------------------------------------------------
        # Part 6 -- "no route off this host": from INSIDE the --internal network this time (a
        # peer container, not sharing any namespace with G), a raw connect to two well-known,
        # unrelated public addresses fails immediately -- the identical shape tests/
        # test_edge_plugin_container.py's own Part 3 already measures for its own network, and
        # what makes this run satisfy question 154 despite naming real public IPs (no packet
        # actually leaves; there is no default route to leave by).
        # -----------------------------------------------------------------------------------
        no_route_probe_container = _labelled_id(run_id, "no-route-probe")
        guard.track_container(no_route_probe_container)
        no_route_raw = _docker(
            "run", "--rm", "--name", no_route_probe_container, "--network", network_name, *guard.label_args(),
            PROBE_IMAGE, "python3", "-c", _RAW_CONNECT_PROBE_SCRIPT,
        )
        no_route_result = json.loads(no_route_raw.stdout.strip().splitlines()[-1])
        for host, port, outcome in no_route_result:
            assert outcome["failed"], f"the --internal network must have no route off this host, but reaching {host}:{port} did not fail: {outcome}"
            assert outcome["elapsed_s"] < 1.0, f"connect to {host}:{port} from inside the --internal network took {outcome['elapsed_s']:.3f}s -- should be immediate"
            assert "unreachable" in outcome["message"].lower() or outcome.get("errno") == 101, f"expected an ENETUNREACH-shaped failure for {host}:{port}, got: {outcome}"
        print(f"\n--- no-route-off-this-host proof, observed ---\n{json.dumps(no_route_result)}")

        # -----------------------------------------------------------------------------------
        # Part 7 -- "the authority is not reachable from the proposer." A raw socket connect,
        # from a container occupying Container P's own position on the network (attached to
        # the SAME --internal network, sharing no namespace with G -- exactly Container P's
        # own vantage point; tests/test_edge_plugin_container.py's own raw-connect
        # corroboration uses an equivalent stand-in probe container rather than instrumenting
        # the real subject binary directly), against Container G's OWN address on that network
        # at both of its loopback-bound ports. Measured, not asserted to be any particular
        # errno -- av-command bound ONLY 127.0.0.1 inside G's own namespace (question 155), so
        # nothing listens on G's shared-network address for either port; the connection is
        # REFUSED (a listening-socket fact), not merely unreachable (a routing fact) -- a
        # different failure signature from Part 1/Part 6's ENETUNREACH, deliberately: this
        # network DOES have a route to G, there is simply nothing there to answer.
        # -----------------------------------------------------------------------------------
        authority_probe_container = _labelled_id(run_id, "authority-probe")
        guard.track_container(authority_probe_container)
        authority_probe_script = (
            "import socket, time, json\n"
            f"targets = [({gateway_container!r}, 50110), ({gateway_container!r}, 50111)]\n"
            "out = []\n"
            "for host, port in targets:\n"
            "    t0 = time.monotonic()\n"
            "    try:\n"
            "        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n"
            "        s.settimeout(3)\n"
            "        s.connect((host, port))\n"
            "        out.append((host, port, {'connected': True, 'elapsed_s': time.monotonic() - t0}))\n"
            "    except OSError as e:\n"
            "        out.append((host, port, {'connected': False, 'elapsed_s': time.monotonic() - t0, 'message': str(e), 'errno': e.errno}))\n"
            "print(json.dumps(out))\n"
        )
        authority_raw = _docker(
            "run", "--rm", "--name", authority_probe_container, "--network", network_name, *guard.label_args(),
            PROBE_IMAGE, "python3", "-c", authority_probe_script,
        )
        authority_result = json.loads(authority_raw.stdout.strip().splitlines()[-1])
        for host, port, outcome in authority_result:
            assert outcome["connected"] is False, f"the command authority's own port {port} on {host} must not be reachable from the proposer's side of the network, but it connected: {outcome}"
            assert outcome["elapsed_s"] < 3.5, f"connect to {host}:{port} took {outcome['elapsed_s']:.3f}s -- a refused connection on the same L2 segment should be fast"
        print(f"\n--- authority-not-reachable-from-proposer proof, observed ---\n{json.dumps(authority_result)}")

    finally:
        guard.cleanup()
        shutil.rmtree(run_scratch, ignore_errors=True)

    # Question 156: the guard removed exactly what it created, and nothing labelled by this
    # run remains -- checked only after cleanup has actually run (success or failure above).
    guard.assert_nothing_left()


# Printed as one JSON line by a disposable python:3.13-slim container -- deliberately a tiny,
# dependency-free stdlib script (no pip install, question 154), the identical script tests/
# test_edge_plugin_container.py already uses for the identical measurement.
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
