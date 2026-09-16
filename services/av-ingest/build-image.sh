#!/usr/bin/env bash
# services/av-ingest/build-image.sh -- the single documented way to (re)create
# `av-ingest:local` (D5b, docs/p5-plan.md's D5 "three placements" milestone; modelled on
# services/edge-plugin/build-image.sh -- see that script's own header for the precedent this
# one follows, with one deliberate simplification: `av-ingest-server` was ALREADY cross-built
# for this round (round 4 commit 7c70ac8's own proof), so this script has no prebuild `docker
# run` step of its own -- it verifies the existing cross-built binary's size+digest, then
# builds the runtime image straight from it).
#
# What this does, in order:
#   1. Preconditions: docker installed and running, the Dockerfile present.
#   2. Verifies the already cross-built `av-ingest-server` binary's size and SHA-256 against
#      the values recorded in services/av-ingest/Dockerfile's own header comment (round 4
#      commit 7c70ac8's proof) -- refuses to build from anything that does not match, rather
#      than silently packaging an unexpected binary (question 212's own rule, applied one
#      step before the image itself even exists).
#   3. Verifies the runtime base image (debian:bookworm-slim, pinned by digest in the
#      Dockerfile's own FROM line) is present locally under that EXACT digest --
#      `docker image inspect ... RepoDigests` -- mirroring `scripts/kit/build_kit.py::
#      _verify_prebuild_base_image_digest`'s identical shape (question 154: never pulls;
#      question 212: never trusts a base image without comparing it to its recorded digest).
#   4. Prunes by label (question 156: prune before creating) -- removes any image already
#      carrying `label=org.altavista.component=av-ingest`.
#   5. Builds `av-ingest:local` from services/av-ingest/Dockerfile, from the repository root.
#   6. Reads back the built image's content-addressed ID and records it -- plus the runtime
#      base image digest and the packaged binary's own SHA-256 -- in services/av-ingest/
#      IMAGE_DIGEST.md, the way services/catalog/IMAGE_DIGEST.md and services/edge-plugin/
#      IMAGE_DIGEST.md already do (question 212).
#
# This script is NEVER invoked by any test -- services/cfs/build-image.sh's own rule,
# verbatim; scripts/kit/d5_three_placements.py only ever inspects an already-built image
# and refuses, naming this script, when it is missing or its digest does not match.
#
# Idempotent and re-runnable: every artifact this script writes (the image tag,
# IMAGE_DIGEST.md) is fully overwritten on each run, and step 4's prune means a re-run never
# accumulates dangling images under this component's label.
#
# Usage:
#   services/av-ingest/build-image.sh

