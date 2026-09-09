# services/cfs/Dockerfile (M23.2, docs/open-questions.md questions 147/148/154; docs/sil-plan.md
# M23) -- NASA cFS, pinned by third_party/fetch-cfs.sh, built for OSAL-posix ("SIMULATION=native")
# with CI_LAB/TO_LAB/SCH_LAB replaced by services/cfs/apps/io_lockstep and
# services/cfs/apps/sch_lockstep (the lockstep I/O app and the lockstep scheduler app -- question
# 143's decision: "a lockstep platform-support layer and scheduler app we maintain ... cFS
# otherwise unmodified").
#
# Network use here is the one-time, explicitly-permitted exception (question 154): building this
# image needs the network once (apt packages, and third_party/fetch-cfs.sh's clone of the pinned
# cFS bundle). No image built from this Dockerfile is ever pushed to a real/external registry.
#
# Build from the REPOSITORY ROOT (this Dockerfile COPYs `services/cfs` and `third_party/fetch-cfs.sh`,
# both relative to the repo root, matching services/lockstep-ref/Dockerfile's own convention):
#
#   docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .
#
# M23.4 UPDATE: this image now DOES run `crates/av-lockstep-shim` -- question 153's own decision
# ("a small Rust lockstep shim process ... runs beside the flight software in the same
# container") means a `BINDING_KIND_CONTAINER` instance dialing this image's published port must
# reach a real `LockstepService` server *inside* the same container `docker run` starts, and
# `av_lockstep::docker::ManagedContainer` only ever starts one container per instance (no pod/
# sidecar orchestration exists in this workspace) -- so the shim has to live in this image, not
# a second one. `services/cfs/container-entrypoint.sh` starts both processes in the right order
# (the shim first, then cFS once the shim's own Unix socket exists -- `io_lockstep`'s single,
# non-retried connect attempt cannot race it).
#
# The shim binary itself (`services/cfs/bin/av-lockstep-shim`, a stripped linux/aarch64 ELF) is
# PREBUILT, not compiled by this Dockerfile -- `crates/av-lockstep-shim` depends on `av-cdm`,
# which depends on `spoore-cdm` via the workspace `Cargo.toml`'s own ABSOLUTE host path
# dependency (`/Users/probe/code/spoore/crates/spoore-cdm`, this task's own file ownership
# excludes `~/code/spoore` and the root `Cargo.toml`) -- a plain `docker build` has no access to
# that host path, and vendoring/rewriting the dependency is out of this task's scope. Rebuild it
# with (from the repository root, on this same host, where that path already resolves):
#
#   docker run --rm \
#     -v "$(pwd)":/workspace \
#     -v /Users/probe/code/spoore:/Users/probe/code/spoore:ro \
#     -w /workspace rust:1.85-bookworm \
#     cargo build --release -p av-lockstep-shim --target-dir /workspace/target-docker-linux
#   docker run --rm -v "$(pwd)"/target-docker-linux/release:/b ubuntu:22.04 \
#     sh -c "apt-get update -qq && apt-get install -y -qq binutils && strip -o /b/av-lockstep-shim-stripped /b/av-lockstep-shim"
#   cp target-docker-linux/release/av-lockstep-shim-stripped services/cfs/bin/av-lockstep-shim
#
# (`target-docker-linux/` is `.dockerignore`d -- only the copied-out, stripped binary under
# `services/cfs/bin/` is ever part of this Dockerfile's build context.) This binary links only
# libc/libgcc_s/libm (confirmed with `ldd` against this same `ubuntu:22.04` base -- the crate's
# own doc comment already notes it pulls no TLS stack at all), so the final stage below needs no
# extra runtime package for it.
#
# docs/open-questions.md question 185 (round 5): both stages pin the SAME base image by content
# digest (`ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc`,
# resolved 2026-09-08 via `docker pull ubuntu:22.04` -- the one permitted network window,
# question 154 -- and confirmed identical to the value services/cfs/IMAGE_DIGEST.md's own R4.3
# section already recorded as "the local ubuntu:22.04" resolution, so pinning it changes nothing
# about which bytes either stage starts from). Two consequences: (1) `FROM ubuntu:22.04` was a
# floating tag that could re-resolve to different bytes on a future pull -- pinning it removes
# that as a reproducibility variable entirely, not just "usually agrees"; (2) the final stage no
# longer needs its own `apt-get install ... libc6` (removed below) -- it starts from the exact
# same base rootfs as the builder stage, which already carries a `libc6` your own compiled
# binaries linked against at build time, so there is no version-skew risk to cover with a
# redundant reinstall. That `apt-get` layer was independently confirmed non-deterministic across
# `--no-cache` builds (services/cfs/R4_3_REPORT.md section 4: differing layer diffID on two
# `--no-cache` runs of an unchanged package list) and was the sole cause of at least one digest
# move even between two builds that reused 100% of the builder stage's cache
# (services/cfs/R4_3_REPORT.md section 6's "Mid-task incident"). Verified, not merely assumed
# safe (services/cfs/R5_3_REPORT.md has the full account): `ldd` against both
# `/cfs/av-lockstep-shim` and `/cfs/cpu1/core-cpu1` inside the built final-stage image, run
# without the `apt-get install libc6` line present, resolves every shared library with no
# "not found" entries.

FROM ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc AS builder

