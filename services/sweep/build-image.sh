#!/usr/bin/env bash
# services/sweep/build-image.sh -- the single documented way to (re)create `av-sweep-sample:local`
# (question 245, heavy-ct-2b: a feasibility sweep fanned out as k3s Jobs, byte-identical to a
# local run). Modelled on services/tiles/build-image.sh almost line for line; see
# services/sweep/Dockerfile's own header for what the image is and why THIS script, not the
# Dockerfile, runs the real `cargo build` (the same wall services/tiles/Dockerfile proves).
#
# What this does, in order:
#   0. Preconditions: docker running, git present, the Dockerfile and the two record files
#      present, a spoore checkout at SPOORE_ROOT (default: ../spoore next to this repository; the
#      cross-build needs it bind-mounted so Cargo can load every workspace member's manifest),
#      and the GMAT install at GMAT_ROOT (default: this repository's `GMAT R2026a`).
#   1. Starts a live `docker events` capture for this script's whole run (question 194).
#   2. Prunes: removes only DANGLING images (no tag) labelled org.altavista.component=sweep,
#      i.e. the previous build's image once this build moves the tag. It never removes a tagged
#      image, so the current `av-sweep-sample:local` (and any digest reference to it) is untouched.
#   3. Prebuilds `av-sweep` -- release, `--no-default-features` (no GMAT), `--locked`, stripped --
#      in a real `docker run` of the pinned rust:1.90-bookworm image with this repository and
#      SPOORE_ROOT bind-mounted. This is the one permitted network window (question 154): apt
#      packages and the crates the lockfile names are fetched inside that container. Writes
#      out/sweep-image/bin/av-sweep (a build artifact).
#   4. Stages the GMAT data files named in IMAGE_DATA_FILES.txt into out/sweep-image/data/gmat/data/,
#      verifying every SHA-256 (it refuses to build on a missing file or a mismatch).
#   5. Builds `av-sweep-sample:local` from services/sweep/Dockerfile, from the repository root.
#   6. Records, in services/sweep/IMAGE_DIGEST.md: the image id, the runtime and prebuild base
#      images (pinned by digest), the binary's SHA-256, the shared libraries it needs (`ldd` inside
#      the built image), the data files with sizes and hashes, the rustc used, the commit the
#      image was built from (and whether the tree was dirty under IMAGE_COPIED_PATHS.txt), and the
#      build duration.
#   7. Stops the docker-events capture, writing out/sweep-image/build/last-build-events.jsonl.
#
# This script is NEVER invoked by any test. It does not take the host-wide docker lock itself: run
# it under `.venv/bin/python /Users/probe/code/altavista-lead/bin/with_docker_lock.py -- <this>`
# when other docker work may be running.
#
# Idempotent and re-runnable: every artifact it writes is fully overwritten on each run.
#
# Usage:
#   services/sweep/build-image.sh

