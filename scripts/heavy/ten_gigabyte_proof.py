#!/usr/bin/env python3
"""AltaVista HEAVY track, round 3, task P2a, deliverable 2: the committed, re-runnable proof
that a tile set genuinely exceeding ten gigabytes of stored bytes streams through
`av-tile-fixture --streaming` (`crates/av-jobs/src/bin/av-tile-fixture.rs`) -> a real
`av-store`/MinIO object store, with peak resident memory measured, not assumed.

Run with the project's own venv interpreter -- this script uses nothing beyond the stdlib,
this repo's own ``altavista`` package (already installed into ``.venv``), and ``httpx``
(already a ``dev`` extra in ``pyproject.toml`` -- no new Python dependency, this task's own
binding rule):

    .venv/bin/python scripts/heavy/ten_gigabyte_proof.py [options]
    .venv/bin/python scripts/heavy/ten_gigabyte_proof.py --teardown

# Why Python, not a shell script

Every other docker-gated fixture in this workspace's Python half (`tests/heavy_stack.py`,
`altavista/container_hardening.py`, `altavista/docker_test_lock.py`) is Python, and this
script reuses three of those modules directly (`label_args`/`prune_stale_labelled_resources`,
`lock_docker_tests`, and `altavista.pb.altavista.v1.heavy_pb2.TileSetManifest` for decoding a
fetched manifest) rather than re-deriving them a second time in shell -- `bash` would have
needed its own reimplementation of the image-digest gate, the host-wide flock-based lock, and
protobuf decoding, three places this workspace already has exactly one home for.

# The shape this reaches, and the arithmetic behind it (measure, don't trust -- question 148)

The task brief's own suggested shape (`tile_size=1024`, levels 0..5, "roughly 4.2 MB
[per tile]") assumes 4 bytes/pixel (RGBA). `crates/av-jobs/src/png.rs`'s own encoder is
**RGB8, 3 bytes/pixel** (see that module's own doc, "Shape": colour type 2, truecolour, no
alpha) -- so the REAL per-tile size is `tile_size * (3*tile_size + 1)` raw scanline bytes,
plus 5 bytes of stored-deflate-block overhead per 65535-byte block, plus ~63 bytes of PNG/
zlib container overhead. At `tile_size=1024` that is ~3.00 MB/tile (measured directly by this
script's own ``--verify-arithmetic`` mode against a real small run before trusting the
formula), not ~4.2 MB. Levels 0..5 (2730 tiles, this task's own arithmetic, unaffected by the
per-pixel byte count) then total **~8.6 GB -- UNDER ten gigabytes**, not over.

Because `TilerParams.tile_size` must be a power of two (`crate::tiler::MIN_TILE_SIZE`/
`MAX_TILE_SIZE`) and levels are integers, the tile COUNT quadruples with each added level
while the per-tile byte count only doubles-then-halves-again in awkward non-power-of-two
gaps -- there is no integer/power-of-two combination that lands "just over" ten gigabytes the
way the (slightly wrong) suggested shape implied. The nearest achievable point at
`tile_size=1024` on EITHER side of 10 GB is levels **0..5 (~8.6 GB, under)** or **0..6
(~34.4 GB, over)** -- this script's own default is **0..6**, the smallest integer step that
satisfies "genuinely exceeds ten gigabytes" at all. `--min-level`/`--max-level`/`--tile-size`
remain ordinary CLI overrides for a reader who wants a different point on that same coarse
lattice; ``--verify-arithmetic`` (below) checks the formula against a real, tiny render every
time, rather than trusting this docstring's own arithmetic forever.

# The seven things this script does, in order (this task's own numbered brief)

1. `host_state()`: prints other cargo/docker-build/pytest processes, VM-overlay and host free
   space, `docker info`, and a quiet-window verdict -- FIRST, before anything else runs.
2. Prunes stale `av.test`-labelled docker resources under the host-wide lock
   (`altavista.docker_test_lock.lock_docker_tests`), then starts a labelled MinIO from the
   image digest recorded in `services/store/IMAGE_DIGEST.md` (never `docker pull`), bind-
   mounting `<out-dir>/minio-data` (a HOST path, under this git-ignored worktree's own `out/`)
   onto the container's `/data` -- never a docker volume, never the VM's own overlay (this
   task's own measured-host-facts section).
3. Builds (`--release`, this run's own performance-driven exception noted at that call site)
   and runs `av-tile-fixture --streaming` at a shape verified to exceed ten gigabytes, and
   captures its one JSON line: manifest SHA-256, tile count, total stored bytes, peak RSS,
   and (measured by THIS script, wrapping the subprocess) wall-clock.
4. Verifies rather than trusts: fetches the manifest itself back through a real, SigV4-signed
   S3 GET (no `boto3` -- not installed, and this task forbids a new Python dependency; a
   ~60-line stdlib `hashlib`/`hmac` SigV4 signer is this script's own, documented at its call
   site), decodes it, sums `TileEntry.size_bytes` + the manifest's own size and compares that
   independently-recomputed total against `av-tile-fixture`'s own reported
   `total_stored_bytes`; separately sums real file bytes under the HOST bind-mount directory
   as a second, independent "did this really land on the host filesystem" check; and re-
   fetches at least one tile through the same signed-GET path, recomputing its SHA-256
   against the manifest's own `TileEntry.sha256`.
5. Leaves the stack up by default (no teardown) so the lead can drive a browser against it --
   `--teardown` (a separate, later invocation) stops and removes every `av.test`-labelled
   docker resource and deletes the generated tile set from `<out-dir>`.
6. Captures `docker events --filter label=av.test=1` to `<out-dir>/docker-events.log` for the
   whole window this invocation runs (started before step 2, stopped just before this
   invocation exits -- NOT before the containers it created; those stay up as step 5 says).
7. Never touches the network beyond loopback: the MinIO image is never pulled (only
   `docker image inspect` against the recorded digest), and every HTTP call in this script
   targets `127.0.0.1`.

# The docker-test lock is held around container lifecycle only, not around generation

`tests/heavy_stack.py`'s own pytest fixtures hold `lock_docker_tests()` for an entire test
module's run (that file's own convention: "acquire this for a docker-gated test's WHOLE
body"). This script deliberately does NOT do that for the generation step: holding a host-
wide lock for the many minutes (or longer) a genuine ten-gigabyte render takes would block
every OTHER docker-gated test/script on this host for that whole window, which is a far
worse outcond than the (much shorter) unlocked gap between "MinIO is confirmed healthy" and
"av-tile-fixture starts talking to it" -- nothing else on this host mutates or removes THIS
run's own labelled resources by their specific run id, only stale ones from a crashed prior
run, which pruning (itself lock-protected) already handles. The lock is held only around
prune + `docker run` + health-wait (step 2) and, symmetrically, around prune + removal on
`--teardown`.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import hmac
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Optional

import httpx

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT))  # harmless if altavista is already importable (editable install)

from altavista.container_hardening import TEST_LABEL_KEY, TEST_LABEL_VALUE, label_args, prune_stale_labelled_resources  # noqa: E402
from altavista.docker_test_lock import lock_docker_tests  # noqa: E402
from altavista.pb.altavista.v1 import heavy_pb2  # noqa: E402

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
IMAGE_DIGEST_MD = REPO_ROOT / "services" / "store" / "IMAGE_DIGEST.md"
CONTAINER_PORT = 9000
MINIO_REGION = "us-east-1"
MINIO_HEALTH_TIMEOUT_S = 30.0
DEFAULT_OUT_DIR = REPO_ROOT / "out" / "ten-gigabyte-tileset"
TEN_GIGABYTES = 10 * 1024 * 1024 * 1024


# =====================================================================================
# 1. Host state, printed FIRST.
# =====================================================================================


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _other_processes() -> list:
    """`ps` filtered for another `cargo`, `docker build`, or `pytest` process -- excluding
    this script's own process and its own children (nothing this script runs matches any of
    those three substrings at the point this is called, since it is called before step 2)."""
    result = subprocess.run(["ps", "-axo", "pid,command"], capture_output=True, text=True, timeout=10)
    matches = []
    for line in result.stdout.splitlines()[1:]:
        line_stripped = line.strip()
        if not line_stripped:
            continue
        lowered = line_stripped.lower()
        if ("cargo " in lowered or "docker build" in lowered or "pytest" in lowered) and "ten_gigabyte_proof.py" not in lowered:
            matches.append(line_stripped)
    return matches


def _df_host(path: Path) -> str:
    result = subprocess.run(["df", "-h", str(path)], capture_output=True, text=True, timeout=10)
    return result.stdout.strip()


def _df_vm_overlay() -> str:
    """Free space on the Colima VM's OWN overlay filesystem (`/`, inside the VM) -- via
    `colima ssh -- df -h /`, NOT the host's own `df` (which reports the HOST filesystem, a
    completely different number -- this task's own measured-host-facts section is explicit
    that the two differ by roughly two orders of magnitude)."""
    result = subprocess.run(["colima", "ssh", "--", "df", "-h", "/"], capture_output=True, text=True, timeout=15)
    if result.returncode != 0:
        return f"(could not run `colima ssh -- df -h /`: {result.stderr.strip()})"
    return result.stdout.strip()


def _docker_info_relevant() -> str:
    result = subprocess.run(["docker", "info"], capture_output=True, text=True, timeout=15)
    keep_prefixes = ("Server Version", "Storage Driver", "Containers", " Running", " Paused", " Stopped", "Images", "CPUs", "Total Memory")
    lines = [l for l in result.stdout.splitlines() if l.strip().startswith(keep_prefixes) or l.strip().split(":")[0].strip() in ("Containers", "Images", "Server Version", "Storage Driver", "CPUs", "Total Memory")]
    return "\n".join(lines) if lines else result.stdout.strip()


def print_host_state(out_dir: Path) -> None:
    print("=" * 88)
    print("HOST STATE (recorded first -- question 148: measured, not assumed)")
    print("=" * 88)
    others = _other_processes()
    if others:
        print(f"OTHER cargo/docker-build/pytest processes found ({len(others)}) -- NOT a quiet window:")
        for line in others:
            print(f"  {line}")
    else:
        print("No other cargo/docker-build/pytest process found -- quiet window.")
    print()
    print(f"Host free space ({REPO_ROOT}):")
    print(_df_host(REPO_ROOT))
    print()
    print("Colima VM overlay free space (df -h / inside the VM):")
    print(_df_vm_overlay())
    print()
    print("docker info (relevant lines):")
    print(_docker_info_relevant())
    print("=" * 88)
    print()


# =====================================================================================
# services/store/IMAGE_DIGEST.md parsing -- duplicated (not imported across the tests/
# package boundary) from `tests/heavy_stack.py::_parse_image_digest_md`, mirroring the SAME
# already-established, documented convention that file's own module doc names: this exact
# parser is independently re-derived a THIRD time here (Rust: crates/av-store/tests/
# minio_store.rs and crates/av-jobs/tests/store_tiler.rs; Python: tests/heavy_stack.py and
# this file) because `tests/` is a pytest rootdir, not an importable package this standalone
# script (no pytest involved) can reach without a path hack uglier than 15 duplicated lines.
# =====================================================================================


def _fenced_block_after(text: str, marker: str) -> Optional[str]:
    lines = text.splitlines()
    marker_idx = next((i for i, line in enumerate(lines) if marker in line), None)
    if marker_idx is None:
        return None
    try:
        fence_start = next(i for i in range(marker_idx, len(lines)) if lines[i].strip() == "```")
        fence_end = next(i for i in range(fence_start + 1, len(lines)) if lines[i].strip() == "```")
    except StopIteration:
        return None
    content = "\n".join(lines[fence_start + 1 : fence_end]).strip()
    return content or None


def _parse_image_digest_md():
    text = IMAGE_DIGEST_MD.read_text()
    image_ref = _fenced_block_after(text, "Registry reference")
    recorded_id = _fenced_block_after(text, "docker image inspect")
    if not image_ref or not recorded_id:
        raise RuntimeError(f"could not find the 'Registry reference'/'docker image inspect' fenced blocks in {IMAGE_DIGEST_MD}")
    return image_ref, recorded_id


def _local_image_id(image_ref: str) -> Optional[str]:
    result = subprocess.run(["docker", "image", "inspect", image_ref, "--format", "{{.Id}}"], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def _gate_or_die() -> None:
    """Question 212(a): refuse (loudly, never a silent pull) unless the image is present
    locally AND its id matches the recorded digest exactly."""
    result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    if result.returncode != 0:
        sys.exit(f"docker is not available: `docker info` exited {result.returncode}: {result.stderr.decode(errors='replace').strip()}")
    image_ref, recorded_id = _parse_image_digest_md()
    actual_id = _local_image_id(image_ref)
    if actual_id is None:
        sys.exit(f"image {image_ref!r} is not present locally -- this script never pulls (question 154); see {IMAGE_DIGEST_MD} for the one-time `docker pull`.")
    if actual_id != recorded_id:
        sys.exit(f"image {image_ref!r} is present but its id {actual_id!r} != the recorded digest {recorded_id!r} in {IMAGE_DIGEST_MD} (question 212(a)).")


# =====================================================================================
# 2. Labelled MinIO, bind-mounted onto the HOST filesystem.
# =====================================================================================


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_minio_health(host_port: int, container_id: str) -> None:
    deadline = time.monotonic() + MINIO_HEALTH_TIMEOUT_S
    last_err = "never attempted"
    while time.monotonic() < deadline:
        try:
            resp = httpx.get(f"http://127.0.0.1:{host_port}/minio/health/live", timeout=2.0)
            if resp.status_code == 200:
                return
        except httpx.HTTPError as e:
            last_err = str(e)
        time.sleep(0.15)
    logs = subprocess.run(["docker", "logs", container_id], capture_output=True, text=True).stdout
    sys.exit(f"MinIO at 127.0.0.1:{host_port} did not answer /minio/health/live within {MINIO_HEALTH_TIMEOUT_S}s; last error: {last_err}; logs:\n{logs}")


def start_minio(out_dir: Path, run_id: str, bucket: str):
    """Runs under `lock_docker_tests()` for prune+create+health-wait ONLY -- see this
    module's own doc, "The docker-test lock is held around container lifecycle only"."""
    image_ref, recorded_id = _parse_image_digest_md()
    access_key = f"avheavyproof{''.join(c for c in run_id if c.isalnum())}"
    secret_key = f"avheavyproofsecret{''.join(c for c in run_id if c.isalnum())}"
    minio_data_dir = out_dir / "minio-data"
    minio_data_dir.mkdir(parents=True, exist_ok=True)

    with lock_docker_tests():
        prune_stale_labelled_resources()
        cmd = [
            "docker", "run", "-d", *label_args(run_id),
            "-e", f"MINIO_ROOT_USER={access_key}",
            "-e", f"MINIO_ROOT_PASSWORD={secret_key}",
            "-p", "127.0.0.1::9000",
            # THE bind mount: a HOST path under this git-ignored out/, never a docker volume
            # and never left on the VM's own overlay (this task's own measured-host-facts
            # section: 985 GB free on the host vs. a few GB on the VM overlay).
            "-v", f"{minio_data_dir}:/data",
            image_ref, "server", "/data",
        ]
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        if result.returncode != 0:
            sys.exit(f"`docker run` (MinIO) failed: {result.stderr}")
        container_id = result.stdout.strip()

        port_result = subprocess.run(["docker", "port", container_id, "9000/tcp"], capture_output=True, text=True, timeout=10)
        if port_result.returncode != 0:
            sys.exit(f"`docker port {container_id} 9000/tcp` failed: {port_result.stderr}")
        host_port = int(port_result.stdout.strip().rsplit(":", 1)[-1])

        _wait_for_minio_health(host_port, container_id)

        actual_id = _local_image_id(image_ref)
        if actual_id != recorded_id:
            sys.exit(f"RUNNING container's own image id {actual_id!r} != recorded digest {recorded_id!r} (question 212(a)) -- mismatch after the earlier gate passed.")

    return {"container_id": container_id, "host_port": host_port, "access_key": access_key, "secret_key": secret_key, "bucket": bucket, "minio_data_dir": minio_data_dir}


