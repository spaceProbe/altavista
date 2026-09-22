#!/usr/bin/env bash
# services/tiles/build-image.sh -- the single documented way to (re)create `av-tiles:local`
# (H4's remaining round-2 open item 6, docs/heavy-plan.md: "a docker-gated proof of av-tiles
# serving out of a real MinIO" -- modelled on services/edge-plugin/build-image.sh almost line
# for line; see services/tiles/Dockerfile's own header comment for why THIS script, not the
# Dockerfile, has to run the real `cargo build`).
#
# What this does, in order:
#   1. Preconditions: docker installed and running, git present, the Dockerfile present, and a
#      spoore checkout present at SPOORE_ROOT -- an environment override, defaulting to the
#      sibling checkout `../spoore` next to this repository's own root (question 219(b)) -- the
#      cross-build below needs it bind-mounted even though `av-tiles` itself never uses spoore
#      for anything (see the Dockerfile's own header comment: Cargo has to load every workspace
#      member's manifest to resolve the workspace at all, `av-proposer` included).
#   2. Starts a live `docker events` capture for this script's own full execution window
#      (question 194's round-6 amendment -- verbatim precedent from services/edge-plugin/
#      build-image.sh, adapted paths only).
#   3. Prunes by label (question 156: prune before creating) -- removes any image already
#      carrying `label=org.altavista.component=tiles`.
#   4. Prebuilds `av-tiles`, stripped, via a real `docker run` bind-mounting BOTH this
#      repository and SPOORE_ROOT (read-only, at the sibling of the repository's own mount
#      point -- question 219(b)/(c)) into a pinned `rust:1.90-bookworm` container. Writes
#      services/tiles/bin/av-tiles (git-ignored, a build artifact).
#   5. Builds `av-tiles:local` from services/tiles/Dockerfile, from the repository root.
#   6. Reads back the built image's content-addressed ID and this Dockerfile's own pinned base
#      image reference, and writes both -- plus the prebuild base image reference, the
#      prebuilt binary's own SHA-256, and this run's build duration -- to
#      services/tiles/IMAGE_DIGEST.md.
#   7. Stops the docker-events capture, writing services/tiles/build/last-build-events.jsonl
#      (question 194's own attribution record).
#
# This script is NEVER invoked by any test (services/edge-plugin/build-image.sh's own rule,
# verbatim) -- tests/test_tiles_container.py (via tests/heavy_stack.py's own
# `_compute_tiles_skip_reason`) only ever inspects an already-built image and skips VISIBLY,
# naming this script, when it is absent or its digest has drifted (question 194/212(a)).
#
# Idempotent and re-runnable: every artifact this script writes (the prebuilt binary, the
# image tag, IMAGE_DIGEST.md, the events log) is fully overwritten on each run, and step 3's
# prune means a re-run never accumulates dangling images under this component's label.
#
# Usage:
#   services/tiles/build-image.sh

