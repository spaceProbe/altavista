#!/usr/bin/env bash
# services/cfs/build-image.sh -- documented, ONE-TIME build of the cFS lockstep image
# (docs/open-questions.md question 179).
#
# What this does, in order:
#   1. Builds `altavista-cfs-lockstep:local` from services/cfs/Dockerfile, from the repository
#      root, exactly once. This is the one network window question 154 permits (apt packages,
#      and third_party/fetch-cfs.sh's pinned cFS clone, both inside the Dockerfile) -- nothing
#      else in this repo's test/run path touches the network.
#   2. Reads back the built image's content-addressed ID (`docker image inspect --format
#      '{{.Id}}'`) and prints it.
#   3. Writes services/cfs/IMAGE_CONTEXT_MANIFEST.txt: every host path the Dockerfile's COPY
#      steps read from (a COPYed directory expands to every file under it, recursively), each
#      with its SHA-256, one entry per line as `<sha256>  <path>`, sorted by path. This is what
#      services/cfs/tests/test_image_digest.py diffs against on a future digest mismatch, so
#      drift is attributable to specific files instead of merely detected.
#
#      A COPYed path that `git check-ignore` reports as ignored (docs/open-questions.md
#      question 179's unindexed amendment, dated 2026-09-08: `services/cfs/bin/av-lockstep-shim`
#      is "a compiled cross-build artifact deliberately not tracked" -- see this Dockerfile's own
#      header comment for the exact rebuild recipe) is a BUILD ARTIFACT: its manifest line gets a
#      third token, `<sha256>  <path>  BUILD_ARTIFACT`, so the manifest self-describes which
#      entries are expected to be absent on a clean checkout that has not run that cross-build
#      step, rather than a hardcoded filename list living a second time in the test file. This is
#      a property of the PATH (is it git-ignored?), not a hardcoded name, so a future build
#      artifact gets the same treatment automatically.
#
# The COPY list is PARSED from services/cfs/Dockerfile itself (not hardcoded here) so the
# manifest cannot silently drift out of sync with the Dockerfile: add/remove/change a COPY line
# and this script picks it up on the next run with no second place to edit. `COPY --from=...`
# lines are skipped -- those copy between build stages inside the image, not from a host path,
# so they cannot appear in a host-path manifest (see services/cfs/Dockerfile line 89, the one
# such line as of this writing: `COPY --from=builder /build/.../cpu1 /cfs/cpu1`).
#
# This script is NEVER invoked by any test. Run it by hand, once, whenever you intend to change
# what's in the image and re-pin services/cfs/IMAGE_DIGEST.md. Safe to run when Docker isn't
# installed or isn't running: it fails loudly before writing anything, and the manifest is only
# ever replaced atomically (written to a temp file, then moved into place) so a failed or
# interrupted run never leaves a half-written manifest on disk.
#
# docs/open-questions.md question 194 (round 6, second half -- the docker-events record): the
# local `altavista-cfs-lockstep:local` tag (and, as of round 6, its own digest-pinned base image)
# has vanished from this host's Docker store twice before with no path to attribution from inside
# this repository -- the daemon's own `docker events` retains only a short in-memory buffer that
# gets flushed within minutes by unrelated container activity on a shared host (measured directly,
# services/cfs/R6_4_REPORT.md section 2), so a *retrospective* `docker events --since` query run
# even a short time after the fact is not reliable. This script therefore starts a LIVE `docker
# events` capture (filtered to `type=image` and `type=container` -- tag/untag/delete/pull/push for
# images, create/start/die/destroy for containers, at minimum) before doing anything else Docker-
# related, and stops it only at the very end (this script's own full execution window, not just
# the `docker build` step) so any interference immediately before/after the build is also caught.
# Written to services/cfs/build/last-build-events.jsonl (this script's own most recent run; not
# accumulated across runs, matching how IMAGE_DIGEST.md's own top section always reflects the
# *current* pin rather than a growing history). A failure to start or write this capture is logged
# as a WARNING and never aborts or fails the build; conversely a real `docker build` failure is
# never hidden by (or attributed to) the events capture -- see the trap-based cleanup below.
#
# Usage:
#   services/cfs/build-image.sh
#
# After it prints the new digest, compare it to services/cfs/IMAGE_DIGEST.md's recorded value
# by hand (`services/cfs/tests/test_image_digest.py` also does this, and on mismatch prints a
# manifest diff against the last-recorded manifest). If the new digest is an intentional
# re-pin, update IMAGE_DIGEST.md's recorded digest and commit the manifest this script just
# wrote alongside it.

