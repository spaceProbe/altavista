#!/bin/sh
# Fetch the RTEMS kernel source at a pinned release tag under third_party/rtems/rtems-src,
# following the same convention as fetch-rsb.sh / third_party/fetch-cfs.sh: network use is the
# one-time fetch/build exception (docs/open-questions.md question 154); nothing under
# third_party/rtems/rtems-src is committed to git (no .git kept).
#
# Pinned: tag 6.1 (docs/open-questions.md question 144's decided RTEMS release for the
# zynqmp_rpu_lock_step BSP), commit 0a46769ba42d3476b0f37a85db49b3276658d293 (recorded here,
# not just the tag).
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/rtems-src"

RTEMS_TAG="6.1"
RTEMS_COMMIT="0a46769ba42d3476b0f37a85db49b3276658d293"
RTEMS_URL="${RTEMS_KERNEL_URL:-https://github.com/RTEMS/rtems.git}"

if [ -f "$dest/PINNED_COMMIT" ] && [ -d "$dest/bsps" ]; then
    actual="$(cat "$dest/PINNED_COMMIT")"
    if [ "$actual" != "$RTEMS_COMMIT" ]; then
        echo "third_party/rtems/rtems-src is present but pinned at $actual, not $RTEMS_COMMIT -- remove it and re-run to re-pin" >&2
        exit 1
    fi
    echo "already present and pinned: $dest ($RTEMS_COMMIT)"
    exit 0
fi

rm -rf "$dest" "$dest.tmp"
git clone --quiet --no-checkout "$RTEMS_URL" "$dest.tmp"
(
    cd "$dest.tmp"
    git checkout --quiet "$RTEMS_TAG"
)
actual="$(cd "$dest.tmp" && git rev-parse HEAD)"
if [ "$actual" != "$RTEMS_COMMIT" ]; then
    echo "fetch-rtems-src.sh: tag $RTEMS_TAG resolved to $actual, expected $RTEMS_COMMIT" >&2
    exit 1
fi
rm -rf "$dest.tmp/.git"
mv "$dest.tmp" "$dest"
printf '%s\n' "$RTEMS_COMMIT" > "$dest/PINNED_COMMIT"

echo "fetched RTEMS kernel tag $RTEMS_TAG ($RTEMS_COMMIT) into $dest"