# Reproducibility (docs/open-questions.md question 182, following on from question 179's own
# root cause recorded in services/cfs/IMAGE_DIGEST.md's "Re-pinned 2026-09-08" section):
# third_party/cfs/cfe/cmake/generate_build_env.cmake bakes a build date, user, and hostname into
# cFE's CONFIGDATA object, defaulting to `date +%Y%m%d%H%M`/`whoami`/`hostname` whenever the
# corresponding environment variable is unset -- that file's own top comment: "may be passed via
# environment variables to force a particular date, user, or hostname ... if hoping to reproduce
# an exact binary of a prior build." Pinned here to fixed, deliberately-chosen values so two
# builds from an identical build context produce byte-identical CONFIGDATA instead of drifting
# on the wall-clock minute or the builder host's identity. `ENV` (not `ARG`) is required: these
# must be real environment variables so `RUN make native_std.compile` below (which is what
# actually invokes cFE's cmake and runs generate_build_env.cmake, per that file's own "runs at
# build time, not prep time" comment) inherits them via getenv() -- an `ARG` is build-time-only
# and is not implicitly exported into a later RUN's process environment.
#
# NOTE the env-var names generate_build_env.cmake actually reads do NOT all match cFE's own
# CONFIGDATA field names -- confirmed by reading that file's full 48 lines, not just its
# BUILDDATE excerpt: `set(BUILDDATE $ENV{BUILDDATE})` reads env var BUILDDATE, but
# `set(BUILDHOST $ENV{HOSTNAME})` reads env var HOSTNAME (not BUILDHOST) and
# `set(BUILDUSER $ENV{USER})` reads env var USER (not BUILDUSER). Pinning `ENV BUILDHOST=...` /
# `ENV BUILDUSER=...` here would compile but have no effect. USER/HOSTNAME are otherwise unused
# anywhere else in third_party/fetch-cfs.sh or the fetched third_party/cfs/ tree (grepped).
ENV BUILDDATE=202609080000
ENV USER=altavista
ENV HOSTNAME=altavista-build

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake git ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY third_party/fetch-cfs.sh third_party/fetch-cfs.sh
# The one permitted network window (question 154): clones the pinned cFS bundle
# (third_party/fetch-cfs.sh's own header comment records the exact commit and why).
RUN sh third_party/fetch-cfs.sh

COPY services/cfs/apps/io_lockstep third_party/cfs/apps/io_lockstep
COPY services/cfs/apps/sch_lockstep third_party/cfs/apps/sch_lockstep
COPY services/cfs/apps/adcs third_party/cfs/apps/adcs
COPY services/cfs/apps/shared third_party/cfs/apps/shared
COPY services/cfs/psp-lockstep third_party/cfs/psp-lockstep

# Mission-config overrides (services/cfs/build/*.cmake's own header comments explain why each
# one replaces a fetched sample_defs/ file -- the fetched bundle's own sample_defs/ hardcodes
# CI_LAB/TO_LAB/SCH_LAB/sample_app/hs/cf/md/mm/cs/fm/lc/sc/ds/hk, none of which this task fetches
# or wants; this is a mission-config customization, the sample_defs/ directory's own intended
# purpose, not a patch to cFE/OSAL/PSP core -- question 148's "record a cFE/OSAL/PSP patch"
# does not apply to it).
COPY services/cfs/build/targets.cmake third_party/cfs/sample_defs/targets.cmake
COPY services/cfs/build/generate_startup.cmake third_party/cfs/sample_defs/generate_startup.cmake
COPY services/cfs/build/cpu1_install_custom.cmake third_party/cfs/sample_defs/cpu1/install_custom.cmake
# docs/open-questions.md question 185's `-Wl,--build-id=none` fix lives inside
# services/cfs/build/targets.cmake above, NOT in a separate services/cfs/build/
# global_build_options.cmake wired through cFE's own OPTIONAL "global-scope build customization"
# hook -- that was tried first and it broke the build; see targets.cmake's own comment (right
# after its ENABLE_UNIT_TESTS line) for the full root-caused account of why, and
# services/cfs/R5_3_REPORT.md section 4 for the 9-build investigation.

WORKDIR /build/third_party/cfs
# Target the single `native_std` config explicitly (`native_std.<goal>`) -- a bare `prep`/`all`
# expands to *every* CONFIG_NAMES entry in target-configs.mk (RTEMS/VxWorks cross builds, the
# EDS build, standalone OSAL/EdsLib), none of which this task's fetched tree (cfe/osal/psp plus
# three build-tool submodules only -- see third_party/fetch-cfs.sh) can build.
RUN make native_std.prep
RUN make native_std.compile
RUN make native_std.install

FROM ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc
# No `apt-get install libc6` here (question 185; see this file's header comment for the
# hypothesis and the `ldd`-based verification that removing it is safe): this stage now pins the
# exact same base image, by digest, as the builder stage above, so it already has the libc6 that
# shipped in that base rootfs -- the same one the builder stage's own compiled binaries linked
# against, since both are byte-identical copies of the same pinned image. The
# `apt-get update && apt-get install libc6` layer this replaces was independently
# non-deterministic (services/cfs/R4_3_REPORT.md section 4) and contributed nothing this final
# stage's shipped binaries actually needed beyond what the base image already provides.
COPY --from=builder /build/third_party/cfs/build-native_std/exe/cpu1 /cfs/cpu1
COPY services/cfs/bin/av-lockstep-shim /cfs/av-lockstep-shim
COPY services/cfs/container-entrypoint.sh /cfs/container-entrypoint.sh
RUN chmod +x /cfs/av-lockstep-shim /cfs/container-entrypoint.sh
ENTRYPOINT ["/cfs/container-entrypoint.sh"]