# This script is bash (arrays, process substitution below). Under `sh` (bash in POSIX mode on
# macOS) the parser rejects line 230's `<(...)` only when it reaches it -- AFTER the image has
# been built and the runtime-content hash logged, and before the manifest is written -- so a
# `sh services/cfs/build-image.sh` run looked complete but left the manifest stale
# (2026-09-12). Refuse up front instead; bash parses and runs a script command by command, so
# this guard executes before the parser ever sees the process substitution. Bash invoked as
# `sh` still sets BASH_VERSION, so POSIX mode is detected through `shopt -o posix` (and a
# non-bash shell fails the first test, since `shopt` alone is not a POSIX builtin).
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-image.sh: run this with bash (\`bash services/cfs/build-image.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="altavista-cfs-lockstep:local"
MANIFEST_PATH="${SCRIPT_DIR}/IMAGE_CONTEXT_MANIFEST.txt"
# docs/open-questions.md question 194 (round 6): this script's own live `docker events` capture
# for its own execution window, so the next tag/base-image disappearance is attributable. Fixed
# name, overwritten each run -- reflects the MOST RECENT build-image.sh invocation, matching how
# IMAGE_DIGEST.md's own top section always reflects the current pin, not a growing history.
EVENTS_LOG="${SCRIPT_DIR}/build/last-build-events.jsonl"

log() { printf '[build-image] %s\n' "$1" >&2; }
warn() { printf '[build-image] WARNING: %s\n' "$1" >&2; }
die() { printf '[build-image] ERROR: %s\n' "$1" >&2; exit 1; }

# --- Docker-events capture (question 194) + temp-manifest cleanup, one combined EXIT trap so
# neither cleanup step can clobber the other (a second `trap ... EXIT` call replaces the first,
# so this script uses exactly one, set once, early, and updated only through these two globals). -
EVENTS_PID=""
TMP_MANIFEST=""

start_events_capture() {
    # Never fatal: a broken/unavailable events capture must not fail the build (question 194's own
    # wording). Every failure path here warns and returns, leaving EVENTS_PID empty.
    if ! mkdir -p "$(dirname "${EVENTS_LOG}")" 2>/dev/null; then
        warn "could not create $(dirname "${EVENTS_LOG}") -- continuing without a docker events capture for this build."
        return
    fi
    if ! : > "${EVENTS_LOG}.tmp" 2>/dev/null; then
        warn "could not create ${EVENTS_LOG}.tmp -- continuing without a docker events capture for this build."
        return
    fi
    # NOT piped through a filter here, deliberately: `$!` on a pipeline is the LAST command's pid,
    # so `stop_events_capture`'s own `kill` would reap the filter and leave `docker events` itself
    # running as an orphan until its next write got SIGPIPE. The raw stream is captured by one
    # process, and the noise exclusion happens in `stop_events_capture` when `.tmp` becomes the
    # final log (which also keeps the unfiltered capture on disk until that moment).
    docker events --format '{{json .}}' --filter type=image --filter type=container >> "${EVENTS_LOG}.tmp" 2>&1 &
    EVENTS_PID=$!
    sleep 0.3
    if ! kill -0 "${EVENTS_PID}" 2>/dev/null; then
        warn "docker events process exited immediately -- continuing without a docker events capture for this build."
        EVENTS_PID=""
        return
    fi
    log "docker events capture started (pid ${EVENTS_PID}); will write ${EVENTS_LOG} (type=image, type=container: tag/untag/delete/pull/push, create/start/die/destroy; unrelated containers' exec_*/health_status noise dropped when the log is finalised, see stop_events_capture)"
}

# Actions dropped from the finalised log. **Measured, not assumed** (the manager, round 6's re-pin
# build): 72 of the 93 captured lines -- 77% -- were `exec_create`/`exec_start`/`exec_die` from an
# unrelated, always-on container stack's health checks on this shared host, burying the handful of
# image events this capture exists to preserve at roughly 4:1. None of these actions can attribute
# a vanished tag: an exec inside an already-running container, or a health-status transition,
# cannot tag, untag or delete an image. Image events are never dropped, whatever their action.
EVENTS_NOISE_ACTIONS='"Action":"(exec_create|exec_start|exec_die|health_status)'

stop_events_capture() {
    [ -n "${EVENTS_PID}" ] || return 0
    if kill -0 "${EVENTS_PID}" 2>/dev/null; then
        kill "${EVENTS_PID}" 2>/dev/null || true
        wait "${EVENTS_PID}" 2>/dev/null || true
    fi
    EVENTS_PID=""
    if [ -f "${EVENTS_LOG}.tmp" ]; then
        # `grep -v` exits 1 when it matches nothing, which is a perfectly ordinary outcome for a
        # quiet build window; `|| true` keeps that from tripping `set -e`. A grep failure of any
        # kind falls back to the raw capture rather than losing it.
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
    if [ -n "${TMP_MANIFEST}" ] && [ -f "${TMP_MANIFEST}" ]; then
        rm -f "${TMP_MANIFEST}"
    fi
    # `exit`, not `return`: a parse error or `die` must reach the caller as non-zero even
    # after this trap has run its cleanup commands (question 148: an exit code is not evidence,
    # but a wrong exit code is worse than none).
    exit "${status}"
}
trap _cleanup_on_exit EXIT

# --- 0. Preconditions: fail loudly, before doing anything, if Docker is not usable. ----------
command -v docker >/dev/null 2>&1 || die "docker binary not found on PATH -- install/start Docker before running this script."
if ! docker info >/dev/null 2>&1; then
    die "\`docker info\` failed -- Docker is not running (or not accessible). Start Docker Desktop (or the daemon) and re-run."
fi
command -v git >/dev/null 2>&1 || die "git binary not found on PATH -- needed to mark build-artifact manifest entries (git check-ignore)."
[ -f "${DOCKERFILE}" ] || die "Dockerfile not found at ${DOCKERFILE}"

# --- 0b. Start the docker-events capture (question 194) now that Docker is confirmed usable, and
# before the build itself -- this script's own full execution window, not just the build call, so
# interference immediately before/after the build is also caught (both prior disappearances this
# repository recorded happened close to, but not necessarily during, an actual build). -----------
start_events_capture

# --- 1. Build the image, once. ----------------------------------------------------------------
log "building ${IMAGE_TAG} from ${DOCKERFILE} (context: ${REPO_ROOT}) -- this is the one permitted network window (question 154)"
docker build -f "${DOCKERFILE}" -t "${IMAGE_TAG}" "${REPO_ROOT}" 1>&2

# --- 2. Read back the content-addressed image ID. ----------------------------------------------
IMAGE_ID="$(docker image inspect "${IMAGE_TAG}" --format '{{.Id}}')"
[ -n "${IMAGE_ID}" ] || die "docker image inspect returned an empty .Id for ${IMAGE_TAG}"
log "built image ID: ${IMAGE_ID}"

# --- 2b. Compute the runtime-content hash (docs/open-questions.md question 185): SHA-256 over
# the sorted "<path> <sha256>" lines for every file this image actually ships and runs --
# /cfs/av-lockstep-shim, /cfs/container-entrypoint.sh, and every file under /cfs/cpu1 (after
# services/cfs/build/targets.cmake's own unit-tests-off fix, that directory should hold nothing
# but runtime-relevant artifacts -- see services/cfs/IMAGE_DIGEST.md's own "Runtime-content hash"
# section for the canonical definition this mirrors, and services/cfs/tests/
# test_image_reproducibility.py's `runtime_content_hash()` for the independent Python
# re-implementation of this exact algorithm -- question 164's captured-artifact precedent for
# keeping two implementations of one defined algorithm separate).
log "computing runtime-content hash (question 185)"
RUNTIME_CONTENT_HASH="$(docker run --rm --entrypoint sh "${IMAGE_TAG}" -c \
    'find /cfs/cpu1 -type f -exec sha256sum {} + ; sha256sum /cfs/av-lockstep-shim /cfs/container-entrypoint.sh' \
    | awk '{print $2, $1}' \
    | LC_ALL=C sort \
    | shasum -a 256 | awk '{print $1}')"
[ -n "${RUNTIME_CONTENT_HASH}" ] || die "runtime-content hash computation produced no output"
log "runtime-content hash: sha256:${RUNTIME_CONTENT_HASH}"

# --- 3. Parse the Dockerfile's COPY list (skip --from=... intra-image copies). -----------------
# Each surviving COPY line has the form (after collapsing whitespace):
#   COPY <src> <dst>
# `src` is relative to the build context (the repo root, per this Dockerfile's own header
# comment and the `docker build ... <repo root>` invocation above).
declare -a COPY_SRCS=()
while IFS= read -r line; do
    # Strip the leading "COPY" keyword and surrounding whitespace.
    rest="${line#COPY}"
    # shellcheck disable=SC2206 # intentional word-splitting: Dockerfile COPY args are whitespace-separated tokens
    tokens=(${rest})
    if [ "${#tokens[@]}" -lt 2 ]; then
        die "could not parse COPY line (expected at least 2 tokens after COPY): ${line}"
    fi
    first="${tokens[0]}"
    case "${first}" in
        --from=*)
            log "skipping intra-image copy (not a host path): ${line}"
            continue
            ;;
        --*)
            die "unrecognized COPY flag '${first}' in line: ${line} -- extend this parser before proceeding (do not guess)."
            ;;
    esac
    if [ "${#tokens[@]}" -ne 2 ]; then
        die "COPY line has an unexpected shape (expected exactly 'src dst'): ${line}"
    fi
    COPY_SRCS+=("${first}")
done < <(grep -E '^COPY[[:space:]]' "${DOCKERFILE}")

[ "${#COPY_SRCS[@]}" -gt 0 ] || die "parsed zero host COPY paths out of ${DOCKERFILE} -- Dockerfile format changed unexpectedly, refusing to write an empty manifest."

log "parsed ${#COPY_SRCS[@]} host COPY source path(s) from ${DOCKERFILE}"

# --- 4. Expand each COPY src to its full file list (directories expand recursively), hash each. -
# (TMP_MANIFEST is the global declared above; its cleanup is handled by the single combined
# _cleanup_on_exit EXIT trap set near the top of this script, alongside the events-capture
# cleanup -- no separate trap here, so neither cleanup step can clobber the other's trap.)
TMP_MANIFEST="$(mktemp "${MANIFEST_PATH}.XXXXXX")"

# A manifest entry is a "build artifact" iff git itself reports the path as ignored -- this is
# the exact property that distinguishes services/cfs/bin/av-lockstep-shim (`.gitignore` line 35)
# from every other COPYed path, and it generalizes to any future build artifact without a second
# hardcoded filename list here or in the test.
manifest_line() {
    local sha="$1" rel="$2"
    if git -C "${REPO_ROOT}" check-ignore -q -- "${rel}"; then
        printf '%s  %s  BUILD_ARTIFACT\n' "${sha}" "${rel}"
    else
        printf '%s  %s\n' "${sha}" "${rel}"
    fi
}

for src in "${COPY_SRCS[@]}"; do
    abs_path="${REPO_ROOT}/${src}"
    if [ -f "${abs_path}" ]; then
        sha="$(shasum -a 256 "${abs_path}" | awk '{print $1}')"
        manifest_line "${sha}" "${src}" >> "${TMP_MANIFEST}"
    elif [ -d "${abs_path}" ]; then
        # Recursively list every file under the directory, deterministically ordered.
        while IFS= read -r -d '' file; do
            rel="${file#"${REPO_ROOT}"/}"
            sha="$(shasum -a 256 "${file}" | awk '{print $1}')"
            manifest_line "${sha}" "${rel}" >> "${TMP_MANIFEST}"
        done < <(find "${abs_path}" -type f -print0 | sort -z)
    else
        die "COPY source path does not exist on disk: ${src} (resolved to ${abs_path})"
    fi
done

sort -k2 -o "${TMP_MANIFEST}" "${TMP_MANIFEST}"

{
    printf '# services/cfs/IMAGE_CONTEXT_MANIFEST.txt -- generated by services/cfs/build-image.sh\n'
    printf '# One line per host file the Dockerfile'\''s COPY steps read from (directories expanded\n'
    printf '# recursively), sorted by path: "<sha256>  <path>" (path relative to the repository root),\n'
    printf '# or "<sha256>  <path>  BUILD_ARTIFACT" when git itself reports <path> as ignored (a\n'
    printf '# compiled, not-tracked build artifact -- e.g. services/cfs/bin/av-lockstep-shim; see this\n'
    printf '# Dockerfile'\''s own header comment for the rebuild recipe). test_manifest_paths_exist_and_\n'
    printf '# hash_match verifies a BUILD_ARTIFACT entry only when the file is present on disk and\n'
    printf '# skips it visibly, by name, when absent; every other entry is verified strictly.\n'
    printf '# Built image ID for this manifest: %s\n' "${IMAGE_ID}"
    printf '# Runtime-content hash for this build (question 185, see IMAGE_DIGEST.md): sha256:%s\n' "${RUNTIME_CONTENT_HASH}"
    printf '# Regenerate with: services/cfs/build-image.sh -- do not hand-edit.\n'
    cat "${TMP_MANIFEST}"
} > "${MANIFEST_PATH}.new"
mv "${MANIFEST_PATH}.new" "${MANIFEST_PATH}"
rm -f "${TMP_MANIFEST}"
TMP_MANIFEST=""

# --- 5. Stop the docker-events capture (question 194) now that all Docker work is done, so its
# own log line appears alongside the rest of this script's summary output rather than only via
# the EXIT trap (which still covers every early/failure exit above as a safety net). -------------
stop_events_capture

ENTRY_COUNT="$(grep -vc '^#' "${MANIFEST_PATH}")"
log "wrote ${MANIFEST_PATH} (${ENTRY_COUNT} file entries)"
log "done. Image ID: ${IMAGE_ID}"
log "docker events for this run's build window (question 194): ${EVENTS_LOG} (if a WARNING above said the capture didn't start, this file may be absent or stale -- that WARNING is the honest record for that run, not a silent gap)"
log "record both of the following in services/cfs/IMAGE_DIGEST.md's new dated section (by hand, same as always -- this script never edits that file):"
printf 'image_id=%s\n' "${IMAGE_ID}"
printf 'runtime_content_hash=sha256:%s\n' "${RUNTIME_CONTENT_HASH}"
