#!/bin/sh
# Fetch NASA cFS (the bundle repo plus the cfe/osal/psp submodules only -- ci_lab/to_lab/sch_lab
# and the rest of the app catalog are not cloned because M23.2 replaces those three apps
# entirely; see services/cfs/README.md and docs/sil-plan.md's M23 milestone) at a commit pinned
# for a working OSAL-posix ("SIMULATION=native") build (docs/open-questions.md questions 147,
# 148, 154).
#
# Network use here is the one-time, explicitly-permitted exception (question 154): "building
# the cFS image needs the network once (packages and the pinned cFS clone), like vendoring;
# tests and runs never touch it." Nothing under third_party/cfs is committed to git (this
# script fetches on demand, same convention as fetch-gmat-src.sh/fetch-cspice.sh); services/cfs/Dockerfile
# runs this script itself during `docker build`, which is the same permitted network window.
#
# ---------------------------------------------------------------------------------------------
# Why this commit (question 148's research note)
# ---------------------------------------------------------------------------------------------
# Pinned: the nasa/cFS bundle tag v7.0.1, and the exact submodule commits that tag's
# .gitmodules resolves to for cfe/osal/psp (recorded below, not just the tag, so a future
# upstream force-push or tag move cannot silently change what this script fetches).
#
#   bundle (nasa/cFS):              v7.0.1  = 088b2fa828db9ff7e00733f1908e0eeb59f66ce3
#   cfe    (nasa/cFE):                        c5fb2b4d540bd55eb6c3707da7dd13eee679d4dd
#   osal   (nasa/osal):                       d2d877a69cff47452bcca274b309147d48e6c16f
#   psp    (nasa/PSP):                        c4b3b0b65b119e106481ad8e20976ae4d7f554e3
#   tools/tblCRCTool (nasa/tblCRCTool):        cc61e89535db9fabe35fe26d1a600889f06b3104
#   tools/elf2cfetbl (nasa/elf2cfetbl):        118b55fe1d128b48dc45fd36278b87827c6d3faa
#   tools/commandline-tools (nasa/cfs-commandline-tools): d70c56ec035694c9a64b317897403266166f5d68
#
# The three tools/* submodules are host-side build tooling `tools/CMakeLists.txt` requires
# unconditionally for any `CFE_EDS_ENABLED=OFF` build (a CRC tool and an ELF-to-table converter
# for cFE's table services, and a commandline-tools helper) -- not flight apps, and needed
# regardless of which mission apps are built, so fetching them is not scope creep alongside
# "ci_lab/to_lab/sch_lab are not fetched".
#
# Question 148 flagged that NASA's RTEMS 6 support is still landing upstream (build without a
# network stack, RTEMS 6 test containers, Gaisler toolchain differences) and asked the team to
# pin a commit with a *working* build and report what had to be patched. M23.2 targets
# OSAL-posix in Linux (question 147's "easier path"), not RTEMS 6 -- that pairing sidesteps the
# RTEMS 6 landing entirely: `SIMULATION=native` builds cFE against OSAL's `posix` implementation
# and PSP's `pc-linux` module, both of which have built on plain Linux for many releases (this is
# the same build path the upstream cFS CI itself exercises on every commit). v7.0.1 is the latest
# tagged bundle release as of this pin (2026-09) and its native/posix build has no open upstream
# issues blocking it that this task found.
#
# **One patch is carried since M24.4** (see the "Patches applied" block near the end of this
# script, and third_party/renode/M24_4_REPORT.md's question 148 accounting). Through M24.3 none
# was; the paragraph below records why the posix path still needs none:
#
# OSAL's own public API already has the extension point M23.2 needs:
# `OS_TimeBaseCreate(&id, name, external_sync)` -- when `external_sync` is non-NULL, OSAL treats
# it as "a BSP-provided function that will block the calling task until the next tick occurs"
# (osal/src/os/inc/osapi-timebase.h's own doc comment) instead of arming its own POSIX interval
# timer. `services/cfs/psp-lockstep` supplies exactly that function; no OSAL/PSP source under
# third_party/cfs is modified. This is what question 148's "record what had to be patched"
# resolves to for this pin: nothing.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
# CFS_FETCH_DEST lets a test point a real, unmodified run of this script at a scratch directory
# (e.g. a clean-fetch patch-application test that must not touch the shared third_party/cfs
# tree) without duplicating any of the clone/checkout/patch logic below. Defaults to the normal
# in-place location so every existing caller (services/cfs/Dockerfile, developers running this
# by hand) is unaffected.
dest="${CFS_FETCH_DEST:-$here/cfs}"

BUNDLE_COMMIT="088b2fa828db9ff7e00733f1908e0eeb59f66ce3"
CFE_COMMIT="c5fb2b4d540bd55eb6c3707da7dd13eee679d4dd"
OSAL_COMMIT="d2d877a69cff47452bcca274b309147d48e6c16f"
PSP_COMMIT="c4b3b0b65b119e106481ad8e20976ae4d7f554e3"
TBLCRCTOOL_COMMIT="cc61e89535db9fabe35fe26d1a600889f06b3104"
ELF2CFETBL_COMMIT="118b55fe1d128b48dc45fd36278b87827c6d3faa"
CMDLINE_TOOLS_COMMIT="d70c56ec035694c9a64b317897403266166f5d68"

