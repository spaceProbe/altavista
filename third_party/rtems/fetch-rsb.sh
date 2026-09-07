#!/bin/sh
# Fetch the RTEMS Source Builder (RSB) at a pinned release tag under third_party/rtems/rsb,
# following the same convention as third_party/fetch-cfs.sh / fetch-renode.sh: network use is
# the one-time, explicitly-permitted fetch/build exception (docs/open-questions.md question
# 154); nothing under third_party/rtems/rsb is committed to git (no .git kept, same as
# fetch-cfs.sh's fetched submodules).
#
# Pinned: tag 6.1 (exact match to RTEMS 6.1, docs/open-questions.md question 144's decided
# RTEMS release), commit b1aec32059aa0e86385ff75ec01daf93713fa382 (recorded here, not just the
# tag, so a future upstream tag move cannot silently change what this script fetches -- same
# discipline as fetch-cfs.sh's commit pins).
#
# One patch is carried (question 148: "a patch we carry is recorded under third_party/ with
# its upstream issue link"): third_party/rtems/patches/rsb-binutils-with-system-zlib.patch and
# rsb-gdb-with-system-zlib.patch add `--with-system-zlib` to the binutils and gdb configure
# invocations. Without it, both fail to build their bundled zlib copy on this host (Xcode 26 /
# clang 21's macOS SDK headers): `zlib/zutil.h`'s `#define fdopen(fd,mode) NULL` collides with
# `_stdio.h`'s own `fdopen()` declaration once the SDK header is included after that macro is
# defined, producing "error: expected ')'" in `zutil.c`. This is a known, already-reported
# upstream bug, reproduced independently by this task with byte-for-byte the same error text:
# https://gitlab.rtems.org/rtems/tools/rtems-source-builder/-/issues/100 ("Can not Build
# gdb-16.2-arm64-apple-darwin", closed, but neither the 6.1 tag nor upstream `main` carries a
# `--with-system-zlib` fix as of this pin -- checked directly against both). See
# third_party/rtems/REPORT.md for the full build-log evidence.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/rsb"

RSB_TAG="6.1"
RSB_COMMIT="b1aec32059aa0e86385ff75ec01daf93713fa382"
RSB_URL="${RTEMS_RSB_URL:-https://github.com/RTEMS/rtems-source-builder.git}"

if [ -f "$dest/PINNED_COMMIT" ] && [ -d "$dest/rtems/config" ]; then
    actual="$(cat "$dest/PINNED_COMMIT")"
    if [ "$actual" != "$RSB_COMMIT" ]; then
        echo "third_party/rtems/rsb is present but pinned at $actual, not $RSB_COMMIT -- remove it and re-run to re-pin" >&2
        exit 1
    fi
    if ! grep -q with-system-zlib "$dest/source-builder/config/binutils-2-1.cfg"; then
        echo "third_party/rtems/rsb is present but missing the carried zlib patch -- remove it and re-run to re-pin and re-patch" >&2
        exit 1
    fi
    echo "already present, pinned and patched: $dest ($RSB_COMMIT)"
    exit 0
fi

rm -rf "$dest" "$dest.tmp"
git clone --quiet --no-checkout "$RSB_URL" "$dest.tmp"
(
    cd "$dest.tmp"
    git checkout --quiet "$RSB_TAG"
)
actual="$(cd "$dest.tmp" && git rev-parse HEAD)"
if [ "$actual" != "$RSB_COMMIT" ]; then
    echo "fetch-rsb.sh: tag $RSB_TAG resolved to $actual, expected $RSB_COMMIT" >&2
    exit 1
fi
rm -rf "$dest.tmp/.git"

# Apply the carried zlib patch (see the header comment above and REPORT.md) before this
# checkout is considered fetched, so every consumer of $dest sees the patched configs.
patch -p1 -d "$dest.tmp" < "$here/patches/rsb-binutils-with-system-zlib.patch"
patch -p1 -d "$dest.tmp" < "$here/patches/rsb-gdb-with-system-zlib.patch"

mv "$dest.tmp" "$dest"
printf '%s\n' "$RSB_COMMIT" > "$dest/PINNED_COMMIT"

echo "fetched RTEMS Source Builder tag $RSB_TAG ($RSB_COMMIT) into $dest, patched (with-system-zlib)"
