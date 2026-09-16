# services/av-ingest/Dockerfile (D5b, docs/p5-plan.md's D5 "three placements" milestone;
# docs/open-questions.md questions 148/154/156/194/207/212) -- packages `av-ingest-server`
# (crates/av-ingest/src/bin/av-ingest-server.rs, the `engine`-tier component D5 places on
# three resources) as a thin, labelled runtime image, following `services/edge-plugin/
# Dockerfile`'s own established pattern for this exact situation (a cross-built-for-Linux
# Rust binary, no builder stage in this Dockerfile at all).
#
# Build from the REPOSITORY ROOT, by running services/av-ingest/build-image.sh -- NEVER
# `docker build` directly (see services/edge-plugin/Dockerfile's own header comment,
# "Why this Dockerfile has no Rust builder stage," for the structural reason: the root
# workspace Cargo.toml pins `spoore-cdm` to a host sibling path a `docker build`'s isolated
# context cannot see).
#
#   services/av-ingest/build-image.sh
#
# ------------------------------------------------------------------------------------------
# No prebuild step here -- the binary already exists, verified by digest
# ------------------------------------------------------------------------------------------
# Unlike `services/edge-plugin/Dockerfile` (whose build-image.sh cross-builds
# `av-edge-plugin` itself via a `docker run` bind mount), `av-ingest-server` was ALREADY
# cross-built for this round (round 4 commit 7c70ac8's own proof, restated in
# `services/av-ingest/build-image.sh`): `target-docker-linux-kit/av-ingest/release/
# av-ingest-server`, ELF 64-bit LSB pie executable, ARM aarch64, GNU/Linux 3.7.0, stripped,
# 2,765,064 bytes, sha256 0acdf73d7cf53f46fdd9c8065ae066d905e05d37b88b46f2577b97aee2b06315.
# `build-image.sh` verifies this size+digest itself, every run, before `docker build` ever
# sees the file -- if it does not match (or the file is missing), the script refuses rather
# than silently building from something else (question 212's own rule, applied one level
# earlier: not just "verify the built image", but "verify the input to the build too").
#
# ------------------------------------------------------------------------------------------
# Pinning (question 154: the one permitted network window is image build; digests, not
# floating tags -- `services/edge-plugin/Dockerfile`'s own header comment states this
# identically, and this image reuses the IDENTICAL base digest for the identical reason:
# same glibc/OpenSSL 3.0.x ABI the cross-built binary above was linked against)
# ------------------------------------------------------------------------------------------
# `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171`
# -- already present on this host (never pulled by this Dockerfile or its build script; see
# question 212's own rule, applied by `services/av-ingest/build-image.sh`'s own
# `_verify_base_image_digest`, mirroring `scripts/kit/build_kit.py::
# _verify_prebuild_base_image_digest`'s identical shape). `ldd` against the cross-built
# binary, run inside this exact base image (measured directly, not assumed): `libssl.so.3`/
# `libcrypto.so.3` are the only two NOT already satisfied by the base image itself
# (`libgcc_s.so.1`/`libm.so.6`/`libc.so.6`/`/lib/ld-linux-aarch64.so.1` all resolve already)
# -- `libssl3` (runtime .so only) below is what supplies them.
#
# ------------------------------------------------------------------------------------------
# Labels (question 156: label every image; prune by label before creating)
# ------------------------------------------------------------------------------------------
# `org.altavista.project=altavista` -- shared with every other image this repository builds.
# `org.altavista.component=av-ingest` -- THIS component. `services/av-ingest/build-image.sh`
# prunes by `label=org.altavista.component=av-ingest` before every build (question 207's
# host-wide docker lock held around it), and the D5 runner script
# (`scripts/kit/d5_three_placements.py`) filters on the SAME label, plus `av.test`/
# `av.test.run_id`/a per-placement label, for every CONTAINER/network/volume it creates from
# this image -- never the persistent image build artifact itself (not something a test run
# created).
#
# ------------------------------------------------------------------------------------------
# Network posture at RUNTIME (question 155/202: `av-ingest-server`'s own `--grpc-bind`/
# `--admin-bind` refuse a non-loopback bind outright -- measured directly by `tests/
# test_edge_plugin_container.py`'s own module doc: "binding ... to 0.0.0.0 ... is refused
# immediately with 'is not a loopback address'." This is deliberate ADR-004-level policy,
# not something this image works around: it is NEVER told to bind 0.0.0.0. Each of D5's
# three containers binds its OWN internal 127.0.0.1:50060 (gRPC) / 127.0.0.1:50160 (admin)
# -- distinct per-container loopbacks, never colliding with each other because each
# container has its own network namespace by default -- and `docker run -p
# 127.0.0.1:<host-port>:50060` (Docker's userland proxy, `EnableUserlandProxy: true` on
# this host -- measured via `docker info`) is what makes that internal loopback reachable
# from the host-side driver at a DISTINCT host port per placement, without the server
# itself ever binding anything but loopback. See `scripts/kit/d5_three_placements.py`'s own
# module doc for the empirical proof this actually works before it is relied on for the
# real measured runs.
#
# ------------------------------------------------------------------------------------------
# Container hardening (mirrors `services/edge-plugin/Dockerfile`'s own posture; question
# 207's ruling applies identically here: rootless podman / Quadlet / UBI9-FIPS are the
# PRODUCTION substrate's concern, not this development image's)
# ------------------------------------------------------------------------------------------
# This image expects to be run as (`scripts/kit/d5_three_placements.py` runs it exactly
# this way):
#
#   docker run --rm \
#     --read-only \
#     -v <log-dir-bind-mount>:/var/lib/av-ingest \
#     -v <verify-key-fixture>:/opt/av-ingest/keys/test_signing_key.pub.pem:ro \
#     --cap-drop ALL \
#     --security-opt no-new-privileges \
#     -p 127.0.0.1:<host-port>:50060 -p 127.0.0.1:<host-admin-port>:50160 \
#     av-ingest:local <trailing --flags>
#
#   * Non-root user: dedicated, unprivileged, non-login (`USER` is the last identity-
#     affecting instruction).
#   * Read-only root: the ONE writable path is `/var/lib/av-ingest` (declared VOLUME,
#     bind-mounted to a host directory under `out/` per placement -- Colima mounts home
#     directories only, and this worktree is under /Users/probe, so this works; verified by
#     the D5 runner before it is relied on).
#   * `--cap-drop ALL` / `--security-opt no-new-privileges` / default seccomp: identical
#     reasoning to `services/edge-plugin/Dockerfile`'s own -- this image needs no Linux
#     capability, is not setuid/setgid anywhere, and makes no unusual syscall.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

