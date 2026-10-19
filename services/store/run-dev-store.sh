#!/usr/bin/env bash
# services/store/run-dev-store.sh -- H1b (docs/heavy-plan.md): starts the SAME S3 server image, by
# the SAME digest, that crates/av-store/tests/s3_store.rs runs its own tests against, for a
# developer to poke at by hand (an S3 client, curl against the S3 API directly). Which server that
# is (question 256) is whatever services/store/IMAGE_DIGEST.md records; nothing here names it.
#
# NOT used by any test. crates/av-store/tests/s3_store.rs starts and tears down its own
# containers itself, through av_lockstep::docker::ManagedContainer -- this script exists only
# for a human, and is never invoked from Rust or pytest.
#
# What this script does, and does not, do:
#   - Never `docker pull`s (question 154: the image is pulled once at setup, by the manager,
#     never at test/run time). If the image is not present locally, this script fails with a
#     clear message rather than silently fetching it.
#   - Parses services/store/IMAGE_DIGEST.md for the image reference, the recorded local image id
#     AND the launch record (container port, health path, credential environment, server
#     arguments) -- the SAME file crates/av-store/tests/s3_store.rs reads through
#     `av_lockstep::docker::parse_recorded_launch` and tests/heavy_stack.py through
#     `altavista/store_record.py`. The record has exactly one home; this script is another READER
#     of that file (in awk, as before), never a place that names the digest, a port, a health path
#     or a server argument itself. The credential lines are templates; this script substitutes
#     the `{access_key}` and `{secret_key}` placeholders (and refuses any other brace) with the
#     per-run generated credentials it prints at the end.
#   - Refuses to run if the local image's actual id does not match what was recorded (the same
#     question-212(a) discipline the Rust integration test's own gate applies) -- this script's
#     whole stated purpose is "the SAME image the tests use", so silently running a drifted
#     image would make that claim false.
#   - Publishes the container port on loopback only (`-p 127.0.0.1::<port>`), never on all
#     interfaces.
#   - Labels the container `av.test=1` / `av.test.run_id=<id>` -- the SAME labels every
#     Docker-gated test in this workspace uses (question 156). This is deliberate, not an
#     oversight: it means the NEXT docker-gated test run on this host (Rust or Python) will
#     prune this dev container away via prune_stale_test_resources, exactly like any other
#     test-created resource. A developer who wants an S3 container that survives a test run
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
# Stop it with the `docker rm -f -v <container id>` command this script prints at the end (-v:
# the image may declare a VOLUME, and a plain `rm -f` would leave its anonymous volume behind).

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

# Extracts the contents of the first fenced (```) code block that appears AFTER the first line
# containing $1 -- the same shape av_lockstep::docker::fenced_block_after and
# altavista/store_record.py's `fenced_block_after` parse, reimplemented here in awk (this
# script has no Rust toolchain dependency, deliberately) rather than shelling out to the test
# binary itself. Every line is trimmed and empty lines are dropped, as both of those readers do
# for the multi-line launch-record fields, so a one-line block prints one line.
fenced_block_after() {
  awk -v marker="$1" '
    !found && index($0, marker) { found=1; next }
    found && !infence && /^```$/ { infence=1; next }
    found && infence && /^```$/ { exit }
    found && infence { gsub(/^[ \t]+|[ \t]+$/, ""); if (length($0)) print }
  ' "$DIGEST_MD"
}

# One required field of the record, as a single line (the first line of its block).
required_field() {
  local value
  value="$(fenced_block_after "$1" | head -n1)"
  if [[ -z "$value" ]]; then
    echo "error: could not parse the '$1' fenced code block out of $DIGEST_MD" >&2
    exit 1
  fi
  printf '%s' "$value"
}

IMAGE_REF="$(required_field "Registry reference")"
RECORDED_ID="$(required_field "docker image inspect")"
CONTAINER_PORT="$(required_field "Container port")"
HEALTH_PATH="$(required_field "Health path")"
if ! [[ "$CONTAINER_PORT" =~ ^[0-9]+$ ]] || (( CONTAINER_PORT < 1 || CONTAINER_PORT > 65535 )); then
  echo "error: the 'Container port' block of $DIGEST_MD must be an integer from 1 to 65535, got '$CONTAINER_PORT'" >&2
  exit 1
