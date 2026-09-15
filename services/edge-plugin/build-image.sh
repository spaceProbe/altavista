#!/usr/bin/env bash
# services/edge-plugin/build-image.sh -- the single documented way to (re)create
# `av-edge-plugin:local` (E4b, docs/edge-plan.md milestone E4; modelled on
# services/cfs/build-image.sh -- see that script's own header for the precedent this one
# follows almost line for line, and services/edge-plugin/Dockerfile's own header comment
# for exactly why THIS script, not the Dockerfile, has to run the real `cargo build`).
#
# What this does, in order:
#   1. Preconditions: docker installed and running, git present, the Dockerfile present,
#      and /Users/probe/code/spoore present (the cross-build below needs it bind-mounted --
#      see the Dockerfile's header comment for why).
#   2. Starts a live `docker events` capture for this script's own full execution window
#      (question 194's round-6 amendment -- verbatim precedent from services/cfs/
#      build-image.sh, adapted paths only).
#   3. Prunes by label (question 156: prune before creating) -- removes any image already
#      carrying `label=org.altavista.component=edge-plugin` (a dangling image a previous
#      run's now-superseded `av-edge-plugin:local` tag left behind; the currently-tagged
#      image, if any, is about to be replaced by step 5 regardless).
#   4. Prebuilds `av-edge-plugin`, stripped, via a real `docker run` bind-mounting BOTH this
#      repository and /Users/probe/code/spoore (read-only) into a pinned `rust:1.90-bookworm`
#      container -- see services/edge-plugin/Dockerfile's own header comment for the exact
#      command and why a plain `docker build` cannot do this. Writes services/edge-plugin/
#      bin/av-edge-plugin (git-ignored, a build artifact -- services/cfs/bin/av-lockstep-shim's
#      own precedent).
#   5. Builds `av-edge-plugin:local` from services/edge-plugin/Dockerfile, from the
#      repository root.
#   6. Reads back the built image's content-addressed ID and this Dockerfile's own pinned
#      base image reference, and writes both -- plus the prebuild base image reference, the
#      prebuilt binary's own SHA-256, and this run's build duration -- to services/
#      edge-plugin/IMAGE_DIGEST.md.
#   7. Stops the docker-events capture, writing services/edge-plugin/build/
#      last-build-events.jsonl (question 194's own attribution record).
#
# This script is NEVER invoked by any test (services/cfs/build-image.sh's own rule,
# verbatim) -- tests/test_edge_plugin_container.py only ever inspects an already-built
# image and skips VISIBLY, naming this script, when it is absent (question 194).
#
# Idempotent and re-runnable: every artifact this script writes (the prebuilt binary, the
# image tag, IMAGE_DIGEST.md, the events log) is fully overwritten on each run, and step 3's
# prune means a re-run never accumulates dangling images under this component's label.
#
# Usage:
#   services/edge-plugin/build-image.sh

