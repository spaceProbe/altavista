#!/usr/bin/env bash
# services/store/run-dev-store.sh -- H1b (docs/heavy-plan.md): starts the SAME MinIO image, by
# the SAME digest, that crates/av-store/tests/minio_store.rs runs its own tests against, for a
# developer to poke at by hand (mc, curl against the S3 API directly, the MinIO console).
#
# NOT used by any test. crates/av-store/tests/minio_store.rs starts and tears down its own
# containers itself, through av_lockstep::docker::ManagedContainer -- this script exists only
# for a human, and is never invoked from Rust or pytest.
#
# What this script does, and does not, do:
#   - Never `docker pull`s (question 154: the image is pulled once at setup, by the manager,
#     never at test/run time). If the image is not present locally, this script fails with a
#     clear message rather than silently fetching it.
#   - Parses services/store/IMAGE_DIGEST.md for the image reference AND the recorded local
#     image id, the SAME file crates/av-store/tests/minio_store.rs's own
#     `parse_image_digest_md` parses -- the recorded digest has exactly one home; this script
#     is a second READER of that file, never a second place that names the digest itself.
#   - Refuses to run if the local image's actual id does not match what was recorded (the same
#     question-212(a) discipline the Rust integration test's own gate applies) -- this script's
#     whole stated purpose is "the SAME image the tests use", so silently running a drifted
#     image would make that claim false.
#   - Labels the container `av.test=1` / `av.test.run_id=<id>` -- the SAME labels every
#     Docker-gated test in this workspace uses (question 156). This is deliberate, not an
#     oversight: it means the NEXT docker-gated test run on this host (Rust or Python) will
#     prune this dev container away via prune_stale_test_resources, exactly like any other
#     test-created resource. A developer who wants a MinIO container that survives a test run
#     should say so explicitly (this script does not offer that mode -- keeping one label
#     convention, with one meaning, is worth more than a second flag nobody but this script
#     understands).
#   - Takes the host-wide docker-test lock (`$HOME/.altavista/locks/docker-tests.lock`, the
#     identical path crates/av-lockstep/src/docker_test_lock.rs's own `lock_file_path` computes)
#     for only the `docker run` call itself, not for the whole time a developer spends poking at
#     the container afterward -- holding it longer would block every OTHER docker-gated test or
#     dev script on this host for no reason once the container has actually started.
#     macOS (this host) ships no `flock(1)` CLI utility at all (that is a Linux util-linux
#     tool -- measured directly: `flock -x 200` here answers "flock: command not found", and
#     because macOS's default `/bin/bash` is 3.2 (pre-`inherit_errexit`), that failure inside a
#     `$(...)` command substitution does NOT stop this script even under `set -e`, so this was
#     caught by actually running the script, not by inspection). This script therefore takes
#     the lock through `python3`'s stdlib `fcntl.flock` instead -- the SAME primitive
#     `crates/av-lockstep/src/docker_test_lock.rs`'s own cross-language proof test already
#     uses to verify a Python process observes the identical Rust-held lock, and the same
#     primitive `altavista/docker_test_lock.py`'s own Python-side tests use for its half of
#     this workspace's cross-language contract -- so this is a proven-compatible primitive on
#     this host, not a new one this script hopes works. The lock PATH is re-derived from
#     `$HOME` inline (not imported from `altavista.docker_test_lock`), the same deliberate
#     choice `docker_test_lock.rs`'s own cross-process/cross-language test explains: proving
#     two independent computations of the path agree is stronger than one importing the
#     other's constant.
#
# Usage: services/store/run-dev-store.sh
# Stop it with the `docker rm -f <container id>` command this script prints at the end.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIGEST_MD="$REPO_ROOT/services/store/IMAGE_DIGEST.md"
LOCK_FILE="$HOME/.altavista/locks/docker-tests.lock"

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker is not installed or not on PATH" >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "error: \`docker info\` failed -- the Docker daemon is not reachable" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 is not installed or not on PATH -- needed to take the host-wide docker-test lock (see this script's own header comment for why not the flock(1) CLI)" >&2
  exit 1
fi
if [[ ! -f "$DIGEST_MD" ]]; then
  echo "error: $DIGEST_MD does not exist -- nothing to parse the image reference/digest from" >&2
  exit 1
fi

# Extracts the trimmed contents of the first fenced (```) code block that appears AFTER the
# first line containing $1 -- the same two-block shape
# crates/av-store/tests/minio_store.rs's own `fenced_block_after` parses, reimplemented here in
# awk (this script has no Rust toolchain dependency, deliberately) rather than shelling out to
# the test binary itself.
fenced_block_after() {
  awk -v marker="$1" '
    !found && index($0, marker) { found=1; next }
    found && !infence && /^```$/ { infence=1; next }
    found && infence && /^```$/ { exit }
    found && infence { print }
  ' "$DIGEST_MD"
}

