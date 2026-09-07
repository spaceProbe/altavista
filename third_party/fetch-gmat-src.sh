#!/bin/sh
# Fetch the GMAT R2026a source headers needed to compile crates/gmat-sys's C++ shim.
# The binary GMAT release ships no headers; the source (Apache 2.0) is on SourceForge and
# GitHub (nasa/GMAT). Only src/base and src/gmatutil are checked out.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/gmat-src"
branch="${GMAT_SRC_BRANCH:-GMAT-R2026a}"
if [ -d "$dest/src/base" ]; then
  echo "already present: $dest"; exit 0
fi
git clone --depth 1 --branch "$branch" --sparse https://git.code.sf.net/p/gmat/git "$dest"
( cd "$dest" && git sparse-checkout set src/base src/gmatutil )
# Headers are all the shim needs; the object store is hundreds of megabytes.
rm -rf "$dest/.git"
echo "fetched GMAT $branch headers into $dest"
