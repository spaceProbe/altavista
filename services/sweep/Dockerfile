# services/sweep/Dockerfile (question 245, heavy-ct-2b) -- the SAMPLE image: `av-sweep` built
# `--no-default-features` (no GMAT linked: `cargo tree -p av-sweep --no-default-features -i
# gmat-sys` finds nothing), packaged to run ONE sweep sample as a job
# (`av-sweep --run-sample --drm-pb /av-job/in/000 ... --out /av-job/out/run_products.pb`) under
# `av_jobs::container::ContainerExecutor` or `av_jobs::k8s::K8sJobExecutor`, and to be compared
# byte for byte with the same sample run by a native process. Modelled on services/tiles/Dockerfile.
#
# Build from the REPOSITORY ROOT by running services/sweep/build-image.sh -- NEVER `docker build`
# directly, for the reason services/tiles/Dockerfile's header proves in full (the root workspace
# manifest pins `spoore-cdm` and, through `crates/av-proposer`, more crates to HOST paths a
# `docker build` context cannot reach, and this host's classic builder has no host-path bind
# mount): the binary is prebuilt, stripped, by a real `docker run` bind-mounting the repository in
# the pinned `rust:1.90-bookworm@sha256:3914072c...` container (build-image.sh's "prebuild" step),
# and COPYed here as a build artifact.
#
# ------------------------------------------------------------------------------------------
# Pinning (question 154: digests, not floating tags)
# ------------------------------------------------------------------------------------------
# `debian:bookworm-slim@sha256:88200866...` and the prebuild base `rust:1.90-bookworm@sha256:3914072c...`
# are the identical two digests services/tiles/, services/edge-plugin/ and services/proposer/ pin
# (the same Debian 12 userland the prebuild toolchain links against: same glibc, same OpenSSL
# 3.0.x ABI). Both are present locally; nothing here pulls a base image.
#
# ------------------------------------------------------------------------------------------
# What is in the image besides the binary: the GMAT-install data files a native sample READS
# ------------------------------------------------------------------------------------------
# A no-GMAT sample still reads four data files from a GMAT install, located by $GMAT_ROOT
# (`av_orbital::cof::locate_gmat_root`): the JGM2 gravity coefficients, the DE ephemeris the
# Sun/Moon point masses read, and the FK5 nutation series and Earth-orientation table. They are
# copied from the GMAT R2026a install (NASA, Apache-2.0) into the image under
# /opt/av-sweep/gmat/data/, byte for byte, verified by SHA-256 against
# services/sweep/IMAGE_DATA_FILES.txt by build-image.sh (which refuses a mismatch), and named with
# their hashes in services/sweep/IMAGE_DIGEST.md. Which four was measured, not read off the code: a
# study of the fixture run with $GMAT_ROOT pointing at a directory holding only these four files
# completes (see IMAGE_DIGEST.md). A DRM whose models read any other GMAT data file (space
# weather, other gravity fields, other ephemerides) fails in the pod with the missing path named;
# add the file to IMAGE_DATA_FILES.txt to support it.
#
# ------------------------------------------------------------------------------------------
# Network posture and hardening at RUNTIME
# ------------------------------------------------------------------------------------------
# A sample needs NO network: it reads /av-job/in/*, computes, writes /av-job/out/run_products.pb
# and prints one line to stderr. The executors run it with `--network none` (docker) or under a
# default-deny NetworkPolicy plus a start-up gate (k8s). The image declares NO `VOLUME`: the whole
# root filesystem is read-only under `--read-only` / `readOnlyRootFilesystem`, the only writable
# path being the executor's `/av-job/out` mount. It expects to run as:
#
#   docker run --rm --read-only --cap-drop ALL --security-opt no-new-privileges --network none \
#     --user 10001:10001 -v <in>:/av-job/in:ro -v <out>:/av-job/out av-sweep-sample:local \
#     /usr/local/bin/av-sweep --run-sample --drm-pb /av-job/in/000 ... --out /av-job/out/run_products.pb
#
# The executors always pass their own fixed uid:gid 10001:10001; the image's own USER (a fixed,
# explicit, non-root uid 10003, never 0) applies only when the image is run directly. The files
# below are world-readable so either identity reads them.
#
# **There is deliberately NO ENTRYPOINT and no CMD.** The two executors treat a job's command
# differently: `docker run <image> <command...>` APPENDS the command to an image ENTRYPOINT, while a
# Kubernetes container's `command` REPLACES it. With an ENTRYPOINT here the same JobSpec.command
# worked under k8s and failed under docker (measured: the container ran
# `av-sweep /usr/local/bin/av-sweep --run-sample ...` and refused the extra argument). With none,
# one command line runs identically under both: the sample's command names the binary in full, at
# the path crates/av-sweep/src/bin/av-sweep/jobs.rs::SAMPLE_BINARY, which a unit test keeps equal
# to this file's COPY destination.
#
# NOT a kit component this round (the lead decides): this image is not in the air-gap kit.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

LABEL org.altavista.project="altavista"
LABEL org.altavista.component="sweep"

# libssl3 (runtime .so only) -- av-sweep links the system OpenSSL (ADR-004's crypto rule: SHA-256
# only, through the system OpenSSL; `openssl`/`av-http1`). Measured with `ldd` against the prebuilt
# binary inside this exact base (IMAGE_DIGEST.md): libssl.so.3, libcrypto.so.3, libgcc_s.so.1,
# libm.so.6, libc.so.6. No ca-certificates: a sample makes no TLS connection. This apt install is
# the one network use this Dockerfile makes (question 154's one permitted window).
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 10003 avsweep \
    && useradd --uid 10003 --gid avsweep --no-create-home --shell /usr/sbin/nologin avsweep

# The prebuilt, stripped binary (a build artifact, not a source file).
COPY out/sweep-image/bin/av-sweep /usr/local/bin/av-sweep
RUN chmod 0755 /usr/local/bin/av-sweep

# The four GMAT data files, laid out as $GMAT_ROOT/data/... (staged by build-image.sh from the GMAT
# install, hash-verified there).
COPY out/sweep-image/data/gmat/ /opt/av-sweep/gmat/
RUN chmod -R a+rX /opt/av-sweep
ENV GMAT_ROOT=/opt/av-sweep/gmat

# Last identity-affecting instruction (question 207).
USER avsweep

# (No ENTRYPOINT and no CMD -- see the header: the executors supply the whole command.)