# Same bash-only guard as services/cfs/build-image.sh, and for the identical reason: this
# script uses bash arrays and process substitution, which a POSIX-mode `sh` invocation
# parses lazily and can fail on partway through a run, after some Docker side effects have
# already happened. Refuse up front instead.
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-image.sh: run this with bash (\`bash services/edge-plugin/build-image.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="av-edge-plugin:local"
LABEL_FILTER="label=org.altavista.component=edge-plugin"
BIN_DIR="${SCRIPT_DIR}/bin"
BIN_PATH="${BIN_DIR}/av-edge-plugin"
DIGEST_DOC="${SCRIPT_DIR}/IMAGE_DIGEST.md"
EVENTS_LOG="${SCRIPT_DIR}/build/last-build-events.jsonl"
SPOORE_HOST_PATH="/Users/probe/code/spoore"
# Pinned prebuild base -- see the Dockerfile's own header comment for how this digest was
# resolved. R5.3 (question 208(a)): this was
# `rust:1.85-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4`,
# chosen to match the workspace's DECLARED `rust-version = "1.85"`. That floor was measured
# false this round and corrected to "1.87" (`regorus 0.12.0` needs `const_vec_string_slice`,
# stabilised in 1.87; the 1.86 failure and the 1.87 pass are both recorded beside
# `rust-version` in the root `Cargo.toml`), and Cargo enforces `rust-version` workspace-wide --
# so a 1.85 prebuild container now refuses to build ANY crate here with "rustc 1.85.1 is not
# supported ... requires rustc 1.87". Leaving it would have been a silent breakage: this image
# was already built, its digest is recorded in IMAGE_DIGEST.md, and nothing fails until
# somebody next rebuilds. `rust:1.90-bookworm@sha256:3914072ca...` is the digest
# `services/proposer/build-image.sh` already pins AND the one
# `tests/test_edge_plugin_container.py` already cross-builds THIS crate with for real, so the
# 1.90 toolchain compiling `av-edge-plugin` is measured, not assumed.
# NOTE: `services/edge-plugin/IMAGE_DIGEST.md` still records the 1.85 base, because it is a
# GENERATED record of the last REAL build and is never hand-edited -- the next run of this
# script regenerates it with the digest below.
PREBUILD_BASE_IMAGE="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
SCRATCH_TARGET_DIR="${REPO_ROOT}/target-docker-linux"

log() { printf '[build-image] %s\n' "$1" >&2; }
warn() { printf '[build-image] WARNING: %s\n' "$1" >&2; }
die() { printf '[build-image] ERROR: %s\n' "$1" >&2; exit 1; }

START_SECONDS=$SECONDS

# --- Docker-events capture (question 194), verbatim precedent from services/cfs/
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
    # Not piped through a filter here, deliberately -- see services/cfs/build-image.sh's
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
[ -d "${SPOORE_HOST_PATH}/crates/spoore-cdm" ] || die "${SPOORE_HOST_PATH}/crates/spoore-cdm not found -- the prebuild step below bind-mounts this exact path (see the Dockerfile's own header comment for why: av-cdm's spoore-cdm dependency is an absolute host path, not something a plain \`docker build\` can reach)."

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

# --- 2. Prebuild av-edge-plugin, stripped, via a bind-mounted docker run. ------------------
# See services/edge-plugin/Dockerfile's own header comment ("Why this Dockerfile has no
# Rust builder stage") for exactly why this step exists and cannot instead be a `RUN` line
# inside `docker build`.
mkdir -p "${BIN_DIR}"
rm -rf "${SCRATCH_TARGET_DIR}"
log "prebuilding av-edge-plugin (release, stripped) via ${PREBUILD_BASE_IMAGE} with ${SPOORE_HOST_PATH} bind-mounted read-only -- this is the one permitted network window (question 154): apt packages inside the prebuild container"
docker run --rm \
    -v "${REPO_ROOT}:/workspace" \
    -v "${SPOORE_HOST_PATH}:${SPOORE_HOST_PATH}:ro" \
    -w /workspace \
    "${PREBUILD_BASE_IMAGE}" \
    bash -c 'set -euo pipefail
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null
        cargo build --release -p av-ingest-client --bin av-edge-plugin --target-dir /workspace/target-docker-linux
        strip /workspace/target-docker-linux/release/av-edge-plugin' \
    1>&2

[ -f "${SCRATCH_TARGET_DIR}/release/av-edge-plugin" ] || die "prebuild finished but ${SCRATCH_TARGET_DIR}/release/av-edge-plugin does not exist"
cp "${SCRATCH_TARGET_DIR}/release/av-edge-plugin" "${BIN_PATH}"
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

# Parsed from the Dockerfile itself (not hardcoded here) so this can never silently drift
# out of sync with a future re-pin -- mirrors services/cfs/build-image.sh's own COPY-parsing
# philosophy, applied to the one FROM line this Dockerfile has.
RUNTIME_BASE_IMAGE="$(grep -E '^FROM[[:space:]]' "${DOCKERFILE}" | head -1 | awk '{print $2}')"
[ -n "${RUNTIME_BASE_IMAGE}" ] || die "could not parse a FROM line out of ${DOCKERFILE}"
case "${RUNTIME_BASE_IMAGE}" in
    *@sha256:*) ;;
    *) die "the Dockerfile's FROM line (${RUNTIME_BASE_IMAGE}) is not pinned by digest -- question 154/185's own rule: every base image is pinned by content digest, never a floating tag." ;;
esac

# --- 4. Record the digests, the prebuilt binary's own hash, and this run's duration. -------
DURATION_S=$(( SECONDS - START_SECONDS ))
{
    printf '# services/edge-plugin/IMAGE_DIGEST.md -- generated by services/edge-plugin/build-image.sh\n'
    printf '# Do not hand-edit; re-run the script to regenerate.\n\n'
    printf '## %s (most recent build)\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Image tag: `%s`\n' "${IMAGE_TAG}"
    printf -- '- Image ID (docker image inspect --format {{.Id}}):\n'
    printf '```\n%s\n```\n' "${IMAGE_ID}"
    printf -- '- Runtime base image, pinned by digest (this Dockerfile'\''s own FROM line):\n'
    printf '```\n%s\n```\n' "${RUNTIME_BASE_IMAGE}"
    printf -- '- Prebuild base image, pinned by digest (used only by this script'\''s own bind-mounted `docker run`, never by the Dockerfile itself):\n'
    printf '```\n%s\n```\n' "${PREBUILD_BASE_IMAGE}"
    printf -- '- Prebuilt `av-edge-plugin` binary SHA-256 (%s):\n' "${BIN_PATH#"${REPO_ROOT}"/}"
    printf '```\nsha256:%s\n```\n' "${BIN_SHA256}"
    printf -- '- Build duration (this run, prebuild + image build): %ds\n' "${DURATION_S}"
    printf -- '- Rebuild with: `services/edge-plugin/build-image.sh`\n'
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
