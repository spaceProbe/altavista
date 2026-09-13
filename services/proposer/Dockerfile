# services/proposer/Dockerfile (R3.3, docs/aiplane-plan.md milestone A4;
# docs/open-questions.md question 206's decision 11(a); questions 148/154/156/194/196(d)) --
# packages `av-proposer` (crates/av-proposer/src/bin/av-proposer.rs, A4b's deterministic
# rule-based station-keeping proposer) as a runnable, labelled image: `av-proposer:local`.
#
# Build from the REPOSITORY ROOT (matching services/edge-plugin/Dockerfile's,
# services/lockstep-ref/Dockerfile's and services/cfs/Dockerfile's own convention), by
# running services/proposer/build-image.sh -- NEVER `docker build` directly (see "Why this
# Dockerfile has no Rust builder stage" below for exactly why, proven, not assumed, for THIS
# crate specifically):
#
#   services/proposer/build-image.sh
#
# ------------------------------------------------------------------------------------------
# Why this Dockerfile has no Rust builder stage (root-caused, not guessed -- and worse here
# than for the edge plugin, not merely the identical wall)
# ------------------------------------------------------------------------------------------
# services/edge-plugin/Dockerfile's own header comment already root-causes this same wall
# for `av-edge-plugin`: the root workspace `Cargo.toml` pins `spoore-cdm` to an ABSOLUTE HOST
# PATH (`spoore-cdm = { path = "/Users/probe/code/spoore/crates/spoore-cdm" }`), which a
# `docker build`'s isolated context cannot reach, and this host's classic (non-BuildKit)
# builder has no host-path bind-mount mechanism at all to work around it (question 196(a)).
# `av-proposer` (crates/av-proposer/Cargo.toml) hits the IDENTICAL wall through TWO absolute
# host path dependencies, not one: `spoore-cdm.workspace = true` (the same one) AND
# `spoore-models = { path = "/Users/probe/code/spoore/crates/spoore-models" }` (D2's real
# `spoore_models::KalmanFilter` this crate's ModelService implementation genuinely runs, per
# decision 11(b) -- never a stub). Its own `build.rs` also reads
# `/Users/probe/code/spoore/proto/spoore/v0/model_service.proto` directly off the host
# filesystem at compile time (see that file's own module doc, "spoore's ModelService contract
# (D2), server only") -- a THIRD host-path dependency a `docker build` context cannot supply
# BuildKit or not. The accepted, already-precedented answer (`av-lockstep-shim`'s own original
# case, then `av-edge-plugin`'s) is identical regardless of how many host paths are involved:
# prebuild the binary OUTSIDE `docker build`, with a real `docker run` bind mount (which --
# unlike `docker build` -- has always supported host-path bind mounts), strip it, and `COPY`
# the resulting stripped binary into the image as a build artifact, never compiled by the
# Dockerfile itself. This Dockerfile follows that SAME precedent, automated (not hand-run) by
# services/proposer/build-image.sh's own "prebuild" step, which runs:
#
#   docker run --rm \
#     -v "$(pwd)":/workspace \
#     -v /Users/probe/code/spoore:/Users/probe/code/spoore:ro \
#     -w /workspace \
#     rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f \
#     bash -c 'sed -i "s|http://deb.debian.org|https://deb.debian.org|g" \
#         /etc/apt/sources.list.d/debian.sources 2>/dev/null || true && \
#       apt-get update -qq && apt-get install -y -qq --no-install-recommends \
#         protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && \
#       cargo build --release -p av-proposer --bin av-proposer \
#         --target-dir /workspace/target-docker-linux && \
#       strip /workspace/target-docker-linux/release/av-proposer'
#
# The `sed` above is an UNCONFIRMED workaround for a KNOWN, UNRESOLVED host defect (measured
# on this host, this task -- see services/proposer/build-image.sh's own comment at its
# identical line for the full writeup): `apt-get update` against deb.debian.org fails
# deterministically with "GPG error: ... At least one invalid signature was encountered", for
# every rust:*-bookworm image tried (1.85 and 1.90), over BOTH http:// and https:// (this sed
# appeared to fix it once, then failed 4/4 times immediately after -- kept anyway since https
# is strictly no worse, but it is NOT a confirmed fix). `apt -o Debug::Acquire::gpgv=1 update`
# shows the DEPRECATED `apt-key ... verify` wrapper exiting 1 with an entirely empty
# Good/Bad/Valid summary, while a DIRECT `gpgv --keyring debian-archive-keyring.gpg` against
# the identical, freshly-curled `InRelease` file reports "Good signature" for all three
# signers -- the keyring is not expired (`apt-key list`: every debian-archive-*-automatic key
# expires 2029-2031) and the file is not corrupt (stable sha256 across fetches). This looks
# like a broken `apt-key`/`gpgv` invocation inside this image on THIS host, not a network or
# trust-store problem this Dockerfile caused or can fix from inside a `RUN` line.
#
# Why `rust:1.90-bookworm`, not the workspace's own `rust:1.85-bookworm` pin (measured, not
# guessed -- a real build failure this task's own R3.3 gate hit and root-caused): unlike
# `av-edge-plugin`, `av-proposer` depends on `av-command` (for `av_command::clock::Clock`/
# `av_command::counters::Counters` -- `crates/av-proposer/Cargo.toml`'s own comment), and
# `av-command` depends on `regorus` (`crates/av-command/src/policy.rs`'s Rego evaluator).
# `regorus 0.12.0` (this workspace's own pinned `Cargo.lock` version, unrelated to this task's
# own changes -- confirmed with `git diff Cargo.lock` before reaching for a version bump)
# calls `Vec::len`/`Vec::is_empty`/`Vec::as_slice` from a `const fn` context
# (`regorus-0.12.0/src/lookup.rs`, `.../value/array/mod.rs`), which needs the
# `const_vec_string_slice` stabilization -- NOT YET STABLE at Rust 1.85 (the exact compiler
# error, reproduced against `rust:1.85-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4`
# while building THIS image for the first time: "`alloc::vec::Vec::<T, A>::len` is not yet
# stable as a const fn ... help: add `#![feature(const_vec_string_slice)]`"). `rust:1.90-
# bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f` (rustc
# 1.90.0, confirmed directly: `docker run --rm <image> rustc --version`) compiles `regorus`
# cleanly. This is a narrower fix than bumping the WORKSPACE's own `rust-version = "1.85"`
# (which many other Dockerfiles/CI assumptions key off): only THIS script's own cross-compile
# toolchain moves, and moving it newer is always safe for a crate whose own `rust-version` says
# "at least 1.85" (Rust's own backward-compatibility guarantee) -- `av-edge-plugin`'s own
# cross-build never hit this wall only because its dependency graph never reaches `av-command`
# (and therefore never reaches `regorus`) at all.
#
# (`protobuf-compiler`+`libprotobuf-dev`: this crate's OWN `build.rs` runs two independent
# `protoc` passes -- spoore's `model_service.proto` server, and this workspace's own
# `DataGatewayService`/`ModelProposeService` client via `tonic_build::manual` -- and
# `av-cdm`'s own `build.rs` (a transitive dependency's build script, still runs in this same
# prebuild container) needs it for `proto/altavista/v1/*.proto`; `av-cdm`/`av-proposer`'s
# `build.rs::resolve_protoc` fallback needs BOTH packages for the identical reason
# services/edge-plugin/Dockerfile's own header names -- `protobuf-compiler` alone ships
# `protoc` but not `google/protobuf/any.proto`. `libssl-dev`+`pkg-config` satisfy
# `openssl-sys`'s own system-OpenSSL discovery, ADR-004's crypto rule unaffected: still the
# system/distro OpenSSL, never a bundled one.) The resulting stripped binary is copied to
# `services/proposer/bin/av-proposer` (git-ignored -- a compiled artifact, exactly like
# `services/cfs/bin/av-lockstep-shim` and `services/edge-plugin/bin/av-edge-plugin`) and this
# Dockerfile's only job is to COPY it, its runtime shared libraries (satisfied by the base
# image's own packages, see below), and the committed A4b fixture (see "Self-contained and
# offline" below) into a minimal image. `target-docker-linux/` is already both `.gitignore`d
# and `.dockerignore`d, so it never bloats this Dockerfile's own build context either.
#
# ------------------------------------------------------------------------------------------
# Pinning (question 154: the one permitted network window is image build; digests, not
# floating tags, are what makes that window reproducible)
# ------------------------------------------------------------------------------------------
# `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171`
# -- the IDENTICAL runtime base `services/edge-plugin/Dockerfile` pins (same Debian 12
# "bookworm" userland, same glibc/OpenSSL 3.0.x ABI the prebuilt binary below is linked
# against -- `services/proposer/build-image.sh` re-verifies this image's own `ldd` output
# against THIS binary specifically rather than assuming the edge plugin's own measurement
# transfers unchanged). The prebuild base,
# `rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f`
# (workspace `rust-version = "1.85"`, edition 2021, but THIS crate's own dependency graph needs
# a newer cross-compile toolchain than that floor -- see "Why this Dockerfile has no Rust
# builder stage" above for the measured `regorus`/`const_vec_string_slice` reason), is used
# only by build-image.sh's own `docker run` above, never referenced by this file -- recorded
# here anyway so both digests this image's bytes ultimately depend on are pinned in one place.
#
# ------------------------------------------------------------------------------------------
# Labels (question 156: label every image; prune by label before creating)
# ------------------------------------------------------------------------------------------
# `org.altavista.project=altavista` -- this platform's own project label, shared with every
# other image this repository builds.
# `org.altavista.component=proposer` -- THIS component specifically. `services/proposer/
# build-image.sh` prunes by `label=org.altavista.component=proposer` before every build, and
# `tests/test_proposer_container.py` filters on the SAME label when it asserts (question 156's
# own rule) that no image/container/network IT created is left behind after it runs -- the
# persistent build artifact this Dockerfile produces is deliberately NOT swept by that test
# (it is not something the test created), only the test's own throwaway containers/networks/
# images (labelled `av.test`, `crates/av-lockstep/src/docker.rs`'s own established
# convention).
#
# ------------------------------------------------------------------------------------------
# Self-contained and offline: the committed A4b fixture is baked in, even though this image's
# own ENTRYPOINT never opens it directly
# ------------------------------------------------------------------------------------------
# Unlike the edge plugin (which replays a fixture it reads itself), `av-proposer`'s own
# propose-only mode reads run products over the network, from `av-gateway`'s own catalogue
# (D1: "a caller names a run by identity, never a path") -- this image's ENTRYPOINT never
# opens `run_products.pb` or any other fixture file, by design (D4a's own structural
# guarantee: this crate generates no `CommandAuthorityService` client and holds no catalogue
# of its own). The real, committed `tests/fixtures/demo_two_instance.runproducts.bin`
# (`run_id` `demo_two_instance_frozen_fixture`) is still baked in below, at a fixed, documented
# path -- purely for provenance: an operator or a later test inspecting this image (`docker
# run av-proposer:local --help`, or a `docker cp` of the path below) can see EXACTLY which
# real `RunProducts` fixture this image's own acceptance run (`tests/
# test_proposer_container.py`) queries `av-gateway` for via `--run-id
# demo_two_instance_frozen_fixture`, without depending on any file outside this image or any
# network fetch to answer that question later (question 154's "offline" reading, applied to
# provenance rather than to this image's own runtime behaviour, which needs no fixture file at
# all).
#
# ------------------------------------------------------------------------------------------
# Network posture at RUNTIME (this image says nothing about it -- see tests/
# test_proposer_container.py's own module doc for the full argument): this image never bakes
# in a `--gateway-endpoint` or any other deployment fact; every knob `av-proposer`'s own CLI
# accepts (`crates/av-proposer/src/bin/av-proposer.rs`'s own module doc: "every knob is a
# command-line argument, never an environment variable", question 199) is supplied at
# `docker run` time, as trailing arguments this Dockerfile's exec-form ENTRYPOINT appends
# nothing ahead of.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

