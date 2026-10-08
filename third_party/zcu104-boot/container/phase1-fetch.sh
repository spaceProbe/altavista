#!/bin/bash
# container/phase1-fetch.sh -- PHASE 1, the only phase with network. Runs in the pinned Debian
# container. Mounts: /recipe (this directory's parent, read-only), /cache (read-write).
# Fetches into /cache and checks every artifact against pinned-inputs.sh / apt-debs.sha256:
#   /cache/apt/*.deb         the pinned apt closure
#   /cache/src/embeddedsw    Xilinx embeddedsw at the pinned commit (no .git)
#   /cache/src/bootgen       Xilinx bootgen at the pinned commit (no .git)
#   /cache/tarballs/*        binutils, gcc, newlib, gmp, mpfr, mpc
# Everything already in the cache and matching its pin is left alone, so a second run is a no-op
# apart from `apt-get update`'s absence: the apt step is skipped when /cache/apt verifies.
set -euo pipefail
. /recipe/container/lib.sh
mkdir -p /cache/apt /cache/src /cache/tarballs

apt_ok() {
    [ -s /recipe/apt-debs.sha256 ] || return 1
    ( cd /cache/apt && ls -1 *.deb 2>/dev/null | LC_ALL=C sort > /tmp/have.txt && \
      awk '{print $2}' /recipe/apt-debs.sha256 | LC_ALL=C sort > /tmp/want.txt && \
      cmp -s /tmp/have.txt /tmp/want.txt && sha256sum -c --quiet /recipe/apt-debs.sha256 )
}

if apt_ok; then
    echo "phase1: apt cache verifies against apt-debs.sha256 (${#APT_PACKAGES[@]} packages), no download"
else
    echo "phase1: downloading the pinned apt closure"
    rm -rf /cache/apt/*
    apt-get update -qq
    apt-get install -y --no-install-recommends --download-only \
        -o Dir::Cache::archives=/cache/apt/ -o APT::Keep-Downloaded-Packages=true -o APT::Sandbox::User=root "${APT_PACKAGES[@]}"
    rm -rf /cache/apt/partial /cache/apt/lock
    ( cd /cache/apt && sha256sum *.deb ) > /cache/apt/SHA256SUMS.measured
    if [ -s /recipe/apt-debs.sha256 ]; then
        ( cd /cache/apt && sha256sum -c --quiet /recipe/apt-debs.sha256 ) || { echo "phase1: a downloaded .deb differs from apt-debs.sha256" >&2; exit 4; }
        rm -f /cache/apt/SHA256SUMS.measured
    else
        echo "phase1: apt-debs.sha256 is absent; measured sums left in /cache/apt/SHA256SUMS.measured" >&2
    fi
fi

# Everything below needs git, curl, python3 from the pinned set: install it (no network).
install_pinned_debs

fetch_git() {  # name url commit tree_sha256
    local name="$1" url="$2" commit="$3" want="$4" dest="/cache/src/$1" got
    if [ -d "${dest}" ]; then
        got="$(tree_manifest_hash "${dest}")"
        if [ "${got}" = "${want}" ]; then echo "phase1: ${name} cache verifies (${got})"; return 0; fi
        echo "phase1: ${name} cache tree ${got} != pinned ${want}; refetching" >&2
        rm -rf "${dest}"
    fi
    echo "phase1: fetching ${name} ${commit}"
    local tmp="/cache/src/.tmp-${name}"
    rm -rf "${tmp}"; mkdir -p "${tmp}"
    ( cd "${tmp}" && git init -q . && git remote add origin "${url}" && \
      git -c protocol.version=2 fetch -q --depth 1 origin "${commit}" && \
      git -c advice.detachedHead=false checkout -q --detach FETCH_HEAD )
    [ "$(git -C "${tmp}" rev-parse HEAD)" = "${commit}" ] || { echo "phase1: ${name} HEAD is not ${commit}" >&2; exit 4; }
    rm -rf "${tmp}/.git"
    got="$(tree_manifest_hash "${tmp}")"
    echo "phase1: ${name} tree manifest sha256 ${got}"
    if [ "${got}" != "${want}" ]; then
        echo "phase1: ${name} tree ${got} != pinned ${want}" >&2
        echo "${got}" > "/cache/src/${name}.measured-tree-sha256"
        rm -rf "${tmp}"
        exit 4
    fi
    mv "${tmp}" "${dest}"
}
fetch_git embeddedsw "${EMBEDDEDSW_URL}" "${EMBEDDEDSW_COMMIT}" "${EMBEDDEDSW_TREE_SHA256}"
fetch_git bootgen "${BOOTGEN_URL}" "${BOOTGEN_COMMIT}" "${BOOTGEN_TREE_SHA256}"

for entry in "${TOOLCHAIN_TARBALLS[@]}"; do
    IFS='|' read -r name url want <<<"${entry}"
    f="/cache/tarballs/${name}"
    if [ -f "${f}" ] && [ "$(sha256sum "${f}" | cut -d' ' -f1)" = "${want}" ]; then
        echo "phase1: ${name} cache verifies"
        continue
    fi
    echo "phase1: fetching ${url}"
    rm -f "${f}"
    curl -fsSL --retry 3 -o "${f}.part" "${url}"
    got="$(sha256sum "${f}.part" | cut -d' ' -f1)"
    if [ "${got}" != "${want}" ]; then
        echo "phase1: ${name} sha256 ${got} != pinned ${want}" >&2
        rm -f "${f}.part"
        exit 4
    fi
    mv "${f}.part" "${f}"
done
chown -R "${HOST_UID}:${HOST_GID}" /cache
echo "phase1: done"
