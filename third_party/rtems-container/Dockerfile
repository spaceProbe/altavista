# RTEMS 6.1 arm-rtems6 toolchain + zynqmp_rpu_lock_step BSP build box (M24.2c).
#
# Base pinned by digest (recorded in REPORT.md): debian:bookworm-slim
# @sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
#
# This container is a BUILD BOX ONLY. It fetches the same pinned RSB (tag 6.1,
# commit b1aec32059aa0e86385ff75ec01daf93713fa382) and RTEMS kernel source (tag 6.1,
# commit 0a46769ba42d3476b0f37a85db49b3276658d293) that
# third_party/rtems/fetch-rsb.sh / fetch-rtems-src.sh used on the macOS host attempt
# (docs/open-questions.md question 148/154), builds the arm-rtems6 toolchain and the
# zynqmp_rpu_lock_step BSP, and writes everything to a bind-mounted /output so the host
# (which runs Renode -- no linux/arm64 Renode image exists) can pick up the results.
#
# Host prerequisites below follow RTEMS's own documented RSB prerequisites list
# (https://docs.rtems.org/branches/master/user/start/index.html "Host Setup", Debian/Ubuntu
# section) plus git/ca-certificates for the pinned clones done at image-build time (network
# permitted for image build + pinned source fetch only, question 154).
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        git ca-certificates curl wget \
        build-essential gcc g++ \
        autoconf automake libtool \
        bison flex gperf \
        texinfo \
        python3 python3-dev python-is-python3 \
        unzip xz-utils patch \
        zlib1g-dev \
        pkg-config \
        libncurses-dev \
        ninja-build \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work

# --- Fetch RSB, pinned identically to third_party/rtems/fetch-rsb.sh ---
# Pin: tag 6.1, commit b1aec32059aa0e86385ff75ec01daf93713fa382 (cross-checked below, not
# trusting the tag name alone -- same discipline as the host fetch script).
RUN git clone --quiet --no-checkout https://github.com/RTEMS/rtems-source-builder.git rsb.tmp \
    && cd rsb.tmp && git checkout --quiet 6.1 \
    && actual="$(git rev-parse HEAD)" \
    && expected="b1aec32059aa0e86385ff75ec01daf93713fa382" \
    && if [ "$actual" != "$expected" ]; then echo "RSB pin mismatch: got $actual, want $expected" >&2; exit 1; fi \
    && rm -rf .git \
    && cd /work && mv rsb.tmp rsb

# --- Fetch RTEMS kernel source, pinned identically to third_party/rtems/fetch-rtems-src.sh ---
# Pin: tag 6.1, commit 0a46769ba42d3476b0f37a85db49b3276658d293.
RUN git clone --quiet --no-checkout https://github.com/RTEMS/rtems.git rtems-src.tmp \
    && cd rtems-src.tmp && git checkout --quiet 6.1 \
    && actual="$(git rev-parse HEAD)" \
    && expected="0a46769ba42d3476b0f37a85db49b3276658d293" \
    && if [ "$actual" != "$expected" ]; then echo "RTEMS src pin mismatch: got $actual, want $expected" >&2; exit 1; fi \
    && rm -rf .git \
    && cd /work && mv rtems-src.tmp rtems-src

# NOTE: unlike the host's fetch-rsb.sh, the binutils/gdb --with-system-zlib patches are
# NOT applied here by default -- this build is explicitly testing whether they are needed
# on Linux (see REPORT.md, "expectation to verify" before "Toolchain build"). build.sh applies
# them only if a first unpatched attempt actually reproduces the same zlib/fdopen failure.
COPY patches/ /work/patches/
COPY build.sh /work/build.sh
RUN chmod +x /work/build.sh

ENTRYPOINT ["/work/build.sh"]
