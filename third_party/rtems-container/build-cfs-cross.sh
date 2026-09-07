#!/bin/bash
# M24.3 (docs/open-questions.md questions 144/147/148/154; docs/sil-plan.md M24): cross-build the
# pinned cFS bundle plus the lockstep apps for the RTEMS 6.1 `zynqmp_rpu_lock_step` BSP, inside
# the already-built-and-verified `rtems-m24c:build` container (M24.2c). This script is the
# container ENTRYPOINT override -- it does not build any Docker image itself and creates no new
# tags (question 156); it runs the cFS cross-build against the bind-mounted repo and toolchain.
#
# Expected invocation (from the repository root, on the host):
#
#   docker run --rm \
#     -v "$(pwd)":/workspace \
#     -v "$(pwd)/third_party/rtems-container/output":/output \
#     --entrypoint /workspace/third_party/rtems-container/build-cfs-cross.sh \
#     rtems-m24c:build
#
# `/output` is mounted at the SAME container path the BSP itself was built and verified at
# (M24.2c) -- the toolchain cmake file (services/cfs/build/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake)
# hardcodes RTEMS_TOOLS_PREFIX=/output/toolchain to match exactly, avoiding any risk from the
# toolchain's own internal paths differing from what it was built/verified against.
#
# Network use (question 154): this script installs `cmake` via apt if not already present in the
# `rtems-m24c:build` image (that image was built for the toolchain/BSP task, M24.2c, and never
# needed cmake) -- a one-time, recorded exception, exactly like the image-build-time network use
# `services/cfs/Dockerfile` already documents for the posix build. `third_party/fetch-cfs.sh` is
# run too, but is expected to be a no-op (network-free) here: the host's `third_party/cfs` is
# already fetched and pinned, and mounted straight into the container via `/workspace`.
set -euo pipefail

WORKSPACE="${WORKSPACE:-/workspace}"
CFS_DIR="$WORKSPACE/third_party/cfs"
SVC_CFS="$WORKSPACE/services/cfs"
LOG_DIR="$WORKSPACE/third_party/rtems-container/output/rtems-cross-build-logs"

mkdir -p "$LOG_DIR"