fi
if [[ "$HEALTH_PATH" != /* || "$HEALTH_PATH" == *[[:space:]]* ]]; then
  echo "error: the 'Health path' block of $DIGEST_MD must be one token starting with '/', got '$HEALTH_PATH'" >&2
  exit 1
fi
CREDENTIAL_LINES="$(fenced_block_after "Credential environment")"
SERVER_ARGS_LINES="$(fenced_block_after "Server arguments")"
if [[ -z "$CREDENTIAL_LINES" ]]; then
  echo "error: could not parse the 'Credential environment' fenced code block out of $DIGEST_MD" >&2
  exit 1
fi
if [[ -z "$SERVER_ARGS_LINES" ]]; then
  echo "error: could not parse the 'Server arguments' fenced code block out of $DIGEST_MD" >&2
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

# The record's credential lines (NAME=template), rendered with the per-run credentials above.
# Only {access_key} and {secret_key} are substituted; any other brace is refused (the same rule
# the Rust and Python readers apply). bash 3.2 (macOS /bin/bash): no mapfile, so a read loop.
DOCKER_RUN_ARGV=(docker run -d
  --label av.test=1
  --label "av.test.run_id=$RUN_ID"
)
while IFS= read -r line; do
  name="${line%%=*}"
  template="${line#*=}"
  if [[ "$line" != *=* || ! "$name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "error: Credential environment line '$line' in $DIGEST_MD is not NAME=template" >&2
    exit 1
  fi
  leftover="${template//\{access_key\}/}"
  leftover="${leftover//\{secret_key\}/}"
  if [[ "$leftover" == *"{"* || "$leftover" == *"}"* ]]; then
    echo "error: Credential environment line '$line' in $DIGEST_MD uses a brace other than {access_key} or {secret_key}" >&2
    exit 1
  fi
  value="${template//\{access_key\}/$ACCESS_KEY}"
  value="${value//\{secret_key\}/$SECRET_KEY}"
  DOCKER_RUN_ARGV+=(-e "$name=$value")
done <<< "$CREDENTIAL_LINES"
DOCKER_RUN_ARGV+=(-p "127.0.0.1::$CONTAINER_PORT" "$IMAGE_REF")
while IFS= read -r line; do
  DOCKER_RUN_ARGV+=("$line")
done <<< "$SERVER_ARGS_LINES"

mkdir -p "$(dirname "$LOCK_FILE")"

# Hold the host-wide docker-test lock (python3's fcntl.flock -- see this script's own header
# comment for why not the flock(1) CLI, which does not exist on this host) only around
# `docker run` itself -- see the header comment for why not longer. python3 runs `docker run`
# as a child process with the lock already held, prints the container id docker itself printed
# (docker run -d's own stdout), and only then releases the lock (the `finally` block) --
# exactly the same "lock held across the mutating call, released right after" shape
# `lock_docker_tests()`/`prune_stale_test_resources()`'s own Rust callers use.
CONTAINER_ID="$(python3 - "$LOCK_FILE" "${DOCKER_RUN_ARGV[@]}" <<'PYEOF'
import fcntl
import os
import subprocess
import sys

lock_path = sys.argv[1]
docker_run_argv = sys.argv[2:]
os.makedirs(os.path.dirname(lock_path), exist_ok=True)
fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
try:
    result = subprocess.run(docker_run_argv, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.exit(result.returncode)
    sys.stdout.write(result.stdout.strip())
finally:
    fcntl.flock(fd, fcntl.LOCK_UN)
    os.close(fd)
PYEOF
)"

HOST_PORT="$(docker port "$CONTAINER_ID" "$CONTAINER_PORT" | head -n1 | rev | cut -d: -f1 | rev)"

echo "S3 dev container started (image id $ACTUAL_ID, matches $DIGEST_MD)."
echo "  Container id: $CONTAINER_ID"
echo "  Endpoint:     http://127.0.0.1:$HOST_PORT"
echo "  Access key:   $ACCESS_KEY"
echo "  Secret key:   $SECRET_KEY"
echo "  Health check: curl http://127.0.0.1:$HOST_PORT$HEALTH_PATH"
echo "  Stop it with: docker rm -f -v $CONTAINER_ID"
echo "  (labelled av.test=1 -- the next docker-gated test run on this host will also prune it)"