IMAGE_REF="$(fenced_block_after "Registry reference")"
RECORDED_ID="$(fenced_block_after "docker image inspect")"
if [[ -z "$IMAGE_REF" ]]; then
  echo "error: could not parse the 'Registry reference' fenced code block out of $DIGEST_MD" >&2
  exit 1
fi
if [[ -z "$RECORDED_ID" ]]; then
  echo "error: could not parse the 'docker image inspect' fenced code block out of $DIGEST_MD" >&2
  exit 1
fi

# Never `docker pull` (question 154) -- `docker image inspect` only ever reads local state.
if ! ACTUAL_ID="$(docker image inspect "$IMAGE_REF" --format '{{.Id}}' 2>/dev/null)"; then
  echo "error: $IMAGE_REF is not present locally -- this script never pulls it (question 154)." >&2
  echo "       Ask the manager how this image was pulled at setup, or pull it yourself by the" >&2
  echo "       exact reference in $DIGEST_MD if you know that is safe on this host." >&2
  exit 1
fi
if [[ "$ACTUAL_ID" != "$RECORDED_ID" ]]; then
  echo "error: local image $IMAGE_REF has id $ACTUAL_ID, but $DIGEST_MD recorded $RECORDED_ID." >&2
  echo "       Refusing to run a drifted image under the claim 'the same image the tests use'." >&2
  exit 1
fi

RUN_ID="dev-$$-$(date +%s)"
SANITIZED_RUN_ID="$(printf '%s' "$RUN_ID" | tr -cd 'a-zA-Z0-9')"
ACCESS_KEY="avdevstore${SANITIZED_RUN_ID}"
SECRET_KEY="avdevstoresecret${SANITIZED_RUN_ID}"

mkdir -p "$(dirname "$LOCK_FILE")"

# Hold the host-wide docker-test lock (python3's fcntl.flock -- see this script's own header
# comment for why not the flock(1) CLI, which does not exist on this host) only around
# `docker run` itself -- see the header comment for why not longer. python3 runs `docker run`
# as a child process with the lock already held, prints the container id docker itself printed
# (docker run -d's own stdout), and only then releases the lock (the `finally` block) --
# exactly the same "lock held across the mutating call, released right after" shape
# `lock_docker_tests()`/`prune_stale_test_resources()`'s own Rust callers use.
CONTAINER_ID="$(python3 - "$LOCK_FILE" "$IMAGE_REF" "$RUN_ID" "$ACCESS_KEY" "$SECRET_KEY" <<'PYEOF'
import fcntl
import os
import subprocess
import sys

lock_path, image_ref, run_id, access_key, secret_key = sys.argv[1:6]
os.makedirs(os.path.dirname(lock_path), exist_ok=True)
fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
try:
    result = subprocess.run(
        [
            "docker", "run", "-d",
            "--label", "av.test=1",
            "--label", f"av.test.run_id={run_id}",
            "-e", f"MINIO_ROOT_USER={access_key}",
            "-e", f"MINIO_ROOT_PASSWORD={secret_key}",
            "-p", "127.0.0.1::9000",
            image_ref,
            "server", "/data",
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.exit(result.returncode)
    sys.stdout.write(result.stdout.strip())
finally:
    fcntl.flock(fd, fcntl.LOCK_UN)
    os.close(fd)
PYEOF
)"

HOST_PORT="$(docker port "$CONTAINER_ID" 9000 | head -n1 | rev | cut -d: -f1 | rev)"

echo "MinIO dev container started (image id $ACTUAL_ID, matches $DIGEST_MD)."
echo "  Container id: $CONTAINER_ID"
echo "  Endpoint:     http://127.0.0.1:$HOST_PORT"
echo "  Access key:   $ACCESS_KEY"
echo "  Secret key:   $SECRET_KEY"
echo "  Health check: curl http://127.0.0.1:$HOST_PORT/minio/health/live"
echo "  Stop it with: docker rm -f $CONTAINER_ID"
echo "  (labelled av.test=1 -- the next docker-gated test run on this host will also prune it)"