echo "=== [1/8] cmake availability ===" | tee "$LOG_DIR/00-summary.log"
if ! command -v cmake >/dev/null 2>&1; then
    echo "cmake not found in this image -- installing (question 154's one-time network exception)" | tee -a "$LOG_DIR/00-summary.log"
    apt-get update
    apt-get install -y --no-install-recommends cmake
    rm -rf /var/lib/apt/lists/*
fi
cmake --version | tee -a "$LOG_DIR/00-summary.log"

echo "=== [2/8] fetch-cfs.sh (expected no-op: already fetched on host) ===" | tee -a "$LOG_DIR/00-summary.log"
sh "$WORKSPACE/third_party/fetch-cfs.sh" 2>&1 | tee "$LOG_DIR/01-fetch-cfs.log"

echo "=== [3/8] copy lockstep apps + psp into the cFS tree (matches services/cfs/Dockerfile's own COPY steps) ===" | tee -a "$LOG_DIR/00-summary.log"
rm -rf "$CFS_DIR/apps/io_lockstep" "$CFS_DIR/apps/sch_lockstep" "$CFS_DIR/apps/adcs" "$CFS_DIR/apps/shared" "$CFS_DIR/psp-lockstep"
cp -r "$SVC_CFS/apps/io_lockstep" "$CFS_DIR/apps/io_lockstep"
cp -r "$SVC_CFS/apps/sch_lockstep" "$CFS_DIR/apps/sch_lockstep"
cp -r "$SVC_CFS/apps/adcs" "$CFS_DIR/apps/adcs"
cp -r "$SVC_CFS/apps/shared" "$CFS_DIR/apps/shared"
cp -r "$SVC_CFS/psp-lockstep" "$CFS_DIR/psp-lockstep"

echo "=== [4/8] write third_party/cfs/rtems_zynqmp_defs/ (a NEW mission-defs dir -- sample_defs/ for the posix build is untouched) ===" | tee -a "$LOG_DIR/00-summary.log"
mkdir -p "$CFS_DIR/rtems_zynqmp_defs/cpu1"
cp "$SVC_CFS/build/targets-rtems.cmake" "$CFS_DIR/rtems_zynqmp_defs/targets.cmake"
cp "$SVC_CFS/build/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake" "$CFS_DIR/rtems_zynqmp_defs/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake"
cp "$SVC_CFS/build/generate_startup.cmake" "$CFS_DIR/rtems_zynqmp_defs/generate_startup.cmake"
cp "$SVC_CFS/build/cpu1_install_custom.cmake" "$CFS_DIR/rtems_zynqmp_defs/cpu1/install_custom.cmake"
# cfe_perfids.h has no automatic default-fallback (unlike every other per-module config header --
# see services/cfs/build/cfe_perfids.h's own top comment for the full account of how this was
# found: an empty generated header compiles until the first use of a reserved perf-ID constant).
cp "$SVC_CFS/build/cfe_perfids.h" "$CFS_DIR/rtems_zynqmp_defs/cfe_perfids.h"
# Overrides OSAL's default_bsp_rtems_cfg.h (PC-ATA-driver options this Zynq target has no use
# for -- see services/cfs/build/bsp_rtems_cfg.h's own top comment for the full account).
cp "$SVC_CFS/build/bsp_rtems_cfg.h" "$CFS_DIR/rtems_zynqmp_defs/bsp_rtems_cfg.h"

echo "=== [5/8] (re-)append the rtems_zynqmp CONFIG_NAMES entry to target-configs.mk ===" | tee -a "$LOG_DIR/00-summary.log"
# Idempotent by CONTENT, not just presence: strip any previously-appended block (marked by the
# "# M24.3:" sentinel comment this file's own tracked copy starts with) before re-appending the
# CURRENT tracked version -- a plain presence check (grep for the marker, skip if found) bit this
# task once already: editing services/cfs/build/target-configs-append-rtems.mk (adding
# -DOSAL_CONFIG_INCLUDE_NETWORK=FALSE) had no effect on a re-run because the OLD appended text
# was already there and the old check skipped re-appending, silently building with stale PREP_OPTS.
marker_line="$(grep -n '^# M24\.3: appended (not substituted)' "$CFS_DIR/target-configs.mk" | head -1 | cut -d: -f1 || true)"
if [ -n "${marker_line:-}" ]; then
    head -n "$((marker_line - 1))" "$CFS_DIR/target-configs.mk" > "$CFS_DIR/target-configs.mk.tmp"
    mv "$CFS_DIR/target-configs.mk.tmp" "$CFS_DIR/target-configs.mk"
fi
cat "$SVC_CFS/build/target-configs-append-rtems.mk" >> "$CFS_DIR/target-configs.mk"
echo "appended (fresh)" | tee -a "$LOG_DIR/00-summary.log"

cd "$CFS_DIR"

echo "=== [6/8] make rtems_zynqmp.prep ===" | tee -a "$LOG_DIR/00-summary.log"
make rtems_zynqmp.prep 2>&1 | tee "$LOG_DIR/02-prep.log"
prep_status=${PIPESTATUS[0]}
echo "prep exit status: $prep_status" | tee -a "$LOG_DIR/00-summary.log"
if [ "$prep_status" -ne 0 ]; then
    echo "PREP FAILED -- stopping, not proceeding to compile" | tee -a "$LOG_DIR/00-summary.log"
    exit "$prep_status"
fi

echo "=== [7/8] make rtems_zynqmp.compile ===" | tee -a "$LOG_DIR/00-summary.log"
make rtems_zynqmp.compile 2>&1 | tee "$LOG_DIR/03-compile.log"
compile_status=${PIPESTATUS[0]}
echo "compile exit status: $compile_status" | tee -a "$LOG_DIR/00-summary.log"
if [ "$compile_status" -ne 0 ]; then
    echo "COMPILE FAILED -- stopping, not proceeding to install" | tee -a "$LOG_DIR/00-summary.log"
    exit "$compile_status"
fi

echo "=== [8/8] make rtems_zynqmp.install ===" | tee -a "$LOG_DIR/00-summary.log"
make rtems_zynqmp.install 2>&1 | tee "$LOG_DIR/04-install.log"
install_status=${PIPESTATUS[0]}
echo "install exit status: $install_status" | tee -a "$LOG_DIR/00-summary.log"

echo "=== Artifact verification (by content, not exit code) ===" | tee -a "$LOG_DIR/00-summary.log"
EXE_DIR="$CFS_DIR/build-rtems_zynqmp/exe/cpu1"
if [ -d "$EXE_DIR" ]; then
    find "$EXE_DIR" -type f | tee "$LOG_DIR/05-exe-dir-listing.txt"
    for f in "$EXE_DIR"/*; do
        [ -f "$f" ] || continue
        echo "--- $f ---" | tee -a "$LOG_DIR/06-artifact-verify.log"
        file "$f" | tee -a "$LOG_DIR/06-artifact-verify.log"
        /output/toolchain/bin/arm-rtems6-readelf -h "$f" 2>&1 | tee -a "$LOG_DIR/06-artifact-verify.log" || true
        sha256sum "$f" | tee -a "$LOG_DIR/06-artifact-verify.log"
        ls -la "$f" | tee -a "$LOG_DIR/06-artifact-verify.log"
    done
else
    echo "NO EXE DIR AT $EXE_DIR -- build did not produce the expected output tree" | tee -a "$LOG_DIR/00-summary.log"
fi

echo "=== Done. Logs in $LOG_DIR ===" | tee -a "$LOG_DIR/00-summary.log"
