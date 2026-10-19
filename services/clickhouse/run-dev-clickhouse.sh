#!/usr/bin/env bash
# services/clickhouse/run-dev-clickhouse.sh -- starts the SAME ClickHouse server image, by the SAME
# digest, that the sweep study store's tests run against (question 245), for a developer to poke
# at by hand (clickhouse-client, curl, a GUI). The same developer convenience
# services/catalog/run-dev-catalog.sh is for PostGIS and services/store/run-dev-store.sh is for
# the S3 server -- this script mirrors the catalog one's structure and lock discipline, adjusted only for
# ClickHouse's ports, env vars and readiness shape.
#
# NOT used by any test: the store's tests start and tear down their own containers through
# av_lockstep::docker::ManagedContainer. This script exists only for a human.
#
#   - Never `docker pull`s (question 154). If the image is not present locally, this script fails
#     with a clear message rather than silently fetching it (services/clickhouse/IMAGE_DIGEST.md,
#     "Setup, once per host", is the one pull).
#   - Parses services/clickhouse/IMAGE_DIGEST.md for the image reference AND the recorded local
#     image id -- the recorded digest has exactly one home; this script is a second READER of that
#     file, never a second place that names the digest.
#   - Refuses to run if the local image's actual id does not match what was recorded (question
#     212(a)): this script's stated purpose is "the SAME image the tests use".
#   - Labels the container `av.test=1` / `av.test.run_id=<id>` (question 156): the next docker-gated
#     test run on this host prunes it, like any other test-created resource.
#   - Takes the host-wide docker-test lock (`$HOME/.altavista/locks/docker-tests.lock`, the path
#     crates/av-lockstep/src/docker_test_lock.rs computes) for only the `docker run` call itself,
#     through python3's stdlib `fcntl.flock` (macOS ships no flock(1) CLI) -- see
#     services/store/run-dev-store.sh's header for why.
#   - Runs with `--tmpfs /var/lib/clickhouse`: the image declares that path a VOLUME, and without
#     the tmpfs every run would leave an anonymous volume behind after `docker rm -f` (measured,
#     IMAGE_DIGEST.md). Data therefore vanishes with the container, which is what a dev scratch
#     server should do.
#   - Creates a user with a password and a database (IMAGE_DIGEST.md: the image's `default` user is
#     reachable only from inside the container unless CLICKHOUSE_USER/CLICKHOUSE_PASSWORD is set,
#     and a CLICKHOUSE_USER without a password is network-reachable with an EMPTY password).
#   - Applies no schema; the store's own migration does that.
#
# Usage: services/clickhouse/run-dev-clickhouse.sh
# Stop it with the `docker rm -f <container id>` command this script prints at the end.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIGEST_MD="$REPO_ROOT/services/clickhouse/IMAGE_DIGEST.md"
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

# Extracts the trimmed contents of the first fenced (```) code block that appears AFTER the first
# line containing $1 -- the same two-block shape services/catalog/run-dev-catalog.sh parses.
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
  echo "       $DIGEST_MD, 'Setup, once per host', has the one pull." >&2
  exit 1
fi
if [[ "$ACTUAL_ID" != "$RECORDED_ID" ]]; then
  echo "error: local image $IMAGE_REF has id $ACTUAL_ID, but $DIGEST_MD recorded $RECORDED_ID." >&2
  echo "       Refusing to run a drifted image under the claim 'the same image the tests use'." >&2
  exit 1
fi

RUN_ID="dev-$$-$(date +%s)"
SANITIZED_RUN_ID="$(printf '%s' "$RUN_ID" | tr -cd 'a-zA-Z0-9')"
CLICKHOUSE_USER="avdev"
CLICKHOUSE_PASSWORD="avdevclickhouse${SANITIZED_RUN_ID}"
CLICKHOUSE_DB="avdevclickhouse${SANITIZED_RUN_ID}"

mkdir -p "$(dirname "$LOCK_FILE")"

# Hold the host-wide docker-test lock (python3's fcntl.flock) only around `docker run` itself.
CONTAINER_ID="$(python3 - "$LOCK_FILE" "$IMAGE_REF" "$RUN_ID" "$CLICKHOUSE_USER" "$CLICKHOUSE_PASSWORD" "$CLICKHOUSE_DB" <<'PYEOF'
import fcntl
import os
import subprocess
import sys

lock_path, image_ref, run_id, user, password, db = sys.argv[1:7]
os.makedirs(os.path.dirname(lock_path), exist_ok=True)
fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
try:
    result = subprocess.run(
        [
            "docker", "run", "-d",
            "--label", "av.test=1",
            "--label", f"av.test.run_id={run_id}",
            "-e", f"CLICKHOUSE_USER={user}",
            "-e", f"CLICKHOUSE_PASSWORD={password}",
            "-e", f"CLICKHOUSE_DB={db}",
            "-p", "127.0.0.1::8123",
            "-p", "127.0.0.1::9000",
            "--tmpfs", "/var/lib/clickhouse",
            image_ref,
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

HTTP_PORT="$(docker port "$CONTAINER_ID" 8123 | head -n1 | rev | cut -d: -f1 | rev)"
NATIVE_PORT="$(docker port "$CONTAINER_ID" 9000 | head -n1 | rev | cut -d: -f1 | rev)"

echo "ClickHouse dev container started (image id $ACTUAL_ID, matches $DIGEST_MD)."
echo "  Container id: $CONTAINER_ID"
echo "  HTTP:         http://127.0.0.1:$HTTP_PORT   (GET /ping needs no auth; queries need basic auth)"
echo "  Native:       127.0.0.1:$NATIVE_PORT"
echo "  User:         $CLICKHOUSE_USER"
echo "  Password:     $CLICKHOUSE_PASSWORD"
echo "  Database:     $CLICKHOUSE_DB"
echo "  Try:          curl -s -u $CLICKHOUSE_USER:$CLICKHOUSE_PASSWORD 'http://127.0.0.1:$HTTP_PORT/?query=SELECT%20version()'"
echo "                (ready within a few seconds -- poll /ping; the entrypoint creates the database first)"
echo "  No schema is applied by this script. No TLS: the image enables no https_port by default"
echo "  (services/clickhouse/IMAGE_DIGEST.md)."
echo "  Stop it with: docker rm -f $CONTAINER_ID"
echo "  (labelled av.test=1 -- the next docker-gated test run on this host will also prune it)"