# Same bash-only guard as services/tiles/build-image.sh, for the identical reason.
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-image.sh: run this with bash (\`bash services/sweep/build-image.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="av-sweep-sample:local"
LABEL_FILTER="label=org.altavista.component=sweep"
STAGE_DIR="${REPO_ROOT}/out/sweep-image"
BIN_DIR="${STAGE_DIR}/bin"
BIN_PATH="${BIN_DIR}/av-sweep"
DATA_DIR="${STAGE_DIR}/data/gmat/data"
DIGEST_DOC="${SCRIPT_DIR}/IMAGE_DIGEST.md"
COPIED_PATHS_FILE="${SCRIPT_DIR}/IMAGE_COPIED_PATHS.txt"
DATA_FILES_FILE="${SCRIPT_DIR}/IMAGE_DATA_FILES.txt"
EVENTS_LOG="${STAGE_DIR}/build/last-build-events.jsonl"
SPOORE_ROOT="${SPOORE_ROOT:-${REPO_ROOT}/../spoore}"
GMAT_ROOT="${GMAT_ROOT:-${REPO_ROOT}/GMAT R2026a}"
# The container mount destination matters; see services/tiles/build-image.sh's own comment for the
# measured account (mounting THIS repository at a container path whose parent is literally
# /Users/probe/code makes spoore's sibling mount destination equal the fixed absolute path
# crates/av-proposer's dependencies expect). Reused verbatim; it does not have to equal, and does
# not equal, this worktree's real host path.
CONTAINER_WORKSPACE="/Users/probe/code/AltaVista-edge"
SPOORE_CONTAINER_PATH="$(dirname "${CONTAINER_WORKSPACE}")/spoore"
# Pinned prebuild base -- the SAME digest services/tiles/, services/edge-plugin/ and
# services/proposer/ pin.
PREBUILD_BASE_IMAGE="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
SCRATCH_TARGET_DIR="${REPO_ROOT}/target-docker-linux"

log() { printf '[build-image] %s\n' "$1" >&2; }
warn() { printf '[build-image] WARNING: %s\n' "$1" >&2; }
die() { printf '[build-image] ERROR: %s\n' "$1" >&2; exit 1; }

START_SECONDS=$SECONDS

# --- Docker-events capture (question 194), as in services/tiles/build-image.sh. ---------------
EVENTS_PID=""

start_events_capture() {
    if ! mkdir -p "$(dirname "${EVENTS_LOG}")" 2>/dev/null; then
        warn "could not create $(dirname "${EVENTS_LOG}") -- continuing without a docker events capture for this build."
        return
    fi
    if ! : > "${EVENTS_LOG}.tmp" 2>/dev/null; then
        warn "could not create ${EVENTS_LOG}.tmp -- continuing without a docker events capture for this build."
        return
    fi
    docker events --format '{{json .}}' --filter type=image --filter type=container >> "${EVENTS_LOG}.tmp" 2>&1 &
    EVENTS_PID=$!
    sleep 0.3
    if ! kill -0 "${EVENTS_PID}" 2>/dev/null; then
        warn "docker events process exited immediately -- continuing without a docker events capture for this build."
        EVENTS_PID=""
        return
    fi
    log "docker events capture started (pid ${EVENTS_PID}); will write ${EVENTS_LOG}"
}

EVENTS_NOISE_ACTIONS='"Action":"(exec_create|exec_start|exec_die|health_status)'

stop_events_capture() {
    [ -n "${EVENTS_PID}" ] || return 0
    if kill -0 "${EVENTS_PID}" 2>/dev/null; then
        kill "${EVENTS_PID}" 2>/dev/null || true
        wait "${EVENTS_PID}" 2>/dev/null || true
    fi
    EVENTS_PID=""
    if [ -f "${EVENTS_LOG}.tmp" ]; then
        if grep -Ev "${EVENTS_NOISE_ACTIONS}" "${EVENTS_LOG}.tmp" > "${EVENTS_LOG}.filtered" 2>/dev/null || [ -f "${EVENTS_LOG}.filtered" ]; then
            local raw_lines filtered_lines
            raw_lines=$(wc -l < "${EVENTS_LOG}.tmp" | tr -d ' ')
            filtered_lines=$(wc -l < "${EVENTS_LOG}.filtered" | tr -d ' ')
            mv "${EVENTS_LOG}.filtered" "${EVENTS_LOG}"
            rm -f "${EVENTS_LOG}.tmp"
            log "docker events for this build's window written to ${EVENTS_LOG} (${filtered_lines} of ${raw_lines} captured events kept)"
        else
            mv "${EVENTS_LOG}.tmp" "${EVENTS_LOG}"
            log "docker events for this build's window written to ${EVENTS_LOG} (unfiltered)"
        fi
    fi
}

_cleanup_on_exit() {
    local status=$?
    stop_events_capture
    exit "${status}"
}
trap _cleanup_on_exit EXIT

# --- 0. Preconditions. --------------------------------------------------------------------
command -v docker >/dev/null 2>&1 || die "docker binary not found on PATH -- install/start Docker before running this script."
if ! docker info >/dev/null 2>&1; then
    die "\`docker info\` failed -- Docker is not running (or not accessible). Start Docker (or the Colima VM) and re-run."
fi
command -v git >/dev/null 2>&1 || die "git binary not found on PATH."
[ -f "${DOCKERFILE}" ] || die "Dockerfile not found at ${DOCKERFILE}"
[ -f "${COPIED_PATHS_FILE}" ] || die "${COPIED_PATHS_FILE} not found -- question 232's commit-provenance record needs the copied-paths list."
[ -f "${DATA_FILES_FILE}" ] || die "${DATA_FILES_FILE} not found -- it names the GMAT data files this image carries, with their SHA-256."
SPOORE_ROOT="$(cd "${SPOORE_ROOT}" 2>/dev/null && pwd)" || die "SPOORE_ROOT (${SPOORE_ROOT}) does not exist -- set SPOORE_ROOT to your spoore checkout, or place one at the sibling-checkout default ${REPO_ROOT}/../spoore (question 219(b))."
[ -d "${SPOORE_ROOT}/crates/spoore-cdm" ] || die "${SPOORE_ROOT}/crates/spoore-cdm not found -- the prebuild step bind-mounts SPOORE_ROOT so Cargo can resolve the workspace."
[ -d "${GMAT_ROOT}/data" ] || die "GMAT_ROOT (${GMAT_ROOT}) has no data/ directory -- set GMAT_ROOT to a GMAT R2026a install."

start_events_capture

# --- 1. Prune only dangling (untagged) images of this component. --------------------------
log "pruning dangling images labelled ${LABEL_FILTER} (a tagged image is never removed)"
STALE_IDS="$(docker images --filter "${LABEL_FILTER}" --filter dangling=true -q | sort -u || true)"
if [ -n "${STALE_IDS}" ]; then
    while IFS= read -r id; do
        [ -n "${id}" ] || continue
        docker rmi "${id}" >/dev/null 2>&1 || warn "could not remove dangling image ${id} (label ${LABEL_FILTER}) -- it may still be referenced; continuing."
    done <<< "${STALE_IDS}"
else
    log "no dangling ${LABEL_FILTER} images found"
fi

# --- 2. Prebuild av-sweep, stripped, via a bind-mounted docker run. ----------------------------
mkdir -p "${BIN_DIR}"
rm -rf "${SCRATCH_TARGET_DIR}"
log "prebuilding av-sweep (release, --no-default-features, --locked, stripped) via ${PREBUILD_BASE_IMAGE} with ${SPOORE_ROOT} bind-mounted read-only at ${SPOORE_CONTAINER_PATH} -- this is the one permitted network window (question 154): apt packages and the crates Cargo.lock names, fetched inside that container"
docker run --rm \
    -v "${REPO_ROOT}:${CONTAINER_WORKSPACE}" \
    -v "${SPOORE_ROOT}:${SPOORE_CONTAINER_PATH}:ro" \
    -w "${CONTAINER_WORKSPACE}" \
    "${PREBUILD_BASE_IMAGE}" \
    bash -c 'set -euo pipefail
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null
        cargo build --release --locked -p av-sweep --no-default-features --bin av-sweep --target-dir target-docker-linux
        strip target-docker-linux/release/av-sweep
        rustc --version > target-docker-linux/rustc-version.txt
        cargo --version >> target-docker-linux/rustc-version.txt' \
    1>&2

[ -f "${SCRATCH_TARGET_DIR}/release/av-sweep" ] || die "prebuild finished but ${SCRATCH_TARGET_DIR}/release/av-sweep does not exist"
cp "${SCRATCH_TARGET_DIR}/release/av-sweep" "${BIN_PATH}"
chmod +x "${BIN_PATH}"
BIN_SHA256="$(shasum -a 256 "${BIN_PATH}" | awk '{print $1}')"
TOOLCHAIN="$(tr '\n' ';' < "${SCRATCH_TARGET_DIR}/rustc-version.txt" | sed 's/;$//; s/;/; /')"
log "prebuilt binary: ${BIN_PATH} (sha256:${BIN_SHA256}, $(du -h "${BIN_PATH}" | awk '{print $1}')), built by ${TOOLCHAIN}"
rm -rf "${SCRATCH_TARGET_DIR}"

# --- 3. Stage and verify the GMAT data files. -----------------------------------------------
rm -rf "${STAGE_DIR}/data/gmat"
DATA_TABLE=""
DATA_COUNT=0
while IFS= read -r line; do
    case "${line}" in
        ''|'#'*) continue ;;
    esac
    want_sha="${line%%  *}"
    rel_path="${line#*  }"
    [ -n "${want_sha}" ] && [ -n "${rel_path}" ] && [ "${want_sha}" != "${line}" ] || die "malformed line in ${DATA_FILES_FILE}: ${line}"
    src="${GMAT_ROOT}/data/${rel_path}"
    [ -f "${src}" ] || die "data file ${src} (named in ${DATA_FILES_FILE}) does not exist"
    got_sha="$(shasum -a 256 "${src}" | awk '{print $1}')"
    [ "${got_sha}" = "${want_sha}" ] || die "data file ${src} has sha256 ${got_sha}, ${DATA_FILES_FILE} says ${want_sha} -- refusing to build"
    mkdir -p "${DATA_DIR}/$(dirname "${rel_path}")"
    cp "${src}" "${DATA_DIR}/${rel_path}"
    size="$(wc -c < "${src}" | tr -d ' ')"
    DATA_TABLE="${DATA_TABLE}${want_sha}  ${size}  ${rel_path}"$'\n'
    DATA_COUNT=$((DATA_COUNT + 1))
done < "${DATA_FILES_FILE}"
[ "${DATA_COUNT}" -gt 0 ] || die "${DATA_FILES_FILE} names no data files"
log "staged ${DATA_COUNT} GMAT data file(s) under ${DATA_DIR}, every SHA-256 verified"

# --- 4. Build the runtime image, once. ---------------------------------------------------------
log "building ${IMAGE_TAG} from ${DOCKERFILE} (context: ${REPO_ROOT})"
docker build -f "${DOCKERFILE}" -t "${IMAGE_TAG}" "${REPO_ROOT}" 1>&2

IMAGE_ID="$(docker image inspect "${IMAGE_TAG}" --format '{{.Id}}')"
[ -n "${IMAGE_ID}" ] || die "docker image inspect returned an empty .Id for ${IMAGE_TAG}"
IMAGE_ARCH="$(docker image inspect "${IMAGE_TAG}" --format '{{.Os}}/{{.Architecture}}')"
IMAGE_SIZE="$(docker image inspect "${IMAGE_TAG}" --format '{{.Size}}')"
log "built image ID: ${IMAGE_ID} (${IMAGE_ARCH}, ${IMAGE_SIZE} bytes)"

RUNTIME_BASE_IMAGE="$(grep -E '^FROM[[:space:]]' "${DOCKERFILE}" | head -1 | awk '{print $2}')"
[ -n "${RUNTIME_BASE_IMAGE}" ] || die "could not parse a FROM line out of ${DOCKERFILE}"
case "${RUNTIME_BASE_IMAGE}" in
    *@sha256:*) ;;
    *) die "the Dockerfile's FROM line (${RUNTIME_BASE_IMAGE}) is not pinned by digest -- every base image is pinned by content digest, never a floating tag." ;;
esac

# The shared libraries the stripped binary needs, measured inside the built image itself.
LDD_OUT="$(docker run --rm --entrypoint ldd "${IMAGE_TAG}" /usr/local/bin/av-sweep 2>&1 | sed 's/^[[:space:]]*//')" || warn "ldd inside the image failed; recording its output as is."

# --- 4b. The commit HEAD points at, and whether the tree was dirty under the copied paths. -------
COPIED_PATHS=()
while IFS= read -r copied_path; do
    case "${copied_path}" in
        ''|'#'*) continue ;;
    esac
    COPIED_PATHS+=("${copied_path}")
done < "${COPIED_PATHS_FILE}"
[ "${#COPIED_PATHS[@]}" -gt 0 ] || die "${COPIED_PATHS_FILE} names no paths"

BUILD_COMMIT="$(git -C "${REPO_ROOT}" rev-parse HEAD)"
DIRTY_STATUS="$(git -C "${REPO_ROOT}" status --porcelain -- "${COPIED_PATHS[@]}")"
DIRTY_COUNT=0
if [ -n "${DIRTY_STATUS}" ]; then
    DIRTY_COUNT="$(printf '%s\n' "${DIRTY_STATUS}" | grep -c .)"
fi
if [ "${DIRTY_COUNT}" -gt 0 ]; then
    warn "working tree is dirty under ${DIRTY_COUNT} of this image's own copied path(s) at build time -- IMAGE_DIGEST.md records this honestly; rebuild from a clean tree (after the work is committed) for a provenance the commit check will trust."
fi

# --- 5. Record. --------------------------------------------------------------------------------
DURATION_S=$(( SECONDS - START_SECONDS ))
{
    printf '# services/sweep/IMAGE_DIGEST.md -- generated by services/sweep/build-image.sh\n'
    printf '# Do not hand-edit; re-run the script to regenerate.\n\n'
    printf '## %s (most recent build)\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Image tag: `%s` (%s). The sample job names it as `av-sweep-sample` + the id below: `av-sweep --executor k8s --sample-image av-sweep-sample@<id>`.\n' "${IMAGE_TAG}" "${IMAGE_ARCH}"
    printf -- '- Image ID (docker image inspect --format {{.Id}}):\n'
    printf '```\n%s\n```\n' "${IMAGE_ID}"
    printf -- '- Runtime base image, pinned by digest (this Dockerfile'\''s own FROM line):\n'
    printf '```\n%s\n```\n' "${RUNTIME_BASE_IMAGE}"
    printf -- '- Prebuild base image, pinned by digest (used only by this script'\''s own bind-mounted `docker run`, never by the Dockerfile itself):\n'
    printf '```\n%s\n```\n' "${PREBUILD_BASE_IMAGE}"
    printf -- '- Prebuilt `av-sweep` binary SHA-256 (%s; `--release --locked -p av-sweep --no-default-features`, stripped; built by %s):\n' "${BIN_PATH#"${REPO_ROOT}"/}" "${TOOLCHAIN}"
    printf '```\nsha256:%s\n```\n' "${BIN_SHA256}"
    printf -- '- Binary installed at `/usr/local/bin/av-sweep`, with no ENTRYPOINT or CMD (the job'\''s command names the binary in full, so one command line runs the same under docker and k8s); runtime user uid 10003 `avsweep` (the executors run it as 10001:10001); no VOLUME; `GMAT_ROOT=/opt/av-sweep/gmat`.\n'
    printf -- '- Shared libraries the binary needs (`ldd` inside the image):\n'
    printf '```\n%s\n```\n' "${LDD_OUT}"
    printf -- '- GMAT data files carried at `/opt/av-sweep/gmat/data/` (sha256, bytes, path under `data/`; the list and hashes are services/sweep/IMAGE_DATA_FILES.txt, verified at build):\n'
    printf '```\n%s```\n' "${DATA_TABLE}"
    printf -- '- Built from commit (question 232, extending question 212(a) -- the paths this covers are services/sweep/IMAGE_COPIED_PATHS.txt):\n'
    if [ "${DIRTY_COUNT}" -gt 0 ]; then
        printf '```\n%s (working tree dirty: %s modified paths under the copied paths)\n```\n' "${BUILD_COMMIT}" "${DIRTY_COUNT}"
    else
        printf '```\n%s\n```\n' "${BUILD_COMMIT}"
    fi
    printf -- '- Network gate image the k8s sample pods use (not part of this image; the pinned alpine of crates/av-jobs/tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md, `--k8s-network-gate-image`'\''s default): `alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6`.\n'
    printf -- '- **Not a kit component this round** (the lead decides whether it becomes one): this image is not in the air-gap kit.\n'
    printf -- '- Build duration (this run, prebuild + image build): %ds\n' "${DURATION_S}"
    printf -- '- Rebuild with: `services/sweep/build-image.sh`\n'
} > "${DIGEST_DOC}.new"
mv "${DIGEST_DOC}.new" "${DIGEST_DOC}"
log "wrote ${DIGEST_DOC}"

stop_events_capture

log "done in ${DURATION_S}s. Image ID: ${IMAGE_ID}"
printf 'image_id=%s\n' "${IMAGE_ID}"
printf 'runtime_base_image=%s\n' "${RUNTIME_BASE_IMAGE}"
printf 'prebuild_base_image=%s\n' "${PREBUILD_BASE_IMAGE}"
printf 'prebuilt_binary_sha256=%s\n' "${BIN_SHA256}"
printf 'build_duration_s=%s\n' "${DURATION_S}"
