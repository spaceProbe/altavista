# services/edge-plugin/Dockerfile (E4b, docs/edge-plan.md milestone E4: "the plugin as a
# labelled container"; docs/open-questions.md questions 148/154/156/194/196(d)) -- packages
# `av-edge-plugin` (crates/av-ingest-client/src/bin/av-edge-plugin.rs, E4a's plugin binary)
# as a runnable, labelled image.
#
# Build from the REPOSITORY ROOT (matching services/lockstep-ref/Dockerfile's and
# services/cfs/Dockerfile's own convention), by running services/edge-plugin/build-image.sh
# -- NEVER `docker build` directly (see "Why this Dockerfile has no Rust builder stage"
# below for exactly why a plain `docker build -f services/edge-plugin/Dockerfile .` cannot
# work here, proven, not assumed):
#
#   services/edge-plugin/build-image.sh
#
# ------------------------------------------------------------------------------------------
# Why this Dockerfile has no Rust builder stage (root-caused, not guessed)
# ------------------------------------------------------------------------------------------
# The brief for this task asked for a multi-stage Dockerfile: a Rust builder stage running
# `cargo build --release -p av-ingest-client --bin av-edge-plugin`, then a minimal runtime
# stage. That literal shape is IMPOSSIBLE under a plain `docker build` on this host, for the
# same structural reason `services/cfs/Dockerfile`'s own header comment already documents
# for `av-lockstep-shim`: the root workspace `Cargo.toml` pins `spoore-cdm` (an `av-cdm` ->
# `av-edge`/`av-grpc` -> `av-ingest-client` dependency, exactly on `av-edge-plugin`'s own
# path) to an ABSOLUTE HOST PATH, `spoore-cdm = { path = "/Users/probe/code/spoore/crates/
# spoore-cdm" }` -- a real, compiled dependency (not just a manifest fact: `av-cdm`'s own
# `spoore.v0` adapter names `spoore_cdm` types directly), which a `docker build`'s isolated
# context has no access to. Measured directly (this task's own R1 investigation): a bare
# `RUN cargo build --release -p av-ingest-client --bin av-edge-plugin` inside a builder
# stage, given the full repository as its build context, fails immediately with
#
#   error: failed to load manifest for dependency `av-cdm`
#   Caused by:
#     failed to load manifest for dependency `spoore-cdm`
#   Caused by:
#     failed to read `/Users/probe/code/spoore/crates/spoore-cdm/Cargo.toml`
#   Caused by:
#     No such file or directory (os error 2)
#
# `docker build`'s classic (non-BuildKit) builder has no host-path bind-mount mechanism at
# all -- and per question 196(a), THIS HOST HAS NO BUILDKIT `buildx` (confirmed again while
# building this image: `docker build` prints legacy "Step N/M" output, never BuildKit's own
# framed output), so even BuildKit's `--build-context name=/host/path` escape hatch (which
# would otherwise let a Dockerfile `COPY --from=name` a host directory) is unavailable here.
# `av-lockstep-shim` hit this exact wall and the accepted, documented answer was: prebuild
# the binary OUTSIDE `docker build`, with a real `docker run` bind mount (which -- unlike
# `docker build` -- has always supported host-path bind mounts, BuildKit or not), strip it,
# and COPY the resulting stripped binary into the image as a build artifact, never compiled
# by the Dockerfile itself. This Dockerfile follows that SAME precedent, automated (not
# hand-run) by services/edge-plugin/build-image.sh's own "prebuild" step, which runs:
#
#   docker run --rm \
#     -v "$(pwd)":/workspace \
#     -v /Users/probe/code/spoore:/Users/probe/code/spoore:ro \
#     -w /workspace \
#     rust:1.85-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4 \
#     bash -c 'apt-get update -qq && apt-get install -y -qq --no-install-recommends \
#         protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && \
#       cargo build --release -p av-ingest-client --bin av-edge-plugin \
#         --target-dir /workspace/target-docker-linux && \
#       strip /workspace/target-docker-linux/release/av-edge-plugin'
#
# (`protobuf-compiler`+`libprotobuf-dev` satisfy `crates/av-cdm/build.rs::resolve_protoc`'s
# own PATH fallback plus its well-known-types include-dir search, which under Debian needs
# BOTH packages -- `protobuf-compiler` alone ships `protoc` but not `google/protobuf/
# any.proto`, measured directly the first time this recipe ran, printing exactly the error
# that build.rs's own doc names: "could not find google/protobuf/any.proto ... under any
# of [...]". `libssl-dev`+`pkg-config` satisfy `openssl-sys`'s own system-OpenSSL discovery,
# ADR-004's crypto rule unaffected: still the system/distro OpenSSL, never a bundled one.)
# The resulting stripped binary is copied to `services/edge-plugin/bin/av-edge-plugin`
# (git-ignored -- a compiled artifact, exactly like `services/cfs/bin/av-lockstep-shim`) and
# this Dockerfile's only job is to COPY it, its runtime shared libraries (satisfied by the
# base image's own packages, see below), the committed E4a fixture, and a default plugin
# config into a minimal image. `target-docker-linux/` is already both `.gitignore`d and
# `.dockerignore`d (added for `services/cfs/Dockerfile`'s identical scratch dir), so it
# never bloats this Dockerfile's own build context either.
#
# ------------------------------------------------------------------------------------------
# Pinning (question 154: the one permitted network window is image build; digests, not
# floating tags, are what makes that window reproducible -- services/cfs/Dockerfile's own
# header comment states this identically)
# ------------------------------------------------------------------------------------------
# `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171`
# (resolved via `docker pull debian:bookworm-slim`, matching `rust:1.85-bookworm`'s own
# Debian 12 "bookworm" userland byte for byte -- same glibc, same OpenSSL 3.0.x ABI the
# prebuilt binary above was linked against, confirmed with `ldd` against the prebuilt
# binary run inside this exact base: `libssl.so.3`, `libcrypto.so.3`, `libgcc_s.so.1`,
# `libm.so.6`, `libc.so.6`, `/lib/ld-linux-aarch64.so.1` -- all five resolve once `libssl3`
# is installed below; nothing else the binary links is missing). The prebuild base,
# `rust:1.85-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4`
# (workspace `rust-version = "1.85"`, edition 2021), is used only by build-image.sh's own
# `docker run` above, never referenced by this file -- recorded here anyway so both digests
# this image's bytes ultimately depend on are pinned in one place.
#
# ------------------------------------------------------------------------------------------
# Labels (question 156: label every image; prune by label before creating)
# ------------------------------------------------------------------------------------------
# `org.altavista.project=altavista` -- this platform's own project label, shared with any
# future image this repository builds, so a host-wide "everything AltaVista built" prune is
# possible without naming every component.
# `org.altavista.component=edge-plugin` -- THIS component specifically. `services/
# edge-plugin/build-image.sh` prunes by `label=org.altavista.component=edge-plugin` before
# every build (removing any dangling image a previous run's now-superseded `av-edge-plugin:
# local` tag left behind), and `tests/test_edge_plugin_container.py` filters on the SAME
# label when it asserts (question 156's own rule) that no image/container/network it
# created is left behind after it runs -- the persistent build artifact this Dockerfile
# produces is deliberately NOT swept by that test (it is not something the test created),
# only the test's own throwaway containers/networks (labelled `av.test`, `crates/
# av-lockstep/src/docker.rs`'s own established convention -- see that test file's module doc
# for the exact split).
#
# ------------------------------------------------------------------------------------------
# Network posture at RUNTIME (this image says nothing about it -- see tests/
# test_edge_plugin_container.py's own module doc for the full argument): this image never
# bakes in a signing key, a server CA, or an --endpoint; every one of those is supplied at
# `docker run` time (bind-mounted files, command-line arguments appended after this
# Dockerfile's own ENTRYPOINT), exactly `av-edge-plugin`'s own module doc's "every knob is a
# command-line argument, never an environment variable" rule (question 199), applied one
# level up to the container boundary too.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

