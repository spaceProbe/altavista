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
# Usage:
#   services/cfs/build-image.sh
#
# After it prints the new digest, compare it to services/cfs/IMAGE_DIGEST.md's recorded value
# by hand (`services/cfs/tests/test_image_digest.py` also does this, and on mismatch prints a
# manifest diff against the last-recorded manifest). If the new digest is an intentional
# re-pin, update IMAGE_DIGEST.md's recorded digest and commit the manifest this script just
# wrote alongside it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
IMAGE_TAG="altavista-cfs-lockstep:local"
MANIFEST_PATH="${SCRIPT_DIR}/IMAGE_CONTEXT_MANIFEST.txt"

log() { printf '[build-image] %s\n' "$1" >&2; }
die() { printf '[build-image] ERROR: %s\n' "$1" >&2; exit 1; }

# --- 0. Preconditions: fail loudly, before doing anything, if Docker is not usable. ----------
command -v docker >/dev/null 2>&1 || die "docker binary not found on PATH -- install/start Docker before running this script."
if ! docker info >/dev/null 2>&1; then
    die "\`docker info\` failed -- Docker is not running (or not accessible). Start Docker Desktop (or the daemon) and re-run."
fi
[ -f "${DOCKERFILE}" ] || die "Dockerfile not found at ${DOCKERFILE}"

# --- 1. Build the image, once. ----------------------------------------------------------------
log "building ${IMAGE_TAG} from ${DOCKERFILE} (context: ${REPO_ROOT}) -- this is the one permitted network window (question 154)"
docker build -f "${DOCKERFILE}" -t "${IMAGE_TAG}" "${REPO_ROOT}" 1>&2

# --- 2. Read back the content-addressed image ID. ----------------------------------------------
IMAGE_ID="$(docker image inspect "${IMAGE_TAG}" --format '{{.Id}}')"
[ -n "${IMAGE_ID}" ] || die "docker image inspect returned an empty .Id for ${IMAGE_TAG}"
log "built image ID: ${IMAGE_ID}"

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
TMP_MANIFEST="$(mktemp "${MANIFEST_PATH}.XXXXXX")"
trap 'rm -f "${TMP_MANIFEST}"' EXIT

for src in "${COPY_SRCS[@]}"; do
    abs_path="${REPO_ROOT}/${src}"
    if [ -f "${abs_path}" ]; then
        sha="$(shasum -a 256 "${abs_path}" | awk '{print $1}')"
        printf '%s  %s\n' "${sha}" "${src}" >> "${TMP_MANIFEST}"
    elif [ -d "${abs_path}" ]; then
        # Recursively list every file under the directory, deterministically ordered.
        while IFS= read -r -d '' file; do
            rel="${file#"${REPO_ROOT}"/}"
            sha="$(shasum -a 256 "${file}" | awk '{print $1}')"
            printf '%s  %s\n' "${sha}" "${rel}" >> "${TMP_MANIFEST}"
        done < <(find "${abs_path}" -type f -print0 | sort -z)
    else
        die "COPY source path does not exist on disk: ${src} (resolved to ${abs_path})"
    fi
done

sort -k2 -o "${TMP_MANIFEST}" "${TMP_MANIFEST}"

{
    printf '# services/cfs/IMAGE_CONTEXT_MANIFEST.txt -- generated by services/cfs/build-image.sh\n'
    printf '# One line per host file the Dockerfile'\''s COPY steps read from (directories expanded\n'
    printf '# recursively), sorted by path: "<sha256>  <path>" (path relative to the repository root).\n'
    printf '# Built image ID for this manifest: %s\n' "${IMAGE_ID}"
    printf '# Regenerate with: services/cfs/build-image.sh -- do not hand-edit.\n'
    cat "${TMP_MANIFEST}"
} > "${MANIFEST_PATH}.new"
mv "${MANIFEST_PATH}.new" "${MANIFEST_PATH}"
trap - EXIT
rm -f "${TMP_MANIFEST}"

ENTRY_COUNT="$(grep -vc '^#' "${MANIFEST_PATH}")"
log "wrote ${MANIFEST_PATH} (${ENTRY_COUNT} file entries)"
log "done. Image ID: ${IMAGE_ID}"
printf '%s\n' "${IMAGE_ID}"
