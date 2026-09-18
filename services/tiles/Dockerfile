# services/tiles/Dockerfile (H4's remaining round-2 open item 6, docs/heavy-plan.md: "a
# docker-gated proof of av-tiles serving out of a real MinIO" -- extended this round to prove
# the GATEWAY itself running as a container, not merely the store-backed `ObjectSource` it
# reads through) -- packages `av-tiles` (crates/av-tiles/src/bin/av-tiles.rs, H4's tile
# gateway binary) as a runnable, labelled image.
#
# Build from the REPOSITORY ROOT (matching services/edge-plugin/Dockerfile's,
# services/proposer/Dockerfile's, services/lockstep-ref/Dockerfile's and
# services/cfs/Dockerfile's own convention), by running services/tiles/build-image.sh --
# NEVER `docker build` directly (see "Why this Dockerfile has no Rust builder stage" below
# for exactly why a plain `docker build -f services/tiles/Dockerfile .` cannot work here --
# this is the SAME wall services/edge-plugin/Dockerfile's own header comment already proved,
# cited here rather than rediscovered):
#
#   services/tiles/build-image.sh
#
# ------------------------------------------------------------------------------------------
# Why this Dockerfile has no Rust builder stage (cited, not rediscovered)
# ------------------------------------------------------------------------------------------
# `services/edge-plugin/Dockerfile`'s own header comment root-causes this wall in full: the
# root workspace `Cargo.toml` pins `spoore-cdm` (an `av-cdm` -> `av-tiles` dependency, exactly
# on THIS crate's own path -- `crates/av-tiles/Cargo.toml` depends on `av-cdm` directly) to a
# HOST PATH outside this repository (`spoore-cdm = { path = "../spoore/crates/spoore-cdm" }`,
# question 219(c), resolved against wherever the workspace root is mounted) -- a real, compiled
# dependency (`av-cdm`'s own `spoore.v0` adapter names `spoore_cdm` types directly), which a
# `docker build`'s isolated context cannot reach, and this host's classic (non-BuildKit)
# builder has no host-path bind-mount mechanism at all to work around it (question 196(a): no
# `buildx` on this host). Worse: Cargo has to load EVERY workspace member's manifest to
# resolve the workspace at all, regardless of which `-p` target is actually being built --
# `crates/av-proposer`'s own manifest included, and THAT crate's `spoore-models`/`spoore-ml`
# dependencies are still absolute `/Users/probe/code/spoore/...` host paths (the heavy team's
# remaining half of question 219(b)/(c), off-limits to this track) -- so even a `-p av-tiles`
# build fails at manifest-load time on a path this crate itself never touches, exactly as
# `services/edge-plugin/build-image.sh`'s own `CONTAINER_WORKSPACE` comment measured for
# `av-edge-plugin`. The accepted, already-precedented answer (`av-lockstep-shim`'s original
# case, then `av-edge-plugin`'s, then `av-proposer`'s) is identical here: prebuild the binary
# OUTSIDE `docker build`, with a real `docker run` bind mount (which -- unlike `docker
# build` -- has always supported host-path bind mounts), strip it, and `COPY` the resulting
# stripped binary into the image as a build artifact, never compiled by the Dockerfile itself.
# This Dockerfile follows that SAME precedent, automated (not hand-run) by
# services/tiles/build-image.sh's own "prebuild" step, which runs (mounting this repository at
# a container path whose PARENT is literally `/Users/probe/code` -- question 219(c) round 4's
# fix, reused verbatim from services/edge-plugin/build-image.sh's own `CONTAINER_WORKSPACE`
# comment for the full measured account of why a second mount/symlink does not work: it makes
# spoore's own sibling mount destination equal the fixed absolute path av-proposer's still-
# absolute dependencies expect, by construction, so the ONE mount below satisfies both routes
# to `spoore-cdm` even though `av-tiles` itself never uses spoore for anything):
#
#   docker run --rm \
#     -v "$(pwd)":/Users/probe/code/AltaVista-edge \
#     -v "${SPOORE_ROOT:-$(pwd)/../spoore}":/Users/probe/code/spoore:ro \
#     -w /Users/probe/code/AltaVista-edge \
#     rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f \
#     bash -c 'apt-get update -qq && apt-get install -y -qq --no-install-recommends \
#         protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && \
#       cargo build --release -p av-tiles --bin av-tiles \
#         --target-dir target-docker-linux && \
#       strip target-docker-linux/release/av-tiles'
#
# (`protobuf-compiler`+`libprotobuf-dev` satisfy `crates/av-cdm/build.rs`'s own protoc
# discovery -- `av-tiles` depends on `av-cdm` for its wire types (`TileSetManifest`/
# `TileEntry`); `libssl-dev`+`pkg-config` satisfy `openssl-sys`'s system-OpenSSL discovery --
# `av-tiles` depends on `openssl` directly (SHA-256 verification, ADR-004) and transitively
# through `av-store`'s `hyper-openssl` S3 client.)
#
# ------------------------------------------------------------------------------------------
# Pinning (question 154: the one permitted network window is image build; digests, not
# floating tags, are what makes that window reproducible)
# ------------------------------------------------------------------------------------------
# `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171`
# and the prebuild base `rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f`
# are the IDENTICAL two digests `services/edge-plugin/Dockerfile` and
# `services/proposer/Dockerfile` already pin -- reused deliberately, not re-resolved: both are
# already measured (that Dockerfile's own "Pinning" section) to be the same Debian 12
# "bookworm" userland the prebuild toolchain links against (same glibc, same OpenSSL 3.0.x
# ABI), and `rust:1.90-bookworm` is already the floor this workspace's `rust-version = "1.87"`
# requires (question 208(a)'s regorus/const_vec_string_slice measurement). Reusing an
# already-verified pin instead of re-resolving a third one is deliberate: fewer distinct base
# images on this host for Colima's own image-GC pressure to have to keep evicting and
# re-pulling (questions 196(d)/205).
#
# ------------------------------------------------------------------------------------------
# Labels (question 156: label every image; prune by label before creating)
# ------------------------------------------------------------------------------------------
# `org.altavista.project=altavista` (shared with every image this repository builds) and
# `org.altavista.component=tiles` (THIS component specifically) -- `services/tiles/
# build-image.sh` prunes by the second before every build, and `tests/test_tiles_container.py`
# never mistakes this persistent build artifact for one of its own throwaway `av.test`-labelled
# resources (see that file's own module doc for the same split `services/edge-plugin/
# Dockerfile`'s own "Labels" section already establishes).
#
# ------------------------------------------------------------------------------------------
# Network posture at RUNTIME -- the opposite shape from services/edge-plugin/Dockerfile's,
# stated explicitly so a reader does not assume the two images share a network story
# ------------------------------------------------------------------------------------------
# `av-edge-plugin` is a CLIENT that dials exactly one outbound endpoint and accepts no inbound
# connection at all -- its own Dockerfile's expected run posture is `--network none` plus that
# one endpoint. `av-tiles` is the opposite shape: a SERVER that accepts inbound HTTP on `--bind`
# and dials out to whatever `--store-endpoint` names (a real S3-API backend -- MinIO in every
# test this workspace has, a real S3-compatible endpoint in production). This image therefore
# needs BOTH a route to its configured store endpoint AND a reachable listening port -- there is
# no `--network none` posture for it, and this Dockerfile does not claim one.
#
# One consequence worth stating plainly, because it is easy to get wrong when moving from a
# host subprocess to a container: `crates/av-tiles/src/bin/av-tiles.rs`'s own `DEFAULT_BIND`
# (`127.0.0.1:50073`) and `crate::admin::DEFAULT_ADMIN_BIND` (`127.0.0.1:50173`) are both
# LOOPBACK addresses, correct for a bare host subprocess but a trap inside a container: a
# process bound only to a container's own `127.0.0.1` is unreachable through Docker's own port
# publishing (`-p`), because a published port's traffic arrives over the container's `eth0`,
# never over `lo` -- the kernel will not deliver it to a socket bound solely to loopback. A
# deployment that runs this image as a container and wants its ports reachable (through `-p`,
# a Kubernetes Service, or any other L3/L4 forwarding) MUST override both with `--bind
# 0.0.0.0:<port> --admin-bind 0.0.0.0:<port>` as trailing arguments -- `tests/
# test_tiles_container.py`'s own containerised fixture does exactly this, and its own module
# doc names this trap explicitly (measured, not assumed: the naive default-bind run was tried
# first and its published port answered nothing).
#
# Every secret/config knob (`--oidc-issuer`, `--oidc-public-key-path`, `--store-access-key-id`,
# `--store-secret-access-key`, ...) is supplied at `docker run` time as a trailing argument or a
# bind-mounted file -- never baked into this image, mirroring `av-tiles`' own binary doc's
# "every knob is a command-line argument, never an environment variable" rule (question 199),
# applied one level up to the container boundary too, exactly like `services/edge-plugin/
# Dockerfile`'s identical statement for its own image.
#
# ------------------------------------------------------------------------------------------
# Container hardening (question 207's ruling, applied here -- and ONE property stronger than
# services/edge-plugin/Dockerfile's own posture, not merely copied)
# ------------------------------------------------------------------------------------------
# `av-edge-plugin` needs exactly one writable path for its opt-in durable buffer
# (`/var/lib/edge-plugin`, that Dockerfile's own `VOLUME`). `av-tiles` needs NONE: grepped
# directly against every non-test line of `crates/av-tiles/src/**` (`std::fs::`/`File::`/
# `OpenOptions`), the only filesystem call in this crate's own production code is
# `std::fs::read(&public_key_path)` in `src/bin/av-tiles.rs::main` -- one READ of the
# bind-mounted OIDC public key at startup. No log file, no cache, no temp file, no state
# directory, anywhere in this crate's own code path (`crate::counters::Counters` is in-process
# memory only, read back over the admin HTTP surface, never persisted to disk). This image
# therefore declares NO `VOLUME` at all: `--read-only` applies to the WHOLE container root
# filesystem with no exception, which is a STRONGER posture than the edge plugin's own image
# (which still needs one declared writable mount) -- `tests/test_tiles_container.py`'s own
# hardening assertions call `altavista.container_hardening.assert_inspect_hardening`/
# `assert_exec_hardening` with no `writable_mount_dest`/`writable_path` at all (that module was
# extended this round to make the writable-mount check OPTIONAL, exactly for an image like this
# one that has none to assert), rather than manufacturing an unused volume purely to keep the
# assertion's old required-parameter shape.
#
# This image expects to be run as:
#
#   docker run --rm \
#     --read-only \
#     --cap-drop ALL \
#     --security-opt no-new-privileges \
#     av-tiles:local <trailing --flags, including --bind 0.0.0.0:<port> if published>
#
# (no `--security-opt seccomp=...` at all, for the identical reason services/edge-plugin/
# Dockerfile's own header states: the DEFAULT seccomp profile Docker applies whenever no
# override is given is what is asserted; naming it explicitly, even as "the default", would
# defeat the point of proving no override is needed).
#
#   * Non-root user: this image creates and runs as a dedicated, unprivileged, non-login user
#     (see the `groupadd`/`useradd` below) -- `USER` is this Dockerfile's own last
#     identity-affecting instruction, so ENTRYPOINT never runs as root.
#   * Read-only root, with NO exception: see above.
#   * `--cap-drop ALL`: this image needs no Linux capability at all (it binds only non-
#     privileged ports -- `50073`/`50173`, both far above 1024 -- opens no raw socket, changes
#     no ownership).
#   * `--security-opt no-new-privileges`: nothing in this image is setuid/setgid; belt-and-
#     suspenders against any future accidental setuid binary landing in the image.
#   * Default seccomp profile: this image makes no unusual syscall.
#
# NOT implemented here, and why (production substrate's concern, per question 207's ruling --
# identical reasoning to services/edge-plugin/Dockerfile's own "NOT implemented here" section,
# not repeated a second time): rootless podman, Quadlet-managed units, and a UBI9 FIPS base
# image are all ADR-003/secdeploy's job on the real deployment substrate, not reproducible with
# Docker on Colima (no podman, no Quadlet, no UBI9 FIPS entitlement on this development host),
# and this Dockerfile does not attempt them.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