LABEL org.altavista.project="altavista"
LABEL org.altavista.component="av-ingest"

# libssl3 (runtime .so only, no -dev/no compiler): the one package this base image is
# missing that the cross-built binary's own `ldd` output (this file's header comment) names.
# This is the one network use this Dockerfile itself makes (question 154's one permitted
# window); nothing else in this stage touches it.
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends libssl3 \
    && rm -rf /var/lib/apt/lists/*

# A dedicated, fixed, explicit uid/gid -- mirrors services/edge-plugin/Dockerfile's own
# reasoning exactly (never a dynamically-allocated system id, never 0). 10002 (one past
# edge-plugin's own 10001) so the two images' dedicated users never collide if ever
# inspected side by side from the Colima host.
RUN groupadd --gid 10002 avingest \
    && useradd --uid 10002 --gid avingest --no-create-home --shell /usr/sbin/nologin avingest

WORKDIR /opt/av-ingest

# The already cross-built, stripped binary (see this file's own header comment for its
# recorded size/digest, verified by build-image.sh before this COPY ever runs) -- a build
# artifact, not a source file compiled by this Dockerfile.
COPY target-docker-linux-kit/av-ingest/release/av-ingest-server /usr/local/bin/av-ingest-server
RUN chmod +x /usr/local/bin/av-ingest-server

# The ONE writable path under `--read-only` (this file's own "Network posture"/"Container
# hardening" sections above). Created and owned by the dedicated non-root user now, before
# `USER` switches identity below, so the user that will actually own the bind-mounted host
# directory's contents already owns the mount point itself.
RUN mkdir -p /var/lib/av-ingest && chown avingest:avingest /var/lib/av-ingest
VOLUME ["/var/lib/av-ingest"]

# Last identity-affecting instruction (question 207): everything above needs root; nothing
# below it does. `av-ingest-server` (ENTRYPOINT) therefore always runs as uid/gid 10002,
# never as root, regardless of what `docker run` is told (short of `--user root`, which the
# run posture documented above never uses).
USER avingest

# ENTRYPOINT supplies nothing by default (unlike services/edge-plugin/Dockerfile, which
# bakes in its own fixture paths) -- every flag `scripts/kit/d5_three_placements.py` needs
# (`--grpc-bind`, `--admin-bind`, `--log-dir`, `--verify-key`, `--no-require-client-cert`,
# `--clock-tai-ns`, `--clearance-ladder`, `--max-batch-age-ns`) is supplied as trailing
# `docker run` arguments, exactly `av-ingest-server`'s own "every knob is a command-line
# argument, never an environment variable" convention (question 199), applied one level up
# to the container boundary too -- this image bakes in no server config of its own, no
# secret, no endpoint.
ENTRYPOINT ["/usr/local/bin/av-ingest-server"]
