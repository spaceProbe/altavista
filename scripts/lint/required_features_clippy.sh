#!/usr/bin/env bash
# Heavy round 6, task 7 (question 231): `cargo clippy --workspace --all-targets` silently
# skips any target that declares `required-features` (Cargo does not build a `[[bin]]`,
# `[[example]]`, `[[test]]` or `[[bench]]` unless the named features are enabled), so a
# crate can carry unlinted Rust behind the workspace's own named gate. Today that is
# `av-jobs`'s `av-tile-fixture` binary (`required-features = ["store-fixture"]`), found by
# a manual audit rather than by the gate itself. This script closes that hole by DISCOVERING
# every such target from `cargo metadata` -- never by naming a crate or a feature as a
# constant -- grouping them by (package, feature set), and running
# `cargo clippy -p <pkg> --features <feats> --all-targets -- -D warnings` once per group, so
# the next crate that adds a `required-features` target is picked up automatically the next
# time this script runs, with nobody having to remember to name it here.
#
# No new dependency: discovery is `cargo metadata --no-deps --format-version=1` (offline --
# it reads Cargo.toml/Cargo.lock and does not touch the network or build anything), parsed by
# the companion `required_features_clippy.py` (stdlib `json` only -- `jq` is not assumed to
# be installed). Grouping is by package because `cargo clippy -p <pkg> --features <feats>` is
# scoped to one package; two `required-features` targets in the same package that need the
# same feature set are linted together by the same invocation (clippy naturally covers every
# target `--all-targets` finds buildable under those features), and two different feature
# sets in the same package get two separate invocations.
#
# This is ADDITIONAL coverage. The standing `cargo clippy --workspace --all-targets --
# -D warnings` step stays exactly as it is; this script never replaces it, and never touches
# any crate's `required-features` declaration.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

echo "required_features_clippy.sh: running 'cargo metadata --no-deps --format-version=1' (offline, no build) to discover required-features targets..." >&2

METADATA_FILE="$(mktemp)"
trap 'rm -f "$METADATA_FILE"' EXIT
cargo metadata --no-deps --format-version=1 > "$METADATA_FILE"

# Discover (package, sorted feature set) groups and the targets that need them, from the
# metadata JSON alone -- this is the only place that knows about `required-features`, and it
# never names a package or a feature by hand.
#
# NOTE: the discovered lines are deliberately NOT held in a variable named `GROUPS` --
# `GROUPS` is bash's own special, dynamically-populated array (the caller's Unix group IDs,
# e.g. `20` for macOS's "staff"); assigning to it is silently ineffective in some bash
# versions, so `$GROUPS` keeps reading back the real group list instead of our data. Measured
# directly on this host: `bash -c 'echo "${GROUPS[@]}"'` prints the group-ID list starting
# with `20`, which is exactly the bogus "-p 20" this script fed to clippy before the rename.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DISCOVERED_GROUPS="$(python3 "$SCRIPT_DIR/required_features_clippy.py" "$METADATA_FILE")"

if [ -z "$DISCOVERED_GROUPS" ]; then
    echo "required_features_clippy.sh: no workspace target declares required-features; nothing to do." >&2
    exit 0
fi

FAILED=0
GROUP_COUNT=0

while IFS=$'\t' read -r PKG FEATURES TARGETS; do
    [ -z "$PKG" ] && continue
    GROUP_COUNT=$((GROUP_COUNT + 1))
    echo "required_features_clippy.sh: linting package=${PKG} features=${FEATURES} targets=${TARGETS}" >&2
    if ! cargo clippy -p "$PKG" --features "$FEATURES" --all-targets -- -D warnings; then
        echo "required_features_clippy.sh: FAILED package=${PKG} features=${FEATURES}" >&2
        FAILED=1
    fi
done <<< "$DISCOVERED_GROUPS"

echo "required_features_clippy.sh: linted ${GROUP_COUNT} (package, feature set) group(s)." >&2

if [ "$FAILED" -ne 0 ]; then
    echo "required_features_clippy.sh: at least one group failed clippy (-D warnings)." >&2
    exit 1
fi

echo "required_features_clippy.sh: all required-features groups clean." >&2
