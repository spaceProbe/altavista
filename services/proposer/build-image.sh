#!/usr/bin/env bash
# services/proposer/build-image.sh -- the single documented way to (re)create
# `av-proposer:local` (R3.3, docs/aiplane-plan.md milestone A4; docs/open-questions.md
# question 206's decision 11(a) -- modelled on services/edge-plugin/build-image.sh almost
# line for line, adapted crate/binary names and the extra spoore host-path dependency
# services/proposer/Dockerfile's own header comment names).
#
# What this does, in order:
#   1. Preconditions: docker installed and running, git present, the Dockerfile present,
#      the real committed fixture present, and /Users/probe/code/spoore present (the
#      cross-build below needs it bind-mounted -- see the Dockerfile's own header comment for
#      why: av-proposer's spoore-cdm AND spoore-models path dependencies, plus its own
#      build.rs's direct read of spoore's model_service.proto).
#   2. Starts a live `docker events` capture for this script's own full execution window
#      (question 194's round-6 amendment -- verbatim precedent from services/edge-plugin/
#      build-image.sh, adapted paths only).
#   3. Prunes by label (question 156: prune before creating) -- removes any image already
#      carrying `label=org.altavista.component=proposer` (a dangling image a previous run's
#      now-superseded `av-proposer:local` tag left behind; the currently-tagged image, if
#      any, is about to be replaced by step 5 regardless).
#   4. Prebuilds `av-proposer`, stripped, via a real `docker run` bind-mounting BOTH this
#      repository and /Users/probe/code/spoore (read-only) into a pinned `rust:1.90-bookworm`
#      container (the same digest services/edge-plugin/build-image.sh now pins --
#      see services/proposer/Dockerfile's own header comment, "Why this Dockerfile has no Rust
#      builder stage", for the measured regorus/const_vec_string_slice reason a floor of 1.85
#      does not compile this crate's own dependency graph) -- see that Dockerfile's own header
#      comment for the exact command and why a plain `docker build` cannot do this. Writes
#      services/proposer/bin/av-proposer (git-ignored, a build artifact -- services/cfs/
#      bin/av-lockstep-shim's and services/edge-plugin/bin/av-edge-plugin's own precedent).
#   5. Verifies the prebuilt binary's own runtime linkage against the SAME `debian:
#      bookworm-slim` digest the Dockerfile pins (never assumed to be identical to the edge
#      plugin's own prior measurement, even though the base image is the same tag+digest --
#      this is a different binary, checked directly).
#   6. Builds `av-proposer:local` from services/proposer/Dockerfile, from the repository root.
#   7. Reads back the built image's content-addressed ID and this Dockerfile's own pinned
#      base image reference, and writes both -- plus the prebuild base image reference, the
#      prebuilt binary's own SHA-256, and this run's build duration -- to services/proposer/
#      IMAGE_DIGEST.md.
#   8. Stops the docker-events capture, writing services/proposer/build/
#      last-build-events.jsonl (question 194's own attribution record).
#
# This script is NEVER invoked by any test (services/edge-plugin/build-image.sh's own rule,
# verbatim) -- tests/test_proposer_container.py only ever inspects an already-built image and
# skips VISIBLY, naming this script, when it is absent (question 194).
#
# Idempotent and re-runnable: every artifact this script writes (the prebuilt binary, the
# image tag, IMAGE_DIGEST.md, the events log) is fully overwritten on each run, and step 3's
# prune means a re-run never accumulates dangling images under this component's label.
#
# Usage:
#   services/proposer/build-image.sh

