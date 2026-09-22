#!/usr/bin/env bash
# services/catalog/run-dev-catalog.sh -- H2 (docs/heavy-plan.md): starts the SAME PostGIS
# image, by the SAME digest, that crates/av-catalog/tests/catalog_postgis.rs runs its own tests
# against, for a developer to poke at by hand (psql, a GUI client, PostGIS's own tooling). The
# SAME developer convenience services/store/run-dev-store.sh already is for MinIO -- this
# script mirrors that one's structure, comments and lock discipline line for line, adjusted
# only for PostgreSQL's own port/env-var/readiness shape.
#
# NOT used by any test. crates/av-catalog/tests/catalog_postgis.rs starts and tears down its
# own containers itself, through av_lockstep::docker::ManagedContainer -- this script exists
# only for a human, and is never invoked from Rust or pytest.
#
# What this script does, and does not, do (see services/store/run-dev-store.sh's own header
# comment for the fuller account this repeats the structure of):
#   - Never `docker pull`s (question 154). If the image is not present locally, this script
#     fails with a clear message rather than silently fetching it.
#   - Parses services/catalog/IMAGE_DIGEST.md for the image reference AND the recorded local
#     image id, the SAME file crates/av-catalog/tests/catalog_postgis.rs's own
#     `parse_image_digest_md` parses -- the recorded digest has exactly one home; this script
#     is a second READER of that file, never a second place that names the digest itself.
#   - Refuses to run if the local image's actual id does not match what was recorded (question
#     212(a) discipline) -- this script's whole stated purpose is "the SAME image the tests
#     use", so silently running a drifted image would make that claim false.
#   - Labels the container `av.test=1` / `av.test.run_id=<id>` (question 156) -- deliberate,
#     not an oversight: the NEXT docker-gated test run on this host will prune this dev
#     container away via prune_stale_test_resources, exactly like any other test-created
#     resource.
#   - Takes the host-wide docker-test lock (`$HOME/.altavista/locks/docker-tests.lock`, the
#     identical path crates/av-lockstep/src/docker_test_lock.rs's own `lock_file_path`
#     computes) for only the `docker run` call itself -- see services/store/run-dev-store.sh's
#     own header comment for exactly why (macOS ships no `flock(1)` CLI, and why this uses
#     python3's stdlib `fcntl.flock` instead of a hand-rolled alternative).
#   - Does NOT run any migration. This script starts a bare PostGIS container -- a developer
#     who wants the catalog schema applied runs it themselves, with the new
#     `av-catalog-migrate` binary (heavy round 6, task 5: `crates/av-catalog/src/bin/
#     av-catalog-migrate.rs`; `cargo build -p av-catalog --bin av-catalog-migrate` then
#     `target/debug/av-catalog-migrate --catalog-host 127.0.0.1 --catalog-port <the port this
#     script printed> --catalog-user postgres --catalog-password <printed> --catalog-database
#     <printed>` -- idempotent, prints what it applied). `scripts/heavy/README.md` step 0.5b is
#     the copy-pasteable version of that command. This mirrors the same way this script's MinIO
#     sibling does not `ensure_bucket` on a developer's behalf either.
#
# Usage: services/catalog/run-dev-catalog.sh
# Stop it with the `docker rm -f <container id>` command this script prints at the end.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIGEST_MD="$REPO_ROOT/services/catalog/IMAGE_DIGEST.md"
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
# crates/av-catalog/tests/catalog_postgis.rs's own `fenced_block_after` parses (itself mirroring
# crates/av-store/tests/minio_store.rs's identically-named function), reimplemented here in awk
# (this script has no Rust toolchain dependency, deliberately) rather than shelling out to the
# test binary itself.
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
POSTGRES_USER="postgres"
POSTGRES_PASSWORD="avdevcatalog${SANITIZED_RUN_ID}"
POSTGRES_DB="avdevcatalog${SANITIZED_RUN_ID}"

mkdir -p "$(dirname "$LOCK_FILE")"

# Hold the host-wide docker-test lock (python3's fcntl.flock) only around `docker run` itself --
# see services/store/run-dev-store.sh's own header comment for why not longer, and why python3
# rather than the flock(1) CLI (absent on this host).
CONTAINER_ID="$(python3 - "$LOCK_FILE" "$IMAGE_REF" "$RUN_ID" "$POSTGRES_PASSWORD" "$POSTGRES_DB" <<'PYEOF'
import fcntl
import os
import subprocess
import sys

lock_path, image_ref, run_id, postgres_password, postgres_db = sys.argv[1:6]
os.makedirs(os.path.dirname(lock_path), exist_ok=True)
fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
try:
    result = subprocess.run(
        [
            "docker", "run", "-d",
            "--label", "av.test=1",
            "--label", f"av.test.run_id={run_id}",
            "-e", f"POSTGRES_PASSWORD={postgres_password}",
            "-e", f"POSTGRES_DB={postgres_db}",
            "-p", "127.0.0.1::5432",
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

HOST_PORT="$(docker port "$CONTAINER_ID" 5432 | head -n1 | rev | cut -d: -f1 | rev)"

echo "PostGIS dev container started (image id $ACTUAL_ID, matches $DIGEST_MD)."
echo "  Container id: $CONTAINER_ID"
echo "  Connect:      psql -h 127.0.0.1 -p $HOST_PORT -U $POSTGRES_USER -d $POSTGRES_DB"
echo "  User:         $POSTGRES_USER"
echo "  Password:     $POSTGRES_PASSWORD"
echo "  Database:     $POSTGRES_DB"
echo "  (SCRAM-SHA-256 auth from outside the container -- services/catalog/IMAGE_DIGEST.md's"
echo "   own measured pg_hba.conf; \`trust\` applies only inside the container itself)"
echo "  No schema is applied by this script -- run av-catalog-migrate against this connection"
echo "  if you want the catalog tables (cargo build -p av-catalog --bin av-catalog-migrate,"
echo "  then target/debug/av-catalog-migrate --catalog-host 127.0.0.1 --catalog-port $HOST_PORT"
echo "  --catalog-user $POSTGRES_USER --catalog-password $POSTGRES_PASSWORD --catalog-database $POSTGRES_DB)."
echo "  Stop it with: docker rm -f $CONTAINER_ID"
echo "  (labelled av.test=1 -- the next docker-gated test run on this host will also prune it)"