BUNDLE_URL="${CFS_BUNDLE_URL:-https://github.com/nasa/cFS.git}"
CFE_URL="${CFS_CFE_URL:-https://github.com/nasa/cFE.git}"
OSAL_URL="${CFS_OSAL_URL:-https://github.com/nasa/osal.git}"
PSP_URL="${CFS_PSP_URL:-https://github.com/nasa/PSP.git}"
TBLCRCTOOL_URL="${CFS_TBLCRCTOOL_URL:-https://github.com/nasa/tblCRCTool.git}"
ELF2CFETBL_URL="${CFS_ELF2CFETBL_URL:-https://github.com/nasa/elf2cfetbl.git}"
CMDLINE_TOOLS_URL="${CFS_CMDLINE_TOOLS_URL:-https://github.com/nasa/cfs-commandline-tools.git}"

if [ -f "$dest/cfe/cmake/Makefile.sample" ] && [ -f "$dest/osal/src/os/posix/src/os-impl-timebase.c" ] && [ -f "$dest/psp/README.md" ] \
   && [ -d "$dest/tools/tblCRCTool" ] && [ -d "$dest/tools/elf2cfetbl" ] && [ -d "$dest/tools/commandline-tools" ] && [ -f "$dest/PINNED_COMMIT" ]; then
    actual="$(cat "$dest/PINNED_COMMIT")"
    if [ "$actual" != "$BUNDLE_COMMIT" ]; then
        echo "third_party/cfs is present but pinned at $actual, not $BUNDLE_COMMIT -- remove it and re-run to re-pin" >&2
        exit 1
    fi
    echo "already present and pinned: $dest ($BUNDLE_COMMIT)"
    exit 0
fi

rm -rf "$dest" "$dest.tmp"
git clone --no-checkout "$BUNDLE_URL" "$dest.tmp"
(
    cd "$dest.tmp"
    git checkout --quiet "$BUNDLE_COMMIT"
)

# Submodules are cloned directly rather than via `git submodule update` so only cfe/osal/psp are
# fetched -- the bundle's own .gitmodules lists ~20 apps/tools this task does not need (io_lockstep
# and sch_lockstep, both under services/cfs/apps, replace ci_lab/to_lab/sch_lab entirely).
for pair in "cfe|$CFE_URL|$CFE_COMMIT" "osal|$OSAL_URL|$OSAL_COMMIT" "psp|$PSP_URL|$PSP_COMMIT" \
            "tools/tblCRCTool|$TBLCRCTOOL_URL|$TBLCRCTOOL_COMMIT" \
            "tools/elf2cfetbl|$ELF2CFETBL_URL|$ELF2CFETBL_COMMIT" \
            "tools/commandline-tools|$CMDLINE_TOOLS_URL|$CMDLINE_TOOLS_COMMIT"; do
    name="${pair%%|*}"
    rest="${pair#*|}"
    url="${rest%%|*}"
    commit="${rest#*|}"
    rmdir "$dest.tmp/$name" 2>/dev/null || rm -rf "${dest:?}.tmp/${name:?}"
    git clone --no-checkout "$url" "$dest.tmp/$name"
    ( cd "$dest.tmp/$name" && git checkout --quiet "$commit" )
    actual="$(cd "$dest.tmp/$name" && git rev-parse HEAD)"
    if [ "$actual" != "$commit" ]; then
        echo "fetch-cfs.sh: $name landed on $actual, expected $commit" >&2
        exit 1
    fi
    rm -rf "$dest.tmp/$name/.git"
done
rm -rf "$dest.tmp/.git"
mv "$dest.tmp" "$dest"
# No .git kept (same convention as fetch-gmat-src.sh) -- this is a fetched build input, not a
# vendored copy; PINNED_COMMIT is this script's own idempotency/re-pin marker, not a VCS.
printf '%s\n' "$BUNDLE_COMMIT" > "$dest/PINNED_COMMIT"

# ---------------------------------------------------------------------------- Patches applied
# Question 148 requires every carried patch to be recorded. third_party/cfs is fetched, never
# committed, so a patch living only in a working tree is lost on the next fetch -- exactly what
# would have happened after M24.4 wrote this one: the fix existed on disk but this script did not
# apply it, so a clean environment would have silently rebuilt the broken behaviour. Applying it
# here is what makes the pin reproducible.
patches_dir="$here/renode/M24_4/patches"
if [ -d "$patches_dir" ]; then
    for p in "$patches_dir"/*.patch; do
        [ -e "$p" ] || continue
        echo "applying $(basename "$p")"
        if ! patch -p1 -d "$dest" < "$p"; then
            echo "ERROR: failed to apply $(basename "$p") -- the pin is not reproducible without it" >&2
            exit 1
        fi
    done
fi

echo "fetched nasa/cFS $BUNDLE_COMMIT (cfe=$CFE_COMMIT osal=$OSAL_COMMIT psp=$PSP_COMMIT) into $dest"
