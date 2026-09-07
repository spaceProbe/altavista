#!/bin/sh
# Fetch Renode at a pinned version under third_party/renode (not vendored into git), following
# the same convention as third_party/fetch-cfs.sh / fetch-gmat-src.sh / fetch-cspice.sh: network
# use is the one-time, explicitly-permitted install/build exception (docs/open-questions.md
# question 154), and nothing under third_party/renode/<extracted tree> is committed.
#
# M24.1 (docs/sil-plan.md M24 milestone; question 144's target is Renode's mainline Zynq
# UltraScale+ Cortex-R5 platform): "Prefer a portable build for this Mac if it actually runs...
# Otherwise use Renode's official Docker image for linux/arm64 pinned by digest."
#
# Pinned: Renode v1.16.1 (latest tagged release as of this pin, 2026-09), macOS arm64 portable
# (.NET) build -- renode-1.16.1-dotnet.osx-arm64-portable.dmg, the exact asset GitHub's release
# API lists for this tag. SHA-256 recorded below is GitHub's own reported asset digest,
# cross-checked against a local `shasum -a 256` of the downloaded file by this script (fails
# loudly on mismatch, same discipline as fetch-cfs.sh's commit verification).
#
# Finding recorded here (see third_party/renode/REPORT.md): antmicro/renode's official Docker
# Hub image (the "official Docker image" question 144/M24 refers to) publishes **amd64 only**
# for every tag including 1.16.1 -- `docker manifest inspect antmicro/renode:1.16.1` returns a
# single-platform v2 manifest, not a multi-arch manifest list, so no linux/arm64 digest exists
# to pin. This script therefore fetches the macOS arm64 portable build as the primary path; the
# Docker fallback (if the portable build does not run) is documented in REPORT.md as "amd64
# under emulation", not a true linux/arm64 pin, and that deviation is disclosed rather than
# silently substituted.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/renode-1.16.1-osx-arm64"

RENODE_VERSION="1.16.1"
ASSET_NAME="renode-${RENODE_VERSION}-dotnet.osx-arm64-portable.dmg"
ASSET_SHA256="99b8ae5897b8926ef179868d39a504fe5296555dc9c9b973718ddf3ab09175d9"
ASSET_URL="${RENODE_ASSET_URL:-https://github.com/renode/renode/releases/download/v${RENODE_VERSION}/${ASSET_NAME}}"

if [ -f "$dest/PINNED_VERSION" ] && [ -d "$dest/Renode.app" ]; then
    actual="$(cat "$dest/PINNED_VERSION")"
    if [ "$actual" != "$RENODE_VERSION" ]; then
        echo "third_party/renode/renode-1.16.1-osx-arm64 is present but pinned at $actual, not $RENODE_VERSION -- remove it and re-run to re-pin" >&2
        exit 1
    fi
    echo "already present and pinned: $dest ($RENODE_VERSION)"
    exit 0
fi

rm -rf "$dest" "$dest.tmp" "$here/_renode_dmg_mount"
mkdir -p "$dest.tmp"

dmg_path="$here/${ASSET_NAME}"
curl -fL -o "$dmg_path" "$ASSET_URL"

actual_sha="$(shasum -a 256 "$dmg_path" | awk '{print $1}')"
if [ "$actual_sha" != "$ASSET_SHA256" ]; then
    echo "fetch-renode.sh: downloaded $ASSET_NAME sha256 $actual_sha, expected $ASSET_SHA256" >&2
    rm -f "$dmg_path"
    exit 1
fi

mount_point="$here/_renode_dmg_mount"
mkdir -p "$mount_point"
hdiutil attach "$dmg_path" -mountpoint "$mount_point" -nobrowse -quiet

# Copy out whatever .app bundle (or, if this build ships a plain directory instead of a .app,
# that directory) the dmg contains, rather than assuming one exact name.
found_app="$(find "$mount_point" -maxdepth 1 -iname '*.app' | head -n1)"
if [ -n "$found_app" ]; then
    cp -R "$found_app" "$dest.tmp/Renode.app"
else
    echo "fetch-renode.sh: no .app bundle found at top level of the mounted dmg:" >&2
    ls -la "$mount_point" >&2
    hdiutil detach "$mount_point" -quiet || true
    rm -rf "$dest.tmp" "$mount_point"
    exit 1
fi

hdiutil detach "$mount_point" -quiet
rmdir "$mount_point" 2>/dev/null || true
rm -f "$dmg_path"

printf '%s\n' "$RENODE_VERSION" > "$dest.tmp/PINNED_VERSION"
printf '%s\n' "$ASSET_SHA256" > "$dest.tmp/PINNED_SHA256"
mv "$dest.tmp" "$dest"

echo "fetched Renode $RENODE_VERSION ($ASSET_NAME, sha256=$ASSET_SHA256) into $dest"