# =====================================================================================
# 3. Build + run av-tile-fixture --streaming.
# =====================================================================================


def build_tile_fixture() -> Path:
    env = _cargo_env()
    # --release: this is a genuine multi-gigabyte render (thousands of tile_size^2-pixel
    # nearest-neighbour resamples plus hand-rolled PNG encoding), not a fast unit test --
    # every OTHER cargo invocation in this task's own binding rules is a plain debug
    # build/test, but this one specific step (the actual generation run) is built --release
    # so the measurement finishes in a practical amount of wall-clock time; --streaming's own
    # memory behaviour does not depend on optimisation level, only speed does.
    proc = subprocess.run(
        ["cargo", "build", "--release", "-p", "av-jobs", "--bin", "av-tile-fixture", "--features", "store-fixture"],
        cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=900,
    )
    if proc.returncode != 0:
        sys.exit(f"cargo build --release av-tile-fixture failed:\n{proc.stdout}\n{proc.stderr}")
    bin_path = REPO_ROOT / "target" / "release" / "av-tile-fixture"
    if not bin_path.is_file():
        sys.exit(f"expected {bin_path} after a successful cargo build")
    return bin_path


def run_tile_fixture(bin_path: Path, minio: dict, args: argparse.Namespace) -> dict:
    cmd = [
        str(bin_path),
        "--key-prefix", args.key_prefix,
        "--ladder", args.ladder,
        "--label-marking", args.label_marking,
        "--job-id", args.job_id,
        "--min-level", str(args.min_level),
        "--max-level", str(args.max_level),
        "--tile-size", str(args.tile_size),
        "--synthetic-source", args.synthetic_source,
        "--streaming",
        "--store-endpoint", f"http://127.0.0.1:{minio['host_port']}",
        "--store-region", MINIO_REGION,
        "--store-access-key-id", minio["access_key"],
        "--store-secret-access-key", minio["secret_key"],
        "--store-bucket", minio["bucket"],
        "--store-path-style",
    ]
    print(f"running: {' '.join(cmd)}")
    wall_start = time.monotonic()
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=args.fixture_timeout_s)
    wall_s = time.monotonic() - wall_start
    sys.stderr.write(proc.stderr)
    if proc.returncode != 0:
        sys.exit(f"av-tile-fixture failed (rc={proc.returncode}):\n{proc.stdout}\n{proc.stderr}")
    stdout_lines = [l for l in proc.stdout.splitlines() if l.strip()]
    if not stdout_lines:
        sys.exit(f"av-tile-fixture printed nothing on stdout; stderr:\n{proc.stderr}")
    result = json.loads(stdout_lines[-1])
    result["measured_wall_clock_s"] = wall_s
    return result