LABEL org.altavista.project="altavista"
LABEL org.altavista.component="edge-plugin"

# libssl3 (runtime .so only, no -dev/no compiler): the one package this base image is
# missing that the prebuilt binary's own `ldd` output (above) names. This is the one
# network use this Dockerfile itself makes (question 154's one permitted window); nothing
# else in this stage touches it.
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/edge-plugin

# The prebuilt, stripped binary (see this file's own header comment for exactly how and why
# it is prebuilt, not compiled here) -- a build artifact, not a source file.
COPY services/edge-plugin/bin/av-edge-plugin /usr/local/bin/av-edge-plugin
RUN chmod +x /usr/local/bin/av-edge-plugin

# The committed E4a fixture (crates/av-edge/tests/fixtures/ground_segment/README.md: 900s
# at 1Hz, `demo_ground_segment`, pinned batch count 900 and chain head
# d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698) -- baked in so this
# image is a self-contained, offline replay of the demo DRM (question 154: no network at
# run time, and this fixture is exactly what makes that possible).
COPY crates/av-edge/tests/fixtures/ground_segment/run_products.pb /opt/edge-plugin/fixtures/run_products.pb
COPY crates/av-edge/tests/fixtures/ground_segment/port_traffic.pb /opt/edge-plugin/fixtures/port_traffic.pb

# The demo DRM's one pinned PluginConfig (crates/av-edge/tests/generate_edge_plugin_default_
# config.rs generates this file byte-for-byte from the same config `tests/plugin_replay.rs`
# and `crates/av-ingest/tests/plugin_wire.rs` already build and pin -- see that generator's
# own module doc for how to regenerate it if that config ever changes).
COPY services/edge-plugin/plugin-config.default.json /opt/edge-plugin/plugin-config.default.json

# ENTRYPOINT supplies every knob this image can supply on its own (the fixture paths and
# the default config); `--signing-key` (a secret, never baked into an image) and
# `--endpoint` (a deployment fact, never baked into an image) are NOT defaulted here --
# `docker run <image> --signing-key /keys/signing.pem --endpoint <host:port>` (or
# `--endpoint https://... --server-ca ...`) supplies them as trailing arguments, which
# ENTRYPOINT-form (exec, not shell) appends after this list verbatim.
ENTRYPOINT ["/usr/local/bin/av-edge-plugin", \
    "--run-products", "/opt/edge-plugin/fixtures/run_products.pb", \
    "--port-traffic", "/opt/edge-plugin/fixtures/port_traffic.pb", \
    "--plugin-config", "/opt/edge-plugin/plugin-config.default.json"]