LABEL org.altavista.project="altavista"
LABEL org.altavista.component="proposer"

# libssl3 (runtime .so only, no -dev/no compiler): the one package this base image is missing
# that the prebuilt binary's own `ldd` output names (services/proposer/build-image.sh checks
# this directly against THIS binary rather than assuming services/edge-plugin/Dockerfile's own
# measurement transfers unchanged). This is the one network use this Dockerfile itself makes
# (question 154's one permitted window); nothing else in this stage touches it. The `sed`
# below is the identical https:// workaround this file's own header comment documents for the
# prebuild step -- this base image's default http:// sources hit the identical measured GPG
# failure on this host.
RUN sed -i "s|http://deb.debian.org|https://deb.debian.org|g" /etc/apt/sources.list.d/debian.sources 2>/dev/null || true \
    && apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/proposer

# The prebuilt, stripped binary (see this file's own header comment for exactly how and why
# it is prebuilt, not compiled here) -- a build artifact, not a source file.
COPY services/proposer/bin/av-proposer /usr/local/bin/av-proposer
RUN chmod +x /usr/local/bin/av-proposer

# The committed A4b fixture -- baked in for provenance only, see this file's own header
# comment ("Self-contained and offline"). Not read by this image's own ENTRYPOINT.
COPY tests/fixtures/demo_two_instance.runproducts.bin /opt/proposer/fixtures/demo_two_instance.runproducts.bin

# ENTRYPOINT supplies nothing this image can supply on its own -- there is no default
# fixture path, signing key, or config for this binary to prepend (unlike the edge plugin);
# every one of `av-proposer`'s own required flags (`--gateway-endpoint`, `--run-id`,
# `--caller-clearance`, `--entity-id`, `--command-class`, `--score-name`,
# `--reference-radius-m`, `--threshold-m`, `--gain-per-s`, `--max-burn-mps`,
# `--model-node-id`, `--model-version`) is a `docker run <image> ...` trailing argument,
# exec-form ENTRYPOINT appending them after this list verbatim (question 199).
ENTRYPOINT ["/usr/local/bin/av-proposer"]