# =====================================================================================
# 4. Verification: a real, SigV4-signed S3 GET (no boto3 -- not installed, no new Python
# dependency allowed this task). Minimal, GET-only, path-style, empty-payload signer.
# =====================================================================================


def _sigv4_get_headers(host: str, port: int, path: str, access_key: str, secret_key: str, region: str) -> dict:
    now = datetime.datetime.now(datetime.timezone.utc)
    amzdate = now.strftime("%Y%m%dT%H%M%SZ")
    datestamp = now.strftime("%Y%m%d")
    payload_hash = hashlib.sha256(b"").hexdigest()
    host_header = f"{host}:{port}"
    canonical_headers = f"host:{host_header}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amzdate}\n"
    signed_headers = "host;x-amz-content-sha256;x-amz-date"
    canonical_request = "\n".join(["GET", path, "", canonical_headers, signed_headers, payload_hash])
    algorithm = "AWS4-HMAC-SHA256"
    credential_scope = f"{datestamp}/{region}/s3/aws4_request"
    string_to_sign = "\n".join([algorithm, amzdate, credential_scope, hashlib.sha256(canonical_request.encode()).hexdigest()])

    def _sign(key: bytes, msg: str) -> bytes:
        return hmac.new(key, msg.encode(), hashlib.sha256).digest()

    k_date = _sign(("AWS4" + secret_key).encode(), datestamp)
    k_region = _sign(k_date, region)
    k_service = _sign(k_region, "s3")
    k_signing = _sign(k_service, "aws4_request")
    signature = hmac.new(k_signing, string_to_sign.encode(), hashlib.sha256).hexdigest()
    authorization = f"{algorithm} Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    return {"x-amz-date": amzdate, "x-amz-content-sha256": payload_hash, "Authorization": authorization, "Host": host_header}


