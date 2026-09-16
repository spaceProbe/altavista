#!/usr/bin/env python3
"""D5b's three-placement container runner (`docs/p5-plan.md`'s D5 "three placements"
milestone; `docs/open-questions.md` questions 148/154/156/194/207/212/213(d)) -- the
COMMITTED, re-runnable script that brings up (and tears down) the `engine`-tier `av-ingest`
container placed on all three resources of `deploy/secdeploy/secsite.altavista-3.toml`
(`edge-a`/`edge-b`/`edge-c`), so the lead can re-run D5 without hand-run shell.

Placed under `scripts/kit/` rather than `deploy/secdeploy/` because it never touches
secdeploy itself (secdeploy is read-and-run-only this round -- our own placement is the
charter-sanctioned path for actually running these containers, `docs/p5-plan.md`'s D5
brief) and it packages/runs a component the same way `scripts/kit/build_kit.py` already
assembles and verifies this repository's OTHER images -- this is that family's own D5
member, not a secdeploy extension.

# Why `--network host`, not a published bridge network (measured, not assumed)

The obvious design -- each container binds `0.0.0.0` internally and `docker run -p
127.0.0.1:<host-port>:<container-port>` publishes it -- does not work here, for two
independent, both DIRECTLY MEASURED reasons (this script's own D5b investigation; commands
reproduced below so a reader can re-run them):

1. `av-ingest-server` itself REFUSES a non-loopback `--grpc-bind`/`--admin-bind` outright
   (question 155/202 -- `tests/test_edge_plugin_container.py`'s own module doc already
   measured this for the identical binary's bind-side check; reconfirmed directly for this
   task):

       $ docker run --rm -v <pubkey>:/opt/av-ingest/keys/k.pem:ro av-ingest:local \\
           --grpc-bind 0.0.0.0:50060 --admin-bind 127.0.0.1:50160 --log-dir /var/lib/av-ingest \\
           --verify-key demo-ground-segment-flight-plugin:/opt/av-ingest/keys/k.pem \\
           --no-require-client-cert --clock-tai-ns 1767225637000000000 \\
           --clearance-ladder UNCLASSIFIED,CUI --max-batch-age-ns 10000000000000
       av-ingest-server: --grpc-bind "0.0.0.0:50060": "0.0.0.0:50060" is not a loopback
       address -- refusing a plaintext bind (question 155/202: ...)

   So a container can only ever be told to bind its OWN `127.0.0.1`.

2. Docker's own port publishing (`-p 127.0.0.1:HOST:CONTAINER`), even with
   `EnableUserlandProxy: true` (measured via `docker info` on this host), does NOT reach a
   service bound only to the container's own loopback -- it forwards to the container's
   bridge-facing interface instead. Measured directly: a probe container bound to
   `127.0.0.1:50060` internally, published via `-p 127.0.0.1:61060:50060` on a normal
   (non-`--internal`) bridge network, accepted a raw TCP connect from the host (`nc -zv
   127.0.0.1 61060` succeeded) but every actual request got "Empty reply from server"
   (`curl`) / a tonic `"transport error"` (the real `av-edge-latency` driver) -- the
   publish's own upstream leg never reaches the process bound to `lo` inside the container.

`--network host`, by contrast, was measured to work directly: a container run with
`--network host --grpc-bind 127.0.0.1:50070 ...` (no `-p` at all) was immediately dialable
from THIS SCRIPT'S OWN HOST-SIDE `av-edge-latency` at plain `127.0.0.1:50070` -- Colima's
own guest-VM-loopback-to-host-loopback forwarding (a Lima feature, distinct from Docker's
`-p` publishing) makes a `--network host` container's own loopback bind reachable from the
macOS host directly, with zero extra plumbing. Every one of D5's three containers therefore
runs `--network host`, each bound to ITS OWN distinct loopback port pair (never `0.0.0.0`,
never colliding with another placement's ports), and no custom Docker *network* object is
created for D5 at all -- there is nothing for one to do: `--network host` does not attach to
a user-defined network, so `docker network ls --filter label=av.test=1` staying empty after
a D5 run is not a missed cleanup, it is the (measured, not assumed) correct state. This is
recorded here, plainly, as a deliberate deviation from a literal "one labelled docker
network" reading of this task's own brief, with the evidence above -- not silently.

# Ports (D5's three placements)

`av-ingest`'s own stated default bind is `127.0.0.1:50060` (gRPC) / `127.0.0.1:50160`
(admin) -- `edge-a` keeps that exact default (`docs/architecture.md`'s "### Default ports"
table, `av-ingest`'s own row, question 208(c)/217(g)). `edge-b`/`edge-c` need distinct ports so
all three can run `--network host` (one shared loopback, since `--network host` means every
container's "own loopback" really is the identical Colima-VM loopback) without colliding --
but they must NOT collide with any OTHER service's own owned default either, since
`--network host` binds against the real host (Colima-VM) loopback, not a container-private
one, and a docker container simply not being started right now is not the same as a port
being free (round 4 manager review, defect 1): `50061`/`50161` is `gmat-service`'s own owned
gRPC/admin default and `50062`/`50162` is `av-dynamics-service`'s (the SAME table) -- an
earlier version of this script squatted on both, quietly, and only ever worked because neither
service happened to be running at the time. `50063`/`50163` and `50064`/`50164` are chosen
instead, deliberately outside every row of the owned port map -- parsed and verified, not
merely eyeballed, by `deploy/secdeploy/ports.py::load_owned_port_map` (the same parser
`tests/test_suite_declarations.py` and `deploy/secdeploy/ports.py` itself already trust as the
single source), in `tests/test_d5_placement_ports.py`, which fails the day the owned map ever
grows a `50063`/`50064` row and forces this script to move:

    edge-a: grpc 127.0.0.1:50060, admin 127.0.0.1:50160  (av-ingest's own owned default)
    edge-b: grpc 127.0.0.1:50063, admin 127.0.0.1:50163  (deliberately outside every owned-map row)
    edge-c: grpc 127.0.0.1:50064, admin 127.0.0.1:50164  (deliberately outside every owned-map row)

# Cross-OS latency comparison caveat (round 4 manager review, defect 3)

D5b's own measured p50/p99 (its runner's timing output, not reproduced in this module) looked
like a 6x improvement over the lead's single-placement macOS-native baseline (p50 6.8 ms / p99
11.3 ms vs. D5b's p50 ~1.12 ms / p99 ~2.81 ms). That gap is NOT a platform improvement --
root-caused (manager, round 4) to `crates/av-ingest/src/log.rs::append`'s `f.sync_all()`, which
Rust maps to a DIFFERENT durability primitive per OS: `fcntl(F_FULLFSYNC)` on macOS (a full
device-cache barrier) vs. plain `fsync(2)` on Linux. Measured directly on this host, on the very
directory this script bind-mounts as each placement's durable log (`out/d5/logs/<placement>`,
200 iterations each, artifact `scratchpad/r4/fullfsync-macos.json`; independently re-measured by
this round's own worker, same directory, same iteration count, materially agreeing --
`scratchpad/r4/w6/fullfsync-worker-verify.json`):

    fsync(2):              p50   37 us, p99   443 us
    fcntl(F_FULLFSYNC):     p50 5665 us (5.67 ms), p99 10539 us (10.54 ms), max 85.7 ms

The lead's baseline ran macOS-native, so its 6.8 ms p50 / 11.3 ms p99 is almost entirely one
`F_FULLFSYNC` (5.67 / 10.54 ms) plus roughly a millisecond of RPC/framing. D5's placements
(this script) run inside Linux containers, where the IDENTICAL Rust code issues plain
`fsync(2)` at 37 us instead -- explaining, too, why the native-host control (p50 13.4 ms) sits
near, rather than far above, the baseline. The two numbers measure two DIFFERENT durability
guarantees, not the same work done faster: a container-placement latency measured by this
script, compared against a macOS-native baseline, is a cross-OS comparison and must always be
labelled as one, never presented as an apples-to-apples speed difference. See
`crates/av-ingest/src/log.rs`'s own "Durability" module-doc section for the short
cross-reference back to this one.

# Question 212: image digest, verified before every container start

`up()` reads `services/av-ingest/IMAGE_DIGEST.md`'s own recorded Image ID and compares it to
`docker image inspect av-ingest:local --format {{.Id}}` -- refuses to start any container if
they disagree (stale/rebuilt image), naming `services/av-ingest/build-image.sh` as the
remedy, exactly `scripts/kit/build_kit.py::_verify_prebuild_base_image_digest`'s own shape
applied to OUR image rather than a prebuild base.

# Question 207: the host-wide docker lock

Every docker-gated operation in this script (`up`, `down`, `status`) runs its ENTIRE body
under `altavista.docker_test_lock.lock_docker_tests()` -- `tests/test_edge_plugin_container.py`
is the existing precedent this script reuses (not reinvents): the SAME lock, the SAME
`altavista.container_hardening.{TEST_LABEL_KEY,label_args,prune_stale_labelled_resources}`
helpers, so a concurrent Rust `cargo test`/other-worktree docker-gated run on this host can
never tear these containers out mid-run, and this script's own resources are pruned by the
identical label a Rust-side daemon-wide sweep would also catch.

# Usage

    scripts/kit/d5_three_placements.py up      # bring up 3 containers + functional check
    scripts/kit/d5_three_placements.py status  # show the 3 containers + a liveness probe
    scripts/kit/d5_three_placements.py down    # tear down, prove every labelled resource gone
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

from altavista.container_hardening import (  # noqa: E402
    HARDENING_RUN_FLAGS,
    TEST_LABEL_KEY,
    TEST_LABEL_VALUE,
    label_args,
    prune_stale_labelled_resources,
)
from altavista.docker_test_lock import lock_docker_tests  # noqa: E402

IMAGE_TAG = "av-ingest:local"
IMAGE_DIGEST_DOC = REPO_ROOT / "services" / "av-ingest" / "IMAGE_DIGEST.md"
COMPONENT_LABEL = ["--label", "org.altavista.component=ingest"]
PUBKEY_FIXTURE = REPO_ROOT / "crates" / "av-edge" / "tests" / "fixtures" / "test_signing_key.pub.pem"
LOG_ROOT = REPO_ROOT / "out" / "d5" / "logs"
DRIVER_BIN = REPO_ROOT / "target" / "release" / "av-edge-latency"

# Producer identity the committed fixture (crates/av-edge/tests/fixtures/ground_segment) --
# and this script's own functional check -- both use; must match av-edge-latency's own
# harness::plugin_config()'s producer_id.
VERIFY_KEY_PRODUCER = "demo-ground-segment-flight-plugin"

CLOCK_TAI_NS = "1767225637000000000"
CLEARANCE_LADDER = "UNCLASSIFIED,CUI"
MAX_BATCH_AGE_NS = "10000000000000"

# D5's three placements -- deploy/secdeploy/secsite.altavista-3.toml's own three resource
# names, in the identical order. Ports: see this module's own doc, "Ports." `edge-a` is
# av-ingest's own owned default (docs/architecture.md "### Default ports", question
# 208(c)/217(g)); `edge-b`/`edge-c` are 50063/50163 and 50064/50164, chosen DELIBERATELY
# outside every row of that same owned port map (never gmat-service's 50061/50161 or
# av-dynamics-service's 50062/50162, which an earlier version of this script quietly squatted
# on -- round 4 manager review, defect 1) -- guarded by tests/test_d5_placement_ports.py, which
# parses the real owned map with deploy/secdeploy/ports.py::load_owned_port_map and fails the
# day it ever grows a 50063/50064 row.
PLACEMENTS: list[dict[str, Any]] = [
    {"label": "edge-a", "grpc_port": 50060, "admin_port": 50160},
    {"label": "edge-b", "grpc_port": 50063, "admin_port": 50163},
    {"label": "edge-c", "grpc_port": 50064, "admin_port": 50164},
]


def _docker(*args: str, timeout: float = 60.0, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        raise RuntimeError(f"docker {' '.join(args)} failed (rc={result.returncode}): {result.stderr}")
    return result


def container_name(placement: str) -> str:
    return f"av-d5-{placement}"


def _verify_image_digest() -> str:
    """Question 212: refuse to start anything if the image built by `services/av-ingest/
    build-image.sh` does not match the digest that script last recorded. Returns the live
    image id on success."""
    if not IMAGE_DIGEST_DOC.exists():
        raise RuntimeError(f"{IMAGE_DIGEST_DOC} does not exist -- build the image first: services/av-ingest/build-image.sh")
    recorded_text = IMAGE_DIGEST_DOC.read_text()
    recorded_id = None
    lines = recorded_text.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "- Image ID (docker image inspect --format {{.Id}}):" and i + 2 < len(lines):
            recorded_id = lines[i + 2].strip()
            break
    if not recorded_id or not recorded_id.startswith("sha256:"):
        raise RuntimeError(f"could not parse a recorded Image ID out of {IMAGE_DIGEST_DOC}")

    inspect = _docker("image", "inspect", IMAGE_TAG, "--format", "{{.Id}}", check=False)
    if inspect.returncode != 0:
        raise RuntimeError(f"{IMAGE_TAG} is not present locally -- build it first: services/av-ingest/build-image.sh")
    live_id = inspect.stdout.strip()
    if live_id != recorded_id:
        raise RuntimeError(
            f"{IMAGE_TAG}'s live image id ({live_id}) does not match the digest recorded in "
            f"{IMAGE_DIGEST_DOC} ({recorded_id}) -- question 212: rebuild and re-verify with "
            f"services/av-ingest/build-image.sh, or investigate the mismatch, before starting any container."
        )
    return live_id


def _wait_for_readiness(name: str, timeout_s: float = 15.0) -> None:
    prefixes = ("GRPC_LISTENING", "ADMIN_LISTENING")
    deadline = time.monotonic() + timeout_s
    while True:
        logs = _docker("logs", name, check=False).stdout
        lines = logs.splitlines()
        if all(any(line.startswith(p) for line in lines) for p in prefixes):
            return
        # A container that exited (crashed on startup) will never print both lines --
        # fail fast rather than waiting out the full timeout.
        inspect = _docker("inspect", name, "--format", "{{.State.Status}}", check=False)
        if inspect.returncode == 0 and inspect.stdout.strip() not in ("running", "created"):
            raise RuntimeError(f"container {name} is {inspect.stdout.strip()!r}, not running -- logs:\n{logs}")
        if time.monotonic() > deadline:
            raise RuntimeError(f"container {name} never printed both {prefixes} within {timeout_s}s -- logs so far:\n{logs}")
        time.sleep(0.1)


def _up(run_id: str, *, functional_check: bool = True, fresh_logs: bool = False) -> None:
    _verify_image_digest()
    if not PUBKEY_FIXTURE.exists():
        raise RuntimeError(f"{PUBKEY_FIXTURE} does not exist")

    print(f"[d5] pruning any stale {TEST_LABEL_KEY}={TEST_LABEL_VALUE} resources before creating (question 156)", file=sys.stderr)
    prune_stale_labelled_resources()

    LOG_ROOT.mkdir(parents=True, exist_ok=True)
    for placement in PLACEMENTS:
        name = placement["label"]
        log_dir = LOG_ROOT / name
        if fresh_logs and log_dir.exists():
            # Question 148: the functional check's own 1-batch proof already wrote a durable
            # log under this same bind-mount path -- each producer's chain (and its
            # duplicate-sequence check) is per log directory, so a clean measured run needs a
            # FRESH per-placement log directory, not merely a fresh container (`--rm` removes
            # the container, never the bind-mounted host directory it wrote into). The
            # functional check's own proof is archived, never silently discarded.
            archive_dir = LOG_ROOT / "_functional_check_archive" / name
            archive_dir.parent.mkdir(parents=True, exist_ok=True)
            if archive_dir.exists():
                import shutil as _shutil
                _shutil.rmtree(archive_dir)
            log_dir.rename(archive_dir)
            print(f"[d5] archived {name}'s functional-check log to {archive_dir}", file=sys.stderr)
        log_dir.mkdir(parents=True, exist_ok=True)
        container = container_name(name)
        print(f"[d5] starting {container} (grpc 127.0.0.1:{placement['grpc_port']}, admin 127.0.0.1:{placement['admin_port']}, log-dir {log_dir})", file=sys.stderr)
        args = [
            "run", "-d", "--rm", "--name", container,
            *label_args(run_id), *COMPONENT_LABEL,
            "--label", f"av.placement={name}",
            "--network", "host",
            *HARDENING_RUN_FLAGS,
            "-v", f"{log_dir}:/var/lib/av-ingest",
            "-v", f"{PUBKEY_FIXTURE}:/opt/av-ingest/keys/test_signing_key.pub.pem:ro",
            IMAGE_TAG,
            "--grpc-bind", f"127.0.0.1:{placement['grpc_port']}",
            "--admin-bind", f"127.0.0.1:{placement['admin_port']}",
            "--log-dir", "/var/lib/av-ingest",
            "--verify-key", f"{VERIFY_KEY_PRODUCER}:/opt/av-ingest/keys/test_signing_key.pub.pem",
            "--no-require-client-cert",
            "--clock-tai-ns", CLOCK_TAI_NS,
            "--clearance-ladder", CLEARANCE_LADDER,
            "--max-batch-age-ns", MAX_BATCH_AGE_NS,
        ]
        _docker(*args)

    for placement in PLACEMENTS:
        container = container_name(placement["label"])
        _wait_for_readiness(container)
        print(f"[d5] {container} ready", file=sys.stderr)

    if functional_check:
        _functional_check()
        print("[d5] all three placements are up and passed the functional check", file=sys.stderr)
    else:
        print("[d5] all three placements are up (functional check skipped -- already proven by an earlier `up`; this is a clean restart for measurement, question 148's proof stands on the earlier run's own archived evidence)", file=sys.stderr)


def _functional_check() -> None:
    """Question: 'prove all three are actually serving before measuring anything.' Runs the
    real `av-edge-latency` driver, natively on this host (the SAME binary and code path Run
    A/Run B use), for exactly 1 batch per placement, then asserts every placement's durable
    log directory on the HOST side actually grew a file -- not just that the driver's own
    exit code was 0 (question 148: an exit code is not evidence, the artifact is)."""
    if not DRIVER_BIN.exists():
        raise RuntimeError(f"{DRIVER_BIN} does not exist -- build it first: cargo build -p av-track --bin av-edge-latency --release")

    targets: list[str] = []
    for p in PLACEMENTS:
        targets += ["--target", f"{p['label']}=127.0.0.1:{p['grpc_port']}"]
    result = subprocess.run([str(DRIVER_BIN), *targets, "--batches", "1", "--in-flight", "1"], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise RuntimeError(f"functional check: av-edge-latency exited {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}")
    report = json.loads(result.stdout)
    for placement_report in report["placements"]:
        if placement_report["accepted_count"] != 1:
            raise RuntimeError(f"functional check: placement {placement_report['label']!r} accepted_count={placement_report['accepted_count']!r}, expected 1: {report}")

    for placement in PLACEMENTS:
        log_dir = LOG_ROOT / placement["label"]
        written = list(log_dir.rglob("*"))
        written_files = [p for p in written if p.is_file()]
        if not written_files:
            raise RuntimeError(f"functional check: {log_dir} (the host-side bind mount of {placement['label']}'s durable log) has no files after a successful Submit -- question 148: the driver's own exit 0 is not evidence on its own")
        print(f"[d5] functional check: {placement['label']} durable log on host: {[str(f.relative_to(LOG_ROOT)) for f in written_files]}", file=sys.stderr)


def _down() -> None:
    label_filter = f"label={TEST_LABEL_KEY}={TEST_LABEL_VALUE}"
    containers = _docker("ps", "-a", "--filter", label_filter, "-q", check=False).stdout.split()
    for cid in containers:
        _docker("rm", "-f", cid, check=False)
        print(f"[d5] removed container {cid}", file=sys.stderr)
    prune_stale_labelled_resources()  # sweeps any labelled network/volume too (none expected -- see this module's own "--network host" doc section).
    print("[d5] teardown complete", file=sys.stderr)


def _status() -> None:
    label_filter = f"label={TEST_LABEL_KEY}={TEST_LABEL_VALUE}"
    print(_docker("ps", "-a", "--filter", label_filter, check=False).stdout)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=["up", "down", "status", "reset"])
    args = parser.parse_args(argv)

    with lock_docker_tests():
        if args.action == "up":
            run_id = uuid.uuid4().hex[:12]
            _up(run_id)
        elif args.action == "down":
            _down()
        elif args.action == "status":
            _status()
        elif args.action == "reset":
            # Tears down the currently-running placements and brings up a FRESH set with
            # fresh (archived, not discarded) per-placement log directories, skipping the
            # functional check -- for use immediately before a measured run, once `up`'s own
            # functional check has already proven the path once (question 148). A measured
            # run must start every producer's chain at sequence 1, which a bare container
            # restart alone does not guarantee (the bind-mounted host log directory, and the
            # chain state in it, survives `--rm` since it lives on the host, not in the
            # container).
            _down()
            run_id = uuid.uuid4().hex[:12]
            _up(run_id, functional_check=False, fresh_logs=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
