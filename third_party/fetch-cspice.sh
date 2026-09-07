#!/bin/sh
# Fetch the CSPICE toolkit headers (NAIF, public domain) needed to compile GMAT's headers
# with __USE_SPICE__ defined. Only include/ is kept; the SPICE code itself is already inside
# GMAT's libGmatBase.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/cspice"
if [ -f "$dest/include/SpiceUsr.h" ]; then
  echo "already present: $dest/include"; exit 0
fi
# Any platform's package has the same headers; the M1 package is the smallest download here.
url="${CSPICE_URL:-https://naif.jpl.nasa.gov/pub/naif/toolkit/C/MacM1_OSX_clang_64bit/packages/cspice.tar.Z}"
tmp="$(mktemp -d)"
curl -sSf -o "$tmp/cspice.tar.Z" "$url"
( cd "$tmp" && tar -xzf cspice.tar.Z cspice/include )
mkdir -p "$dest"
mv "$tmp/cspice/include" "$dest/include"
rm -rf "$tmp"
echo "fetched CSPICE headers into $dest/include"