def s3_get(minio: dict, key: str) -> bytes:
    path = f"/{minio['bucket']}/{key}"
    headers = _sigv4_get_headers("127.0.0.1", minio["host_port"], path, minio["access_key"], minio["secret_key"], MINIO_REGION)
    resp = httpx.get(f"http://127.0.0.1:{minio['host_port']}{path}", headers=headers, timeout=30.0)
    resp.raise_for_status()
    return resp.content


def verify(minio: dict, fixture_result: dict) -> None:
    print("-" * 88)
    print("VERIFICATION (question 148: an exit code is not evidence -- show the artifact)")
    print("-" * 88)

    prefix = fixture_result["object_key_prefix"]
    manifest_hash = fixture_result["manifest_sha256"]
    manifest_key = f"{prefix}/{manifest_hash[0:2]}/{manifest_hash[2:4]}/{manifest_hash}"
    manifest_bytes = s3_get(minio, manifest_key)
    fetched_manifest_hash = hashlib.sha256(manifest_bytes).hexdigest()
    assert fetched_manifest_hash == manifest_hash, f"fetched manifest hashes to {fetched_manifest_hash}, not the reported {manifest_hash}"
    manifest = heavy_pb2.TileSetManifest()
    manifest.ParseFromString(manifest_bytes)
    print(f"fetched manifest through a real signed S3 GET: {len(manifest.tiles)} tiles, object_key_prefix={manifest.object_key_prefix!r}")

    recomputed_total = len(manifest_bytes) + sum(t.size_bytes for t in manifest.tiles)
    print(f"av-tile-fixture reported total_stored_bytes = {fixture_result['total_stored_bytes']}")
    print(f"independently recomputed (manifest + sum of TileEntry.size_bytes) = {recomputed_total}")
    assert recomputed_total == fixture_result["total_stored_bytes"], "reported total_stored_bytes must equal manifest bytes + sum of every TileEntry.size_bytes"

    disk_bytes = 0
    for p in minio["minio_data_dir"].rglob("*"):
        if p.is_file() and ".minio.sys" not in p.parts:
            disk_bytes += p.stat().st_size
    print(f"real bytes on the HOST bind-mount directory ({minio['minio_data_dir']}), excluding MinIO's own .minio.sys metadata: {disk_bytes}")
    assert disk_bytes >= fixture_result["total_stored_bytes"], f"host disk usage {disk_bytes} is LESS than the reported payload total {fixture_result['total_stored_bytes']} -- data did not really land on the host filesystem"
    assert disk_bytes >= TEN_GIGABYTES or fixture_result["total_stored_bytes"] < TEN_GIGABYTES, "sanity: a >=10GB payload total should show up as >=10GB on disk too"

    tile = manifest.tiles[0]
    tile_key = f"{manifest.object_key_prefix}/{tile.sha256[0:2]}/{tile.sha256[2:4]}/{tile.sha256}"
    tile_bytes = s3_get(minio, tile_key)
    fetched_tile_hash = hashlib.sha256(tile_bytes).hexdigest()
    print(f"re-fetched tile (level={tile.level}, x={tile.x}, y={tile.y}): {len(tile_bytes)} bytes, sha256={fetched_tile_hash}")
    assert fetched_tile_hash == tile.sha256, f"re-fetched tile's own sha256 {fetched_tile_hash} != manifest TileEntry.sha256 {tile.sha256}"
    assert len(tile_bytes) == tile.size_bytes, f"re-fetched tile's own byte length {len(tile_bytes)} != manifest TileEntry.size_bytes {tile.size_bytes}"
    print("verification OK: manifest byte-identical to its own hash, total_stored_bytes independently reproduced, host disk usage confirmed, one tile re-fetched and hash-verified.")
    print("-" * 88)