# Same bash-only guard as services/edge-plugin/build-image.sh, and for the identical reason:
# this script uses bash arrays and process substitution, which a POSIX-mode `sh` invocation
# parses lazily and can fail on partway through a run, after some Docker side effects have
# already happened. Refuse up front instead.
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-image.sh: run this with bash (\`bash services/tiles/build-image.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="av-tiles:local"
LABEL_FILTER="label=org.altavista.component=tiles"
BIN_DIR="${SCRIPT_DIR}/bin"
BIN_PATH="${BIN_DIR}/av-tiles"
DIGEST_DOC="${SCRIPT_DIR}/IMAGE_DIGEST.md"
# Question 232 (extending question 212(a)): the ONE place the paths this image's content is
# derived from are listed -- this script reads it to compute whether the tree was dirty under
# them at build time; tests/heavy_stack.py's own verify_image_commit_provenance reads the SAME
# file to run `git log <recorded_commit>..HEAD -- <these paths>`. Never duplicated.
COPIED_PATHS_FILE="${SCRIPT_DIR}/IMAGE_COPIED_PATHS.txt"
EVENTS_LOG="${SCRIPT_DIR}/build/last-build-events.jsonl"
# Question 219(b): SPOORE_ROOT overrides the spoore checkout used for the cross-build below;
# unset, it defaults to the sibling checkout next to this repository's own root (question 12's
# convention, matching the root Cargo.toml's own relative `spoore-cdm` dependency, question
# 219(c)). Resolved to an absolute, existing path below -- never left as a possibly-relative
# string a later `cd`/mount could misinterpret.
SPOORE_ROOT="${SPOORE_ROOT:-${REPO_ROOT}/../spoore}"
# The container mount destination matters (see the Dockerfile's own header comment, and
# services/edge-plugin/build-image.sh's own identical comment for the full measured account of
# why a plain `/workspace` mount is not enough and a second mount/symlink does not work
# either): mounting THIS repository at a container path whose PARENT is literally
# `/Users/probe/code` makes spoore's own sibling mount destination equal the fixed absolute
# path `crates/av-proposer/Cargo.toml`'s still-absolute dependencies expect, by construction --
# even though `av-tiles` itself never touches spoore, Cargo still has to resolve THAT
# manifest to resolve the workspace at all. This literal is reused verbatim from
# services/edge-plugin/build-image.sh's own `CONTAINER_WORKSPACE` -- it is a container-internal
# mount point name, chosen only so its parent equals `/Users/probe/code`; it does not have to
# equal (and does not equal) this worktree's own real host path, exactly as it already did not
# for that script.
CONTAINER_WORKSPACE="/Users/probe/code/AltaVista-edge"
SPOORE_CONTAINER_PATH="$(dirname "${CONTAINER_WORKSPACE}")/spoore"
# Pinned prebuild base -- the SAME digest services/edge-plugin/build-image.sh and
# services/proposer/build-image.sh already pin (see services/tiles/Dockerfile's own "Pinning"
# header section for why reusing it, rather than re-resolving a third one, is deliberate).
PREBUILD_BASE_IMAGE="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
SCRATCH_TARGET_DIR="${REPO_ROOT}/target-docker-linux"

log() { printf '[build-image] %s\n' "$1" >&2; }
warn() { printf '[build-image] WARNING: %s\n' "$1" >&2; }
die() { printf '[build-image] ERROR: %s\n' "$1" >&2; exit 1; }

START_SECONDS=$SECONDS

# --- Docker-events capture (question 194), verbatim precedent from services/edge-plugin/
# build-image.sh -- one combined EXIT trap so the events-capture cleanup can never be
# clobbered by any other cleanup. ------------------------------------------------------------
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
    # Not piped through a filter here, deliberately -- see services/edge-plugin/build-image.sh's
    # own identical comment: `$!` on a pipeline is the LAST command's pid, which would leave
    # `docker events` itself running as an orphan once `stop_events_capture` reaps only the
    # filter.
    docker events --format '{{json .}}' --filter type=image --filter type=container >> "${EVENTS_LOG}.tmp" 2>&1 &
    EVENTS_PID=$!
    sleep 0.3
    if ! kill -0 "${EVENTS_PID}" 2>/dev/null; then
        warn "docker events process exited immediately -- continuing without a docker events capture for this build."
        EVENTS_PID=""
        return
    fi
    log "docker events capture started (pid ${EVENTS_PID}); will write ${EVENTS_LOG} (type=image, type=container: tag/untag/delete/pull/push, create/start/die/destroy; unrelated containers' exec_*/health_status noise dropped when the log is finalised)"
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
            log "docker events for this build's window written to ${EVENTS_LOG} (${filtered_lines} of ${raw_lines} captured events kept; the rest were unrelated containers' exec_*/health_status noise)"
        else
            warn "could not filter the docker events capture -- keeping the raw stream instead, which is noisier but never less complete."
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
[ -f "${COPIED_PATHS_FILE}" ] || die "${COPIED_PATHS_FILE} not found -- question 232's commit-provenance record needs the same copied-paths list tests/heavy_stack.py's own verifier reads."
SPOORE_ROOT="$(cd "${SPOORE_ROOT}" 2>/dev/null && pwd)" || die "SPOORE_ROOT (${SPOORE_ROOT}) does not exist -- set SPOORE_ROOT to your spoore checkout, or place one at the sibling-checkout default ${REPO_ROOT}/../spoore (question 219(b))."
[ -d "${SPOORE_ROOT}/crates/spoore-cdm" ] || die "${SPOORE_ROOT}/crates/spoore-cdm not found -- the prebuild step below bind-mounts SPOORE_ROOT even though av-tiles itself never uses it (see the Dockerfile's own header comment for why Cargo still needs it to resolve the workspace)."

start_events_capture

# --- 1. Prune by label before creating (question 156). ------------------------------------
log "pruning any dangling image labelled ${LABEL_FILTER} before building"
STALE_IDS="$(docker images --filter "${LABEL_FILTER}" -q | sort -u || true)"
if [ -n "${STALE_IDS}" ]; then
    while IFS= read -r id; do
        [ -n "${id}" ] || continue
        docker rmi -f "${id}" >/dev/null 2>&1 || warn "could not remove stale image ${id} (label ${LABEL_FILTER}) -- it may still be referenced by a running container; continuing."
    done <<< "${STALE_IDS}"
else
    log "no stale ${LABEL_FILTER} images found"
fi

# --- 2. Prebuild av-tiles, stripped, via a bind-mounted docker run. ------------------------
# See services/tiles/Dockerfile's own header comment ("Why this Dockerfile has no Rust builder
# stage") for exactly why this step exists and cannot instead be a `RUN` line inside
# `docker build`.
mkdir -p "${BIN_DIR}"
rm -rf "${SCRATCH_TARGET_DIR}"
log "prebuilding av-tiles (release, stripped) via ${PREBUILD_BASE_IMAGE} with ${SPOORE_ROOT} bind-mounted read-only at ${SPOORE_CONTAINER_PATH} (the sibling of ${CONTAINER_WORKSPACE}, question 219(b)/(c)) -- this is the one permitted network window (question 154): apt packages inside the prebuild container, plus the base images themselves if not already local"
docker run --rm \
    -v "${REPO_ROOT}:${CONTAINER_WORKSPACE}" \
    -v "${SPOORE_ROOT}:${SPOORE_CONTAINER_PATH}:ro" \
    -w "${CONTAINER_WORKSPACE}" \
    "${PREBUILD_BASE_IMAGE}" \
    bash -c 'set -euo pipefail
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null
        cargo build --release -p av-tiles --bin av-tiles --target-dir target-docker-linux
        strip target-docker-linux/release/av-tiles' \
    1>&2

[ -f "${SCRATCH_TARGET_DIR}/release/av-tiles" ] || die "prebuild finished but ${SCRATCH_TARGET_DIR}/release/av-tiles does not exist"
cp "${SCRATCH_TARGET_DIR}/release/av-tiles" "${BIN_PATH}"
chmod +x "${BIN_PATH}"
BIN_SHA256="$(shasum -a 256 "${BIN_PATH}" | awk '{print $1}')"
log "prebuilt binary: ${BIN_PATH} (sha256:${BIN_SHA256}, $(du -h "${BIN_PATH}" | awk '{print $1}'))"
rm -rf "${SCRATCH_TARGET_DIR}"

# --- 3. Build the runtime image, once. -----------------------------------------------------
log "building ${IMAGE_TAG} from ${DOCKERFILE} (context: ${REPO_ROOT})"
docker build -f "${DOCKERFILE}" -t "${IMAGE_TAG}" "${REPO_ROOT}" 1>&2

IMAGE_ID="$(docker image inspect "${IMAGE_TAG}" --format '{{.Id}}')"
[ -n "${IMAGE_ID}" ] || die "docker image inspect returned an empty .Id for ${IMAGE_TAG}"
log "built image ID: ${IMAGE_ID}"

# Parsed from the Dockerfile itself (not hardcoded here) so this can never silently drift out
# of sync with a future re-pin -- mirrors services/edge-plugin/build-image.sh's own identical
# COPY-parsing philosophy, applied to the one FROM line this Dockerfile has.
RUNTIME_BASE_IMAGE="$(grep -E '^FROM[[:space:]]' "${DOCKERFILE}" | head -1 | awk '{print $2}')"
[ -n "${RUNTIME_BASE_IMAGE}" ] || die "could not parse a FROM line out of ${DOCKERFILE}"
case "${RUNTIME_BASE_IMAGE}" in
    *@sha256:*) ;;
    *) die "the Dockerfile's FROM line (${RUNTIME_BASE_IMAGE}) is not pinned by digest -- question 154/185's own rule: every base image is pinned by content digest, never a floating tag." ;;
esac

# --- 3b. Question 232 (extending 212(a)): the commit HEAD points at, and whether the tree was
# dirty UNDER THIS IMAGE'S OWN COPIED PATHS (never the whole tree -- dirt elsewhere is not this
# image's provenance concern) at build time. Read from COPIED_PATHS_FILE -- the ONE list this
# script and tests/heavy_stack.py's own verify_image_commit_provenance both read, never
# duplicated. bash 3.2 on this host (macOS's own /bin/bash) has no `mapfile`/`readarray`, so a
# plain `while read` loop into an array, matching this script's own `STALE_IDS` loop above.
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
    warn "working tree is dirty under ${DIRTY_COUNT} of this image's own copied path(s) at build time -- IMAGE_DIGEST.md will record this honestly, and tests/heavy_stack.py's own commit-provenance verifier (question 232) will refuse to trust this build until it is rebuilt from a clean tree."
fi

# --- 4. Record the digests, the prebuilt binary's own hash, and this run's duration. -------
# Question 212(a): the recorded digest below has exactly one home -- this file, generated by
# this script alone, never hand-edited, and never duplicated into a second record. Every
# consumer (tests/heavy_stack.py's own `_parse_tiles_image_digest_md`) parses this SAME file.
DURATION_S=$(( SECONDS - START_SECONDS ))
{
    printf '# services/tiles/IMAGE_DIGEST.md -- generated by services/tiles/build-image.sh\n'
    printf '# Do not hand-edit; re-run the script to regenerate.\n\n'
    printf '## %s (most recent build)\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Image tag: `%s`\n' "${IMAGE_TAG}"
    printf -- '- Image ID (docker image inspect --format {{.Id}}):\n'
    printf '```\n%s\n```\n' "${IMAGE_ID}"
    printf -- '- Runtime base image, pinned by digest (this Dockerfile'\''s own FROM line):\n'
    printf '```\n%s\n```\n' "${RUNTIME_BASE_IMAGE}"
    printf -- '- Prebuild base image, pinned by digest (used only by this script'\''s own bind-mounted `docker run`, never by the Dockerfile itself):\n'
    printf '```\n%s\n```\n' "${PREBUILD_BASE_IMAGE}"
    printf -- '- Prebuilt `av-tiles` binary SHA-256 (%s):\n' "${BIN_PATH#"${REPO_ROOT}"/}"
    printf '```\nsha256:%s\n```\n' "${BIN_SHA256}"
    printf -- '- Built from commit (question 232, extending question 212(a) -- the paths this covers are services/tiles/IMAGE_COPIED_PATHS.txt):\n'
    if [ "${DIRTY_COUNT}" -gt 0 ]; then
        printf '```\n%s (working tree dirty: %s modified paths under the copied paths)\n```\n' "${BUILD_COMMIT}" "${DIRTY_COUNT}"
    else
        printf '```\n%s\n```\n' "${BUILD_COMMIT}"
    fi
    printf -- '- Build duration (this run, prebuild + image build): %ds\n' "${DURATION_S}"
    printf -- '- Rebuild with: `services/tiles/build-image.sh`\n'
} > "${DIGEST_DOC}.new"
mv "${DIGEST_DOC}.new" "${DIGEST_DOC}"
log "wrote ${DIGEST_DOC}"

stop_events_capture

log "done in ${DURATION_S}s. Image ID: ${IMAGE_ID}"
log "docker events for this run's build window (question 194): ${EVENTS_LOG}"
printf 'image_id=%s\n' "${IMAGE_ID}"
printf 'runtime_base_image=%s\n' "${RUNTIME_BASE_IMAGE}"
printf 'prebuild_base_image=%s\n' "${PREBUILD_BASE_IMAGE}"
printf 'prebuilt_binary_sha256=%s\n' "${BIN_SHA256}"
printf 'build_duration_s=%s\n' "${DURATION_S}"
