#!/bin/sh
# Fetch the GMAT R2026a source headers needed to compile crates/gmat-sys's C++ shim.
# The binary GMAT release ships no headers; the source (Apache 2.0) is on SourceForge and
# GitHub (nasa/GMAT). Only src/base and src/gmatutil are checked out.
#
# Pinned to a COMMIT, and the commit is recorded beside the tree (native-dynamics round 5;
# docs/open-questions.md question 234's provenance question). This script used to clone the
# MOVING branch `GMAT-R2026a` and then `rm -rf` the clone's `.git`, which left
# `third_party/gmat-src` carrying no commit id, no tag and no upstream date of its own -- so
# "is this the tree the shipped libGmatBase.R2026a.dylib was built from?" could not even be
# asked of the checkout, let alone answered, and re-running the script on a later day could
# silently change what the mirror was with nothing recording that it had.
#
# The commit below was measured, not assumed: `git ls-remote` reports refs/tags/R2026a and
# refs/heads/GMAT-R2026a both pointing at it, and every one of the 1128 files in this host's
# existing mirror hashes (git blob hash) byte-identically to that commit's own blobs, with
# nothing extra on either side under the two sparse paths. See docs/native-dynamics-plan.md's
# round-5 status for the full measurement and for what it does and does not establish about
# the shipped binary.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/gmat-src"
revision_file="$here/gmat-src.REVISION"

# The GMAT R2026a release commit: "Merge branch 'GMT-8510_ReleaseTasks' into 'GMAT-R2026a'",
# Peter J Candell, 2026-03-26 16:00:34 +0000. Override only with another commit id, never a
# branch name.
GMAT_SRC_COMMIT="${GMAT_SRC_COMMIT:-47f1a6eb89f3653a542195d42c225c6b0d6746a9}"
# Cloned by TAG rather than by branch: a tag is immutable, so `--depth 1` against it fetches
# exactly the commit this script pins, and the verification below can then be a real check
# rather than a formality.
GMAT_SRC_TAG="${GMAT_SRC_TAG:-R2026a}"

if [ -d "$dest/src/base" ]; then
  if [ -f "$revision_file" ]; then
    echo "already present: $dest ($(cat "$revision_file"))"
  else
    # Not fatal -- the tree is here and usable -- but it must never be silent: an unrecorded
    # mirror is exactly the state question 234 found.
    echo "WARNING: $dest is present but $revision_file does not exist, so nothing records" >&2
    echo "         which upstream commit this checkout is. Delete $dest and re-run this" >&2
    echo "         script to obtain a recorded one." >&2
  fi
  exit 0
fi

git clone --depth 1 --branch "$GMAT_SRC_TAG" --sparse https://git.code.sf.net/p/gmat/git "$dest"
( cd "$dest" && git sparse-checkout set src/base src/gmatutil )

# Verify before the object store goes away. A tag that no longer resolves to the pinned commit
# means upstream moved it; that is a hard stop, not something to fetch through quietly.
actual="$( cd "$dest" && git rev-parse HEAD )"
if [ "$actual" != "$GMAT_SRC_COMMIT" ]; then
  echo "ERROR: tag $GMAT_SRC_TAG resolved to $actual, not the pinned $GMAT_SRC_COMMIT." >&2
  echo "       Upstream moved the tag. Do not use this checkout: establish what changed" >&2
  echo "       first (docs/open-questions.md question 234)." >&2
  rm -rf "$dest"
  exit 1
fi
# Record what this actually is, read back from the checkout rather than echoed from the
# variable, so the file states what was checked out and not what was requested.
printf '%s\n' "$actual" > "$revision_file"

# Headers are all the shim needs; the object store is hundreds of megabytes.
rm -rf "$dest/.git"
echo "fetched GMAT $actual (tag $GMAT_SRC_TAG) headers into $dest"