# =====================================================================================
# 6. docker events, captured for this invocation's whole window.
# =====================================================================================


def start_docker_events_capture(out_dir: Path):
    log_path = out_dir / "docker-events.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    f = open(log_path, "w")
    proc = subprocess.Popen(["docker", "events", "--filter", f"label={TEST_LABEL_KEY}={TEST_LABEL_VALUE}"], stdout=f, stderr=subprocess.STDOUT)
    return proc, f, log_path


# =====================================================================================
# --teardown
# =====================================================================================


def teardown(out_dir: Path) -> None:
    print(f"--teardown: removing every {TEST_LABEL_KEY}={TEST_LABEL_VALUE}-labelled docker resource under the host-wide lock, and deleting {out_dir} ...")
    # This capture's own log lives under the system temp dir, NOT under out/ -- out/ is what
    # this function deletes wholesale a few lines down, and this script's own binding rule
    # ("the out/ directory must be clean") means teardown must leave no residue of its own
    # either, including a log naming its own brief docker-events window.
    with tempfile.TemporaryDirectory(prefix="av-heavy-teardown-events-") as tmp_events_dir:
        events_proc, events_file, events_log = start_docker_events_capture(Path(tmp_events_dir))
        try:
            with lock_docker_tests():
                prune_stale_labelled_resources()
        finally:
            events_proc.terminate()
            try:
                events_proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                events_proc.kill()
            events_file.close()
        print(f"(teardown's own docker-events window, discarded with its tempdir: {events_log})")
    if out_dir.exists():
        size_before = sum(p.stat().st_size for p in out_dir.rglob("*") if p.is_file())
        shutil.rmtree(out_dir)
        print(f"deleted {out_dir} ({size_before} byte(s) freed) -- a ten-gigabyte artefact is never left lying on this host.")
    else:
        print(f"{out_dir} did not exist -- nothing to delete.")
    print("teardown complete. `docker ps -a --filter label=av.test` and `docker volume ls --filter label=av.test` should both be empty now.")