if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-image.sh: run this with bash (\`bash services/av-ingest/build-image.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="av-ingest:local"
LABEL_FILTER="label=org.altavista.component=av-ingest"
DIGEST_DOC="${SCRIPT_DIR}/IMAGE_DIGEST.md"

# The already cross-built binary this image packages (round 4 commit 7c70ac8's own proof,
# restated in services/av-ingest/Dockerfile's own header comment) -- verified below before
# `docker build` ever sees it.
BIN_PATH="${REPO_ROOT}/target-docker-linux-kit/av-ingest/release/av-ingest-server"
EXPECTED_BIN_SIZE=2765064
EXPECTED_BIN_SHA256="0acdf73d7cf53f46fdd9c8065ae066d905e05d37b88b46f2577b97aee2b06315"

log() { printf '[build-image] %s\n' "$1" >&2; }
die() { printf '[build-image] ERROR: %s\n' "$1" >&2; exit 1; }

START_SECONDS=$SECONDS

# --- 0. Preconditions. ----------------------------------------------------------------------
command -v docker >/dev/null 2>&1 || die "docker binary not found on PATH -- install/start Docker before running this script."
if ! docker info >/dev/null 2>&1; then
    die "\`docker info\` failed -- Docker is not running (or not accessible). Start Docker (or the Colima VM) and re-run."
fi
[ -f "${DOCKERFILE}" ] || die "Dockerfile not found at ${DOCKERFILE}"

# --- 1. Verify the already cross-built binary before ever building from it (question 212,
# applied to this script's own input, not just its output). --------------------------------
[ -f "${BIN_PATH}" ] || die "${BIN_PATH} does not exist -- this binary must already be cross-built for this round (round 4 commit 7c70ac8's own proof); rebuild it through scripts/kit/build_kit.py's _cross_build_one_binary (read that commit for how) before re-running this script."
ACTUAL_BIN_SIZE="$(wc -c < "${BIN_PATH}" | tr -d ' ')"
ACTUAL_BIN_SHA256="$(shasum -a 256 "${BIN_PATH}" | awk '{print $1}')"
if [ "${ACTUAL_BIN_SIZE}" != "${EXPECTED_BIN_SIZE}" ] || [ "${ACTUAL_BIN_SHA256}" != "${EXPECTED_BIN_SHA256}" ]; then
    die "${BIN_PATH} does not match its recorded size/digest (question 212) -- expected ${EXPECTED_BIN_SIZE} bytes / sha256:${EXPECTED_BIN_SHA256}, found ${ACTUAL_BIN_SIZE} bytes / sha256:${ACTUAL_BIN_SHA256}. Rebuild it through scripts/kit/build_kit.py's _cross_build_one_binary (read round 4 commit 7c70ac8 for how) rather than building an image from an unverified binary."
fi
log "verified ${BIN_PATH}: ${ACTUAL_BIN_SIZE} bytes, sha256:${ACTUAL_BIN_SHA256} (matches the recorded round 4 commit 7c70ac8 proof)"

# --- 2. Verify the runtime base image is present locally under its exact pinned digest
# (question 212, mirroring scripts/kit/build_kit.py::_verify_prebuild_base_image_digest). ----
RUNTIME_BASE_IMAGE="$(grep -E '^FROM[[:space:]]' "${DOCKERFILE}" | head -1 | awk '{print $2}')"
[ -n "${RUNTIME_BASE_IMAGE}" ] || die "could not parse a FROM line out of ${DOCKERFILE}"
case "${RUNTIME_BASE_IMAGE}" in
    *@sha256:*) ;;
    *) die "the Dockerfile's FROM line (${RUNTIME_BASE_IMAGE}) is not pinned by digest -- question 154/185's own rule: every base image is pinned by content digest, never a floating tag." ;;
esac
BASE_REPO="${RUNTIME_BASE_IMAGE%@*}"
BASE_DIGEST="${RUNTIME_BASE_IMAGE#*@}"
BASE_EXPECTED="${BASE_REPO}@${BASE_DIGEST}"
BASE_REPO_DIGESTS_JSON="$(docker image inspect "${RUNTIME_BASE_IMAGE}" --format '{{json .RepoDigests}}' 2>/dev/null || true)"
if [ -z "${BASE_REPO_DIGESTS_JSON}" ]; then
    die "${RUNTIME_BASE_IMAGE} is not present locally under this exact digest -- question 154 forbids pulling it here. Either the base image was garbage-collected off this host (re-pull it once, out-of-band, at setup time -- never at build/test time), or investigate why the digest this Dockerfile is pinned to no longer matches what is on this host."
fi
case "${BASE_REPO_DIGESTS_JSON}" in
    *"${BASE_EXPECTED}"*) ;;
    *) die "${RUNTIME_BASE_IMAGE} does NOT match its recorded digest (question 212) -- \`docker image inspect\` reports RepoDigests=${BASE_REPO_DIGESTS_JSON}, which does not contain ${BASE_EXPECTED}." ;;
esac
log "verified base image ${RUNTIME_BASE_IMAGE} is present locally under its exact pinned digest"

# --- 3. Prune by label before creating (question 156). --------------------------------------
log "pruning any dangling image labelled ${LABEL_FILTER} before building"
STALE_IDS="$(docker images --filter "${LABEL_FILTER}" -q | sort -u || true)"
if [ -n "${STALE_IDS}" ]; then
    while IFS= read -r id; do
        [ -n "${id}" ] || continue
        docker rmi -f "${id}" >/dev/null 2>&1 || log "WARNING: could not remove stale image ${id} (label ${LABEL_FILTER}) -- it may still be referenced by a running container; continuing."
    done <<< "${STALE_IDS}"
else
    log "no stale ${LABEL_FILTER} images found"
fi

# --- 4. Build the runtime image, once. -------------------------------------------------------
log "building ${IMAGE_TAG} from ${DOCKERFILE} (context: ${REPO_ROOT})"
docker build -f "${DOCKERFILE}" -t "${IMAGE_TAG}" "${REPO_ROOT}" 1>&2

IMAGE_ID="$(docker image inspect "${IMAGE_TAG}" --format '{{.Id}}')"
[ -n "${IMAGE_ID}" ] || die "docker image inspect returned an empty .Id for ${IMAGE_TAG}"
log "built image ID: ${IMAGE_ID}"

# --- 5. Record the digests and the packaged binary's own hash. ------------------------------
DURATION_S=$(( SECONDS - START_SECONDS ))
{
    printf '# services/av-ingest/IMAGE_DIGEST.md -- generated by services/av-ingest/build-image.sh\n'
    printf '# Do not hand-edit; re-run the script to regenerate.\n\n'
    printf '## %s (most recent build)\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Image tag: `%s`\n' "${IMAGE_TAG}"
    printf -- '- Image ID (docker image inspect --format {{.Id}}):\n'
    printf '```\n%s\n```\n' "${IMAGE_ID}"
    printf -- '- Runtime base image, pinned by digest (this Dockerfile'\''s own FROM line, verified present under this exact digest before the build ran):\n'
    printf '```\n%s\n```\n' "${RUNTIME_BASE_IMAGE}"
    printf -- '- Packaged `av-ingest-server` binary (%s), already cross-built (round 4 commit 7c70ac8), verified before this build:\n' "${BIN_PATH#"${REPO_ROOT}"/}"
    printf '```\n%s bytes, sha256:%s\n```\n' "${ACTUAL_BIN_SIZE}" "${ACTUAL_BIN_SHA256}"
    printf -- '- Build duration (this run): %ds\n' "${DURATION_S}"
    printf -- '- Rebuild with: `services/av-ingest/build-image.sh`\n'
    printf -- '- Consumed by: `scripts/kit/d5_three_placements.py` (D5b: three containers, one per site-file placement, labelled edge-a/edge-b/edge-c), which compares the RUNNING image'\''s digest against this record before starting any container (question 212).\n'
} > "${DIGEST_DOC}.new"
mv "${DIGEST_DOC}.new" "${DIGEST_DOC}"
log "wrote ${DIGEST_DOC}"

log "done in ${DURATION_S}s. Image ID: ${IMAGE_ID}"
printf 'image_id=%s\n' "${IMAGE_ID}"
printf 'runtime_base_image=%s\n' "${RUNTIME_BASE_IMAGE}"
printf 'packaged_binary_sha256=%s\n' "${ACTUAL_BIN_SHA256}"
printf 'build_duration_s=%s\n' "${DURATION_S}"
