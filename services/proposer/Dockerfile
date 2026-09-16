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
# for `av-edge-plugin`: the root workspace `Cargo.toml` pins `spoore-cdm` to a HOST PATH
# outside this repository -- a relative sibling path since question 219(c) (`spoore-cdm =
# { path = "../spoore/crates/spoore-cdm" }`), which a `docker build`'s isolated context
# cannot reach, and this host's classic (non-BuildKit) builder has no host-path bind-mount
# mechanism at all to work around it (question 196(a)). `av-proposer`
# (crates/av-proposer/Cargo.toml) hits the IDENTICAL wall through TWO spoore host path
# dependencies, not one: `spoore-cdm.workspace = true` (the same relative one) AND
# `spoore-models = { path = "/Users/probe/code/spoore/crates/spoore-models" }` (D2's real
# `spoore_models::KalmanFilter` this crate's ModelService implementation genuinely runs, per
# decision 11(b) -- never a stub) -- STILL an absolute host path, unlike spoore-cdm:
# question 219(b)/(c) asked every track to convert its own files, and crates/av-proposer is
# off-limits to the P5 track (the heavy team's remaining half). Its own `build.rs` also reads
# `/Users/probe/code/spoore/proto/spoore/v0/model_service.proto` directly off the host
# filesystem at compile time (see that file's own module doc, "spoore's ModelService contract
# (D2), server only") -- likewise still absolute, a THIRD host-path dependency a
# `docker build` context cannot supply BuildKit or not. The accepted, already-precedented
# answer (`av-lockstep-shim`'s own original case, then `av-edge-plugin`'s) is identical
# regardless of how many host paths are involved, or whether they're relative or absolute:
# prebuild the binary OUTSIDE `docker build`, with a real `docker run` bind mount (which --
# unlike `docker build` -- has always supported host-path bind mounts), strip it, and `COPY`
# the resulting stripped binary into the image as a build artifact, never compiled by the
# Dockerfile itself. This Dockerfile follows that SAME precedent, automated (not hand-run) by
# services/proposer/build-image.sh's own "prebuild" step, which runs (SPOORE_ROOT defaults to
# the sibling checkout `../spoore`, question 219(b); mounted TWICE -- once at the sibling of
# the workspace mount, for spoore-cdm's own relative resolution, and once at the fixed
# absolute path av-proposer's still-absolute dependencies expect, regardless of where
# SPOORE_ROOT itself lives on the host):
#
#   docker run --rm \
#     -v "$(pwd)":/workspace \
#     -v "${SPOORE_ROOT}":/spoore:ro \
#     -v "${SPOORE_ROOT}":/Users/probe/code/spoore:ro \
#     -w /workspace \
#     rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f \
#     bash -c 'apt-get update -qq && apt-get install -y -qq --no-install-recommends \
#         protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && \
#       cargo build --release -p av-proposer --bin av-proposer \
#         --target-dir /workspace/target-docker-linux && \
#       strip /workspace/target-docker-linux/release/av-proposer'
#
# ------------------------------------------------------------------------------------------
# SUPERSEDED: this section used to claim a "KNOWN, UNRESOLVED host defect" (a GPG-signature
# story) and carried an "UNCONFIRMED" `sed` workaround rewriting apt's sources to `https://`.
# Question 211 (the lead, 2026-09-15) root-caused the real failure from a real build log, and
# this task re-tested every claim on this host (2026-09-15) rather than carrying the old story
# forward. What is actually true, measured:
#
# - `rust:1.90-bookworm@sha256:3914072ca...` (this prebuild base) ALREADY ships
#   `ca-certificates` (measured directly: `docker run --rm <image> dpkg -l | grep
#   ca-certificates` -> `ii ca-certificates 20230311+deb12u1`; `ls /etc/ssl/certs | wc -l` ->
#   285 real certificate files). A `sed` rewriting its sources to `https://` therefore bought
#   nothing (apt's archive trust comes from the GPG-signed `Release`/`InRelease` file via
#   `debian-archive-keyring`, `Signed-By` in `/etc/apt/sources.list.d/debian.sources` --
#   entirely independent of whether the TRANSPORT is http or https) and was never the actual
#   fix for anything in THIS stage -- it is simply dropped, deleting an unconfirmed workaround
#   for a defect that, on re-test, this image does not have.
# - No GPG/`apt-key`/`gpgv` failure of any kind was reproduced here, against either the http or
#   https default, for this prebuild base -- the previous paragraph's claim was not
#   re-confirmed and is deleted as superseded rather than carried forward unverified.
# - The REAL, reproduced failure (see "Network posture" below and this file's own runtime-stage
#   comment) was entirely in the RUNTIME stage's `FROM debian:bookworm-slim`, which is a
#   different image with a different (empty) certificate store -- not this prebuild stage,
#   which has never been observed to fail this way.
#
# Why `rust:1.90-bookworm`, still newer than the workspace's own `rust-version` floor even
# now that question 208(a) has corrected that floor (measured, not guessed -- a real build
# failure this task's own R3.3 gate hit and root-caused): unlike `av-edge-plugin`,
# `av-proposer` depends on `av-command` (for `av_command::clock::Clock`/
# `av_command::counters::Counters` -- `crates/av-proposer/Cargo.toml`'s own comment), and
# `av-command` depends on `regorus` (`crates/av-command/src/policy.rs`'s Rego evaluator).
# `regorus 0.12.0` (this workspace's own pinned `Cargo.lock` version, unrelated to this task's
# own changes -- confirmed with `git diff Cargo.lock` before reaching for a version bump)
# calls `Vec::len`/`Vec::is_empty`/`Vec::as_slice` from a `const fn` context
# (`regorus-0.12.0/src/lookup.rs`, `.../value/array/mod.rs`), which needs the
# `const_vec_string_slice` stabilization. Question 208(a) binary-searched exactly where that
# stabilizes, on this host, with `cargo +<version> check -p av-command`: 1.86.0 FAILS with the
# same error `rust:1.85-bookworm@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4`
# hit building THIS image the first time ("`alloc::vec::Vec::<T, A>::len` is not yet stable as
# a const fn ... help: add `#![feature(const_vec_string_slice)]`"); 1.87.0 PASSES cleanly. The
# root workspace `Cargo.toml`'s `rust-version` is now `"1.87"` (measured, adjacent
# fail/pass pair), not the stale, never-actually-buildable `"1.85"` this comment used to cite.
# `rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f`
# (rustc 1.90.0, confirmed directly: `docker run --rm <image> rustc --version`) is still
# comfortably newer than that corrected 1.87 floor and still compiles `regorus` cleanly, so
# this prebuild pin needs no change -- only the reasoning above it does. This remains a
# narrower fix than the workspace `rust-version` bump alone would have been for THIS image:
# only THIS script's own cross-compile toolchain choice matters here, and moving it newer than
# whatever the workspace floor truthfully is stays always safe (Rust's own backward-
# compatibility guarantee) -- `av-edge-plugin`'s own cross-build never hit this wall only
# because its dependency graph never reaches `av-command` (and therefore never reaches
# `regorus`) at all.
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
# (workspace `rust-version = "1.87"`, measured by question 208(a) -- see "Why this Dockerfile
# has no Rust builder stage" above -- edition 2021; 1.90 stays newer than that corrected floor,
# so this pin needs no change), is used only by build-image.sh's own `docker run` above, never
# referenced by this file -- recorded here anyway so both digests this image's bytes ultimately
# depend on are pinned in one place.
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
# (question 154's one permitted window); nothing else in this stage touches it.
#
# Question 211 (the lead's ruling, 2026-09-15), root-caused from the lead's own build log and
# independently re-measured here on this host before this fix was written: a PREVIOUS version
# of this line rewrote this base image's default `http://deb.debian.org` sources to
# `https://deb.debian.org` (`sed ... 2>/dev/null || true`) before `ca-certificates` existed in
# `debian:bookworm-slim` -- measured directly: `docker run --rm debian:bookworm-slim@sha256:...
# dpkg -l | grep ca-certificates` returns NOTHING (not installed) and `ls /etc/ssl/certs | wc
# -l` returns `1` (an empty placeholder directory, no real certificates). Forcing `https://`
# with no certificate store made `apt-get update` print "No system certificates available" --
# a WARNING apt does not treat as fatal, so it still exited 0 -- while (measured on this host)
# leaving the package index unusable, so the very next step, `apt-get install libssl3`, failed
# with "Unable to locate package". Apt's own archive trust comes entirely from the GPG-signed
# `Release`/`InRelease` file (`debian-archive-keyring`, named by `Signed-By:` in this image's
# own `/etc/apt/sources.list.d/debian.sources`) -- completely independent of whether the
# TRANSPORT is http or https -- so the rewrite bought nothing and only added a failure mode.
# Fix: drop the rewrite outright (the lead's stated preference) and stay on this image's
# default `http://` sources, which need no certificate store at all. Both silent-failure
# shapes the previous line had are gone, not merely patched: `2>/dev/null || true` no longer
# exists to hide anything (there is no `sed` left to fail), and with no forced-https warning to
# swallow, `apt-get update`'s own exit code is now a true signal -- `&&` still stops this whole
# `RUN` (and therefore `docker build`) the instant either `apt-get update` or `apt-get install`
# returns non-zero, so a package index that cannot be fetched, or `libssl3` that cannot be
# located/installed, fails this build loudly, at this exact line, every time -- never a
# "successful" image silently missing its one required runtime library.
RUN apt-get update -qq \
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
# `--model-node-id`, `--model-version`, and -- R5.1/question 208(b), new this round --
# `--service-token-file <PATH>`) is a `docker run <image> ...` trailing argument, exec-form
# ENTRYPOINT appending them after this list verbatim (question 199).
#
# `--service-token-file <PATH>` (REQUIRED, R5.1): this proposer's own service-subject OIDC
# token, read from a FILE inside the container -- never `--service-token <VALUE>`, since a
# flag VALUE is visible in `ps` (even inside a container's own pid namespace) and in
# `docker inspect <container>`'s own recorded `Config.Cmd`/`Args`. A real deployment supplies
# it with a bind mount or a secret mount, e.g.:
#   docker run -v /path/to/service-token:/run/secrets/av-proposer-token:ro <image> \
#     --service-token-file /run/secrets/av-proposer-token ...
# An absent or empty token file makes this binary refuse to start with a typed error --
# invariant B's fail-closed guarantee: there is no "no token supplied" default that means
# "proceed unauthenticated." `tests/test_proposer_container.py` mounts one this same way,
# under `.av-test-tmp/` (Colima mounts only `$HOME`, never `/tmp`).
ENTRYPOINT ["/usr/local/bin/av-proposer"]