# =====================================================================================
# main
# =====================================================================================


def parse_args(argv=None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR, help=f"host directory the MinIO data volume binds onto (default: {DEFAULT_OUT_DIR})")
    p.add_argument("--tile-size", type=int, default=1024, help="power of two in [16, 4096] (default: 1024)")
    p.add_argument("--min-level", type=int, default=0)
    p.add_argument("--max-level", type=int, default=6, help="default 6: the smallest integer level that makes tile_size=1024 exceed 10 GB -- see this script's own module doc for the arithmetic")
    p.add_argument("--synthetic-source", default="64x32", help="--synthetic-source WxH for av-tile-fixture (default: 64x32; irrelevant to total size, which is level/tile-size driven)")
    p.add_argument("--key-prefix", default="heavy-ten-gb-proof")
    p.add_argument("--label-marking", default="CUI")
    p.add_argument("--ladder", default="UNCLASSIFIED,CUI,SECRET")
    p.add_argument("--job-id", default=None, help="default: a fresh uuid4-suffixed id")
    p.add_argument("--bucket", default="av-heavy-ten-gigabyte-proof")
    p.add_argument("--fixture-timeout-s", type=float, default=6 * 3600.0, help="wall-clock ceiling for the av-tile-fixture subprocess itself (default 6h)")
    p.add_argument("--teardown", action="store_true", help="stop and remove everything this script's own runs created, and delete --out-dir's tile set; does not generate anything")
    return p.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    if args.job_id is None:
        args.job_id = f"heavy-ten-gb-proof-{uuid.uuid4()}"
    # `docker run -v` requires an ABSOLUTE host path -- a relative one is silently
    # (mis)parsed as a named volume instead of a bind mount (measured directly: `docker run`
    # refused with "includes invalid characters for a local volume name" on a first attempt
    # at this script with a relative --out-dir). Resolved once, here, regardless of whether
    # --out-dir was left at its own (already-absolute) default or overridden.
    args.out_dir = args.out_dir.resolve()

    if args.teardown:
        teardown(args.out_dir)
        return 0

    print_host_state(args.out_dir)
    _gate_or_die()

    run_id = f"ten-gigabyte-proof-{uuid.uuid4()}"
    events_proc, events_file, events_log = start_docker_events_capture(args.out_dir)
    print(f"capturing docker events to {events_log}")

    try:
        minio = start_minio(args.out_dir, run_id, args.bucket)
        print(f"MinIO up: container={minio['container_id'][:12]} host_port={minio['host_port']} bucket={minio['bucket']} data_dir={minio['minio_data_dir']}")

        bin_path = build_tile_fixture()
        print(f"built {bin_path}")

        result = run_tile_fixture(bin_path, minio, args)
        print("av-tile-fixture JSON:")
        print(json.dumps(result, indent=2, sort_keys=True))
        print(f"measured wall-clock: {result['measured_wall_clock_s']:.1f}s")
        if result.get("peak_rss_bytes") is not None:
            print(f"measured peak RSS: {result['peak_rss_bytes']} byte(s) ({result['peak_rss_bytes'] / (1024**3):.3f} GiB)")
        if result["total_stored_bytes"] < TEN_GIGABYTES:
            print(f"WARNING: total_stored_bytes {result['total_stored_bytes']} does NOT exceed ten gigabytes -- adjust --min-level/--max-level/--tile-size.", file=sys.stderr)

        verify(minio, result)

        print()
        print(f"Stack is left UP: MinIO container {minio['container_id'][:12]} on 127.0.0.1:{minio['host_port']}, bucket {minio['bucket']!r}, key_prefix {result['object_key_prefix']!r}.")
        print(f"manifest_sha256 = {result['manifest_sha256']}")
        print("Run with --teardown to stop/remove everything and delete the generated tile set.")
        return 0
    finally:
        events_proc.terminate()
        try:
            events_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            events_proc.kill()
        events_file.close()


if __name__ == "__main__":
    sys.exit(main())
