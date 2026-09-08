# Recorded image digest (M23.2, docs/open-questions.md question 154)

Built once, locally, with the one permitted network window (package installs and
`third_party/fetch-cfs.sh`'s pinned cFS clone, both inside `services/cfs/Dockerfile`). **Not
pushed to any registry** -- `docker image inspect`'s own content-addressed image ID is what is
recorded and re-checked, exactly as `crates/av-lockstep/src/docker.rs`'s own module doc comment
describes for the `services/lockstep-ref` image ("No image this task builds is ever pushed to a
real/external registry"). `crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s own
`BINDING_KIND_CONTAINER` tests separately tag and push this same already-built image to a
throwaway *local* (loopback-only) registry so `av_lockstep::docker::ManagedContainer::
pull_and_run`'s own `docker pull <image>@<digest>` has something real to resolve -- that is a
different digest (a registry manifest digest, not this content-addressed image ID) and is never
recorded here; see that test file's own module doc comment.

```
docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .
docker image inspect altavista-cfs-lockstep:local --format '{{.Id}}'
```

Recorded digest (re-pinned 2026-09-08 for M25.4a / question 179 -- see "Re-pinned 2026-09-08"
below for the named cause -- `third_party/cfs` still pinned at
`088b2fa828db9ff7e00733f1908e0eeb59f66ce3`, see `third_party/fetch-cfs.sh`):

```
sha256:29eb1bec64b7e826de157468c79dd82103b80d42b7debc5fa809727dea994cde
```

Previous digest (M24.4b, 2026-09-06):
`sha256:27ed4ff89dee381258a9ccb8410fa8aae19312defc79bf815ddc2e2c509d21ae`.

Previous digest (M24.3, this file's own record was stale by the time M24.4b started --
confirmed live: `docker image inspect altavista-cfs-lockstep:local` returned
`sha256:8d538e787baa26ea1b0956cb50f06daae06b6689be96dfa355a18c6ed96b8601` before M24.4b's own
rebuild, matching neither this nor the recorded value below -- some intervening, undocumented
local build already existed on this host): `sha256:baac4535533094bbfddced863779714345519e71d9be0d2218212186f4151b82`.

Previous digest (M26.1 rename, superseded above): `sha256:3bae6444cbb371a79d2e4b327c21b03764c57778c215ddf4b82614642c1f5c0d`.

**As of M25.4a (question 179) the image is built ONLY by `services/cfs/build-image.sh`**, by
hand, once -- that script is the one network window question 154 permits. It also writes
`services/cfs/IMAGE_CONTEXT_MANIFEST.txt`: the SHA-256 of every host file the Dockerfile's
`COPY` steps read from (directories expanded recursively, the COPY list parsed out of the
Dockerfile itself so it cannot drift in a second hardcoded place). `services/cfs/tests/
test_image_digest.py` no longer builds anything: it inspects an already-built image, skips
visibly (naming `build-image.sh`) when Docker is absent or the image is not built, and on a
mismatch prints which manifest entries changed so drift is attributable rather than merely
detected. A second, non-Docker-gated test asserts the manifest itself is still accurate.

## Re-pinned 2026-09-08 (M25.4a, `docs/open-questions.md` question 179): the cause, named

Question 179 asked why the pin drifted "with an untouched source tree". It is not the build
context, and the manifest proves it rather than a mtime argument alone: the build-context
manifest written by this same build (38 file entries across the 11 host `COPY` paths) matches
the tree, `git status` reports no modification to any COPYed path, and the only commit to touch
`services/cfs/` since the previous pin (`8191ead`) merely started *tracking* three
`services/cfs/build/*.cmake` files that a stray unanchored `build/` line in `.gitignore` had
been swallowing -- it changed no bytes.

**The cause is that this image is not reproducible by construction, and cFE says so itself.**
`third_party/cfs/cfe/cmake/generate_build_env.cmake` lines 15-23:

```
set(BUILDDATE $ENV{BUILDDATE})
if (NOT BUILDDATE)
    execute_process(COMMAND date "+%Y%m%d%H%M" OUTPUT_VARIABLE BUILDDATE ...)
endif(NOT BUILDDATE)
```

with upstream's own comment directly above it: "All 3 of these may be passed via environment
variables to force a particular date, user, or hostname i.e. if hoping to reproduce an exact
binary of a prior build." `BUILDDATE`, `BUILDUSER` and `BUILDHOST` are linked into cFE's
`CONFIGDATA` object. `services/cfs/Dockerfile` sets none of them (checked: no `BUILDDATE`,
`BUILDUSER`, `BUILDHOST` or `SOURCE_DATE_EPOCH` anywhere in the Dockerfile or under
`services/cfs/build/`), so **every build that actually re-executes the builder stage bakes the
current minute into `cpu1`, and the image's content-addressed `.Id` changes even when every
COPYed byte is identical.** Two rebuilds in quick succession agree -- which is exactly the
"deterministic (same digest twice)" observation question 179 recorded -- because they either
land in the same minute or are served whole from Docker's layer cache; a rebuild after the
cache has been evicted does not.

The base image was excluded as the cause rather than assumed: the local `ubuntu:22.04` resolves
to `sha256:2edbbc5dc405...`, created 2026-08-10, i.e. before the previous pin, and this build's
log shows the second-stage `apt-get` layer served from cache while the builder stage genuinely
re-ran.

**Recommendation, not done here (it changes the image, so it is the lead's call):** set
`BUILDDATE`, `BUILDUSER` and `BUILDHOST` to fixed values in `services/cfs/Dockerfile`, rebuild
once, and pin that. The image would then be reproducible from an identical build context and
the digest would stop drifting on cache eviction. Until that is decided, the pin above is
correct for the image on this host and a future mismatch is attributable via the manifest.

## M23.4 changes since the digest above was first recorded

M23.4 (docs/sil-plan.md's M23 exit criterion: "the loop closes in the container with a scored
objective") compiled `services/cfs/apps/adcs` into this image for the first time and closed a
real attitude control loop through it end to end. Doing so surfaced six real defects -- found
only by actually running the compiled apps against a real kernel `Step`, not by static reading
of any of them alone -- each now fixed, each recorded at its own fix site with a full account:

1. `CFE_ES_RegisterApp()` does not exist in the pinned cFE 7.0.1 core API --
   `services/cfs/apps/adcs/fsw/src/adcs_app.c`'s own `ADCS_AppInit`.
2. `adcs_app.h`'s software-bus message structs assumed a full `CFE_MSG_TelemetryHeader_t`/
   `CommandHeader_t` envelope ahead of each packet; `io_lockstep` actually transmits the bare
   CCSDS primary header with no envelope at all -- `adcs_app.h`'s own top comment.
3. `services/cfs/psp-lockstep` (`psp_lockstep`) held mutable state that
   `services/cfs/apps/io_lockstep` and `services/cfs/apps/sch_lockstep` must share, but each is
   an independently `dlopen()`ed cFE app module and a `STATIC` library gives each module its own
   private copy of that state -- `services/cfs/apps/io_lockstep/CMakeLists.txt`'s own top
   comment (now built `SHARED` and installed once).
4. `services/cfs/apps/sch_lockstep`'s wakeup dispatch used `OS_TimerAdd`'s own interval-counting
   callback mechanism in a way that fired **~100,000 times** for a single 100 ms kernel step --
   `sch_lockstep_app.c`'s own top comment (now drives the wakeup by directly polling
   `psp_lockstep_tick_count()` instead).
5. `io_lockstep_port_table.c`'s `wheel_torque_out` entry subscribed to the bare APID (300), but
   a command-type CCSDS packet's real cFE "V1" MsgId also has the command "type" bit (0x1000)
   folded in -- `io_lockstep_port_table.c`'s own top comment and `CCSDS_V1_MSGID` macro.
6. `io_lockstep_app.c`'s own `handle_step` blocked `CFE_SB_PEND_FOREVER` on every declared
   `LOCKSTEP_PORT_FROM_BUS` port on every tick, but the reference ADCS app (mirroring the
   native controller's own "emits nothing before the first measurement arrives" rule)
   legitimately produces no output at all until its first tick with both a star tracker and an
   IMU measurement already received -- a real closed-loop demo's own first tick or two,
   deadlocking the whole run forever -- `io_lockstep_app.c`'s own top comment (now a bounded
   `IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` wait; a timeout is a legitimate "nothing to report this
   tick" outcome, not a protocol error).

Also (M23.4's own reconciliation, not a defect fix): the CCSDS codec that used to live at
`services/cfs/apps/io_lockstep/fsw/{inc,src}/ccsds_codec.{h,c}` moved to
`services/cfs/apps/shared/ccsds/` so `io_lockstep` and `adcs` share one implementation instead
of each carrying an independent copy (see that header's own doc comment); `services/cfs/tests/
test_ccsds_golden.py`'s `CODEC_SRC`/`CODEC_INC` were updated to the new path.

Runtime smoke-checked (not part of the automated test, recorded here for the reviewer): a
`docker run` of this image (with a real `av-lockstep-shim`/peer present via
`services/cfs/container-entrypoint.sh`) needs `--sysctl fs.mqueue.msg_max=256 --sysctl
fs.mqueue.msgsize_max=65536` on this Docker Desktop host -- cFE's core `CFE_SB`/`CFE_EVS` pipes
use POSIX message queues, and this host's default `fs.mqueue.msg_max` is too low for cFE's own
default pipe depth. This is a host/Docker-runtime constraint, not a defect in this image's own
apps; `av_lockstep::docker::ManagedContainer::pull_and_run`'s own `extra_sysctls` parameter
(added by M23.4) is how a `BINDING_KIND_CONTAINER` instance now declares this
(`container.sysctl.*` parameters, `crates/av-kernel/src/drm/binding.rs`). With that sysctl set
and a real shim/peer, a full `Bind` -> several `Step`s round trip against this exact image
closes the loop end to end (`crates/av-kernel/tests/drm_attitude_control_cfs.rs`).

## Rebuilt 2026-09-06 for M26.1 (the `gmatviz` -> `altavista` rename)

The rename touched 13 files inside this image's own build context under `services/cfs/`, so the
image content genuinely changed and its content-addressed ID changed with it. Previous digest:
`sha256:abe1631c2d8e156ac34dd9aff05761695ad5820a2a2130506ba23c588334c684` (recorded for M23.4's
reconciliation fixes). New digest recorded above.

Recorded by the manager during M26.1 review. M26.1's own report attributed this test failure to a
pre-existing, unrelated cause; that was wrong -- the failing assertion names exactly this case
("if this is an intentional change, rebuild and update that file"), and the rename is the change.
Worth keeping in mind for any future task that edits anything under `services/cfs/`: it moves this
digest, and the digest test is the thing that tells you so.

## Rebuilt 2026-09-06 for M24.3 (cross-building cFS for `zynqmp_rpu_lock_step`)

M24.3 (docs/open-questions.md questions 144/147/148/154/157) is scoped to
`third_party/rtems-container/` and cross-build glue under `services/cfs/build/`, but three files
this Dockerfile's own `COPY` steps DO pick up were touched too, exactly the kind of change this
file's own previous note warned about:

- `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`: `connect_to_shim`/its includes are
  now wrapped in `#ifdef AV_CFS_LOCKSTEP_TRANSPORT_UART` (the RTEMS build has no network stack,
  so it opens a UART character device instead of an AF_UNIX socket -- see
  `third_party/rtems-container/M24_3_REPORT.md`'s "Transport" section). This macro is only
  defined by the RTEMS toolchain file, never by this (posix) Dockerfile, so the `#else` branch --
  byte-for-byte the original AF_UNIX code -- is what actually compiles into this image;
  behavior is unchanged, but the file's content (hence this image's content-addressed ID) is not.
- `services/cfs/apps/io_lockstep/CMakeLists.txt` and `services/cfs/apps/sch_lockstep/CMakeLists.txt`:
  `psp_lockstep`'s library type (`SHARED` vs `STATIC`) and its `pthread` link dependency are now
  behind `if(RTEMS)`/`if(NOT RTEMS)` guards, needed for the RTEMS static-app build (see the
  M24_3_REPORT.md "Build attempts" section). `RTEMS` is unset for this (posix) build, so both
  guards resolve to their original branches (`SHARED`, and `pthread` linked) -- again unchanged
  behavior, changed file content.

Rebuilt and re-tagged (`docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .`,
network already available per question 154's one-time exception, `third_party/cfs` already
fetched and pinned so no new clone happened). New digest recorded above. Not run/smoke-tested
again for this rebuild specifically (M23.4's own runtime smoke check already covers this same
code path with the `#else`/`if(NOT RTEMS)` branches taken, which is exactly what's in this image);
`services/cfs/tests/test_image_digest.py` is what actually re-verifies the digest match going
forward.

## Rebuilt 2026-09-06 for M24.4b (`third_party/renode/M24_4b_REPORT.md`)

**Found stale on arrival, exactly as this task's own brief warned:** `docker image inspect
altavista-cfs-lockstep:local --format '{{.Id}}'` returned
`sha256:8d538e787baa26ea1b0956cb50f06daae06b6689be96dfa355a18c6ed96b8601` at the start of this
task -- matching neither the M24.3 value this file had recorded
(`sha256:baac4535533094bbfddced863779714345519e71d9be0d2218212186f4151b82`) nor anything else in
this file's own history, meaning some undocumented local build had already run on this host since
M24.3's own record was written. `services/cfs/tests/test_image_digest.py` was already the one
known failure in this repository's test suite for this reason, unrelated to anything M24.4b itself
did, before this task changed a single line.

**What this task additionally changed under `services/cfs/`** (on top of the pre-existing
staleness above): `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c` -- the root-caused
UART handshake fix (`third_party/renode/M24_4b_REPORT.md`'s own full account): `connect_to_shim`'s
`AV_CFS_LOCKSTEP_TRANSPORT_UART` branch now calls `tcgetattr`/`tcsetattr` to put the port in raw,
blocking mode, fixing a real defect (RTEMS termios's `rawInBufSemaphoreWait` never being set
because nothing ever called `tcsetattr`). Exactly the same shape M24.3's own note above already
disclosed for this identical file: this new code lives entirely inside `#ifdef
AV_CFS_LOCKSTEP_TRANSPORT_UART`, a macro only the RTEMS cross-toolchain file ever defines, never
this (posix) `Dockerfile` -- so the `#else` branch (byte-for-byte the original AF_UNIX code) is
still what actually compiles into this image, and this image's own runtime behavior is unchanged.
Also added: `services/cfs/tests/test_clean_fetch_patches.py` (question 148's clean-fetch patch
test) -- a new file under `services/cfs/tests/`, not copied into the image itself by any
`Dockerfile` `COPY` step (host-buildable, like every other file already in that directory), so it
does not contribute to the image's own content hash, but is noted here for completeness.

Rebuilt and re-tagged (`docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .`
from the repository root, real run, `docker build` exited 0 and reused Docker's own layer cache
for every unaffected stage -- confirmed by reading the full build log, not by exit code alone).
New digest recorded above. Verified with `services/cfs/tests/test_image_digest.py` immediately
after updating this file (see that test's own run below) -- **PASS**, closing the one known
failure in the suite this task's brief named.