LABEL org.altavista.project="altavista"
LABEL org.altavista.component="tiles"

# libssl3 (runtime .so only, no -dev/no compiler) -- the one package this base image is
# missing that `av-tiles`' own direct `openssl` dependency (SHA-256 verification, ADR-004) and
# `av-store`'s transitive `hyper-openssl` S3 client both need at runtime, measured with `ldd`
# against the prebuilt binary run inside this exact base (identical library set to
# `services/edge-plugin/Dockerfile`'s own measurement: `libssl.so.3`, `libcrypto.so.3`,
# `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `/lib/ld-linux-aarch64.so.1`).
#
# `ca-certificates` -- NOT needed by any test this workspace runs (every test endpoint is
# plain `http://`, so `av_store::StoreConfig`'s TLS path is never exercised here), but a real
# production deployment can point `--store-endpoint` at an `https://` S3-compatible backend
# with no `--store-ca-file` override, which `av_store::client`'s own doc says falls back to
# "OpenSSL's own default trust store" -- a claim that is only true if this image actually ships
# one. Installed here so that fallback is real rather than aspirational; this is the one
# network use this Dockerfile itself makes (question 154's one permitted window).
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# A dedicated, fixed, explicit uid/gid -- never a dynamically-allocated system id and never
# uid/gid 0 (see services/edge-plugin/Dockerfile's own identical comment for the full
# reasoning). 10002 (not 10001, `av-edge-plugin:local`'s own uid) purely so the two images'
# fixed ids never collide if ever inspected side by side from the Colima host -- they are
# already isolated by separate container filesystems regardless, this is belt-and-suspenders
# for a human reading `ps`/`inspect` output across both.
RUN groupadd --gid 10002 avtiles \
    && useradd --uid 10002 --gid avtiles --no-create-home --shell /usr/sbin/nologin avtiles

WORKDIR /opt/tiles

# The prebuilt, stripped binary (see this file's own header comment for exactly how and why it
# is prebuilt, not compiled here) -- a build artifact, not a source file.
COPY services/tiles/bin/av-tiles /usr/local/bin/av-tiles
RUN chmod +x /usr/local/bin/av-tiles

# Last identity-affecting instruction (question 207): installing libssl3/ca-certificates,
# creating the user, and copying+chmodding the binary all need root; nothing below does.
# `av-tiles` (ENTRYPOINT) therefore always runs as uid/gid 10002, never as root, regardless of
# what `docker run` is told to do (short of `--user root`, which the run posture documented
# above never uses).
USER avtiles

# ENTRYPOINT supplies nothing of its own: unlike `av-edge-plugin` (which bakes in a fixed demo
# fixture and config), every one of `av-tiles`' own flags is deployment-specific (issuer,
# audience, key path, ladder, key prefix, store endpoint/credentials/bucket, bind addresses) --
# there is no sensible image-baked default for any of them, so all of them are supplied as
# trailing `docker run` arguments, exactly this crate's own "every knob is a command-line
# argument" rule (question 199) applied at the container boundary.
ENTRYPOINT ["/usr/local/bin/av-tiles"]