# Same bash-only guard as services/edge-plugin/build-image.sh, and for the identical reason:
# this script uses bash arrays and process substitution, which a POSIX-mode `sh` invocation
# parses lazily and can fail on partway through a run, after some Docker side effects have
# already happened. Refuse up front instead.
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-image.sh: run this with bash (\`bash services/proposer/build-image.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="av-proposer:local"
LABEL_FILTER="label=org.altavista.component=proposer"
BIN_DIR="${SCRIPT_DIR}/bin"
BIN_PATH="${BIN_DIR}/av-proposer"
DIGEST_DOC="${SCRIPT_DIR}/IMAGE_DIGEST.md"
EVENTS_LOG="${SCRIPT_DIR}/build/last-build-events.jsonl"
SPOORE_HOST_PATH="/Users/probe/code/spoore"
FIXTURE_PATH="${REPO_ROOT}/tests/fixtures/demo_two_instance.runproducts.bin"
# Pinned prebuild base -- NEWER than services/edge-plugin/build-image.sh's own
# `rust:1.85-bookworm` pin (R5.3 moved that script to this same 1.90 digest, because the
# corrected floor makes a 1.85 container refuse to build any crate here), and still newer
# than this workspace's own corrected
# `rust-version = "1.87"` (question 208(a) -- the root `Cargo.toml` used to say "1.85", which
# was never actually true; measured, not guessed). See the Dockerfile's own header comment
# ("Why this Dockerfile has no Rust builder stage") for the measured reason: av-proposer's
# dependency graph reaches av-command -> regorus 0.12.0, which needs `const_vec_string_slice`
# (Vec::len/is_empty/as_slice as const fn) -- confirmed by binary search on this host:
# 1.86.0 fails with the same error a real 1.85 build hit, 1.87.0 passes. rustc 1.90.0
# (confirmed: `docker run --rm <image> rustc --version`) compiles it cleanly, comfortably
# above either floor.
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
[ -f "${FIXTURE_PATH}" ] || die "committed fixture not found at ${FIXTURE_PATH} -- this Dockerfile bakes it in for provenance (see its own header comment, 'Self-contained and offline')."
[ -d "${SPOORE_HOST_PATH}/crates/spoore-cdm" ] || die "${SPOORE_HOST_PATH}/crates/spoore-cdm not found -- the prebuild step below bind-mounts this exact path (see the Dockerfile's own header comment for why: av-proposer's spoore-cdm AND spoore-models path dependencies, and its own build.rs's direct read of spoore's model_service.proto)."
[ -d "${SPOORE_HOST_PATH}/crates/spoore-models" ] || die "${SPOORE_HOST_PATH}/crates/spoore-models not found -- av-proposer's own D2 dependency (a real spoore_models::KalmanFilter, decision 11(b)), the second of the two spoore path dependencies this crate has and av-edge-plugin never did."
[ -f "${SPOORE_HOST_PATH}/proto/spoore/v0/model_service.proto" ] || die "${SPOORE_HOST_PATH}/proto/spoore/v0/model_service.proto not found -- av-proposer's own build.rs reads this file directly at compile time (see that file's own module doc)."

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

# --- 2. Prebuild av-proposer, stripped, via a bind-mounted docker run. --------------------
# See services/proposer/Dockerfile's own header comment ("Why this Dockerfile has no Rust
# builder stage") for exactly why this step exists and cannot instead be a `RUN` line inside
# `docker build`.
mkdir -p "${BIN_DIR}"
rm -rf "${SCRATCH_TARGET_DIR}"
log "prebuilding av-proposer (release, stripped) via ${PREBUILD_BASE_IMAGE} with ${SPOORE_HOST_PATH} bind-mounted read-only -- this is the one permitted network window (question 154): apt packages inside the prebuild container"
docker run --rm \
    -v "${REPO_ROOT}:/workspace" \
    -v "${SPOORE_HOST_PATH}:${SPOORE_HOST_PATH}:ro" \
    -w /workspace \
    "${PREBUILD_BASE_IMAGE}" \
    bash -c 'set -euo pipefail
        # SUPERSEDED (question 211, the lead, 2026-09-15): this comment used to claim a
        # KNOWN, UNRESOLVED HOST DEFECT (a GPG-signature story) and carried an unconfirmed
        # sed rewriting apt sources to https. Re-tested on this host before this fix was
        # written, not carried forward on faith: this PREBUILD base
        # (rust:1.90-bookworm@sha256:3914072ca...) already ships ca-certificates (measured:
        # docker run --rm IMAGE dpkg -l | grep ca-certificates -> installed;
        # ls /etc/ssl/certs | wc -l -> 285 real certificates), so an http-vs-https rewrite
        # changes nothing here -- apt trusts the archive via the GPG-signed Release file
        # (debian-archive-keyring), never via TLS server certificates. No GPG/apt-key/gpgv
        # failure was reproduced against this base image on this host; that old claim is
        # deleted as superseded, not re-asserted. The sed itself is dropped -- see
        # services/proposer/Dockerfile own header comment for the fuller writeup, including
        # the ONE place the failure actually was: the RUNTIME stage own debian:bookworm-slim
        # base, which is a different image with no ca-certificates at all, not this one.
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null
        cargo build --release -p av-proposer --bin av-proposer --target-dir /workspace/target-docker-linux
        strip /workspace/target-docker-linux/release/av-proposer' \
    1>&2

[ -f "${SCRATCH_TARGET_DIR}/release/av-proposer" ] || die "prebuild finished but ${SCRATCH_TARGET_DIR}/release/av-proposer does not exist"
cp "${SCRATCH_TARGET_DIR}/release/av-proposer" "${BIN_PATH}"
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
# of sync with a future re-pin -- mirrors services/edge-plugin/build-image.sh's own
# COPY-parsing philosophy, applied to the one FROM line this Dockerfile has.
RUNTIME_BASE_IMAGE="$(grep -E '^FROM[[:space:]]' "${DOCKERFILE}" | head -1 | awk '{print $2}')"
[ -n "${RUNTIME_BASE_IMAGE}" ] || die "could not parse a FROM line out of ${DOCKERFILE}"
case "${RUNTIME_BASE_IMAGE}" in
    *@sha256:*) ;;
    *) die "the Dockerfile's FROM line (${RUNTIME_BASE_IMAGE}) is not pinned by digest -- question 154/185's own rule: every base image is pinned by content digest, never a floating tag." ;;
esac

# --- 4. Verify the prebuilt binary's own runtime linkage against this exact runtime base --
# (never assumed to be identical to services/edge-plugin/Dockerfile's own prior `ldd`
# measurement, even though the base image tag+digest is the same one -- this is a different
# binary, checked directly). A missing .so here means this Dockerfile's own `apt-get install`
# list (currently: libssl3 only) needs to grow; caught here, at build time, rather than
# discovered later as a container that starts and immediately exits.
log "verifying ${IMAGE_TAG}'s own runtime linkage for the prebuilt av-proposer binary (ldd, inside the built image)"
LDD_OUTPUT="$(docker run --rm --entrypoint /usr/bin/ldd "${IMAGE_TAG}" /usr/local/bin/av-proposer 2>&1 || true)"
if echo "${LDD_OUTPUT}" | grep -q "not found"; then
    die "av-proposer:local's own base image is missing a shared library this binary needs -- ldd output:\n${LDD_OUTPUT}"
fi
log "ldd: every shared library av-proposer needs resolves inside ${IMAGE_TAG}"

# --- 5. Record the digests, the prebuilt binary's own hash, and this run's duration. -------
DURATION_S=$(( SECONDS - START_SECONDS ))
{
    printf '# services/proposer/IMAGE_DIGEST.md -- generated by services/proposer/build-image.sh\n'
    printf '# Do not hand-edit; re-run the script to regenerate.\n\n'
    printf '## %s (most recent build)\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Image tag: `%s`\n' "${IMAGE_TAG}"
    printf -- '- Image ID (docker image inspect --format {{.Id}}):\n'
    printf '```\n%s\n```\n' "${IMAGE_ID}"
    printf -- '- Runtime base image, pinned by digest (this Dockerfile'\''s own FROM line):\n'
    printf '```\n%s\n```\n' "${RUNTIME_BASE_IMAGE}"
    printf -- '- Prebuild base image, pinned by digest (used only by this script'\''s own bind-mounted `docker run`, never by the Dockerfile itself):\n'
    printf '```\n%s\n```\n' "${PREBUILD_BASE_IMAGE}"
    printf -- '- Prebuilt `av-proposer` binary SHA-256 (%s):\n' "${BIN_PATH#"${REPO_ROOT}"/}"
    printf '```\nsha256:%s\n```\n' "${BIN_SHA256}"
    printf -- '- ldd verification against this exact image (step 4):\n'
    printf '```\n%s\n```\n' "${LDD_OUTPUT}"
    printf -- '- Build duration (this run, prebuild + image build + ldd verification): %ds\n' "${DURATION_S}"
    printf -- '- Rebuild with: `services/proposer/build-image.sh`\n'
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
