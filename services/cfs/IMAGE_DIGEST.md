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

**`docker events` capture (docs/open-questions.md question 194, round 6, second half).**
`services/cfs/build-image.sh` now starts a live `docker events --format '{{json .}}' --filter
type=image --filter type=container` capture before doing anything else Docker-related, and stops
it only at the very end of its own run (a `trap`-based cleanup covers early/failure exits too, so
a failed build is never hidden by, nor does it hide, whatever the events capture caught up to that
point). Written to `services/cfs/build/last-build-events.jsonl` -- a fixed name, overwritten each
run, reflecting the **most recent** `build-image.sh` invocation's own execution window (not an
ever-growing history), matching how this file's own top section always reflects the current pin.
When the log is finalised, `exec_create`/`exec_start`/`exec_die`/`health_status` actions from
unrelated containers are dropped (they made up 77% of the first real capture and cannot tag,
untag or delete an image); no image event is ever dropped, whatever its action -- see the round-6
section below for the measurement that motivated this and the numbers after the fix.
A failure to start or write the capture is logged as a `WARNING` in the script's own output and
never fails the build. See `services/cfs/R6_4_REPORT.md` section 5 for the full design rationale,
including a directly-measured finding that this host's Docker daemon retains only a very short
(tens-of-seconds, under load from unrelated containers on this shared host) in-memory `docker
events` history -- a *retrospective* `docker events --since` query is not a reliable substitute
for a *live* capture started ahead of time, which is why this exists as a wrapper around the build
rather than a query run after the fact.

## Runtime-content hash definition (docs/open-questions.md question 185, round 5)

In addition to the whole-image digest above (`docker image inspect`'s content-addressed `.Id`,
which depends on Docker/BuildKit internals and image metadata, not only on shipped file
content), this file also records a **runtime-content hash**: a hash of exactly the files this
image actually ships and runs, independent of any Docker/BuildKit implementation detail.

**Runtime-content set** -- every file at these image-internal paths (paths as they exist inside
the built container, not host paths):
- `/cfs/av-lockstep-shim`
- `/cfs/container-entrypoint.sh`
- every regular file under `/cfs/cpu1`, recursively (as of this round, with cFE's unit-test/
  coverage build turned off -- `services/cfs/build/targets.cmake` -- this directory should
  contain nothing else: no `coverage-*-testrunner` / `*-test` / `*_UT` harness binaries)

These three roots are exactly what `services/cfs/container-entrypoint.sh` (this image's own
`ENTRYPOINT`) reads and executes: it runs `/cfs/av-lockstep-shim` and `exec`s
`/cfs/cpu1/core-cpu1`, which `dlopen()`s the mission app/PSP `.so` modules and reads the table/
startup files also under `/cfs/cpu1` -- see that script's own top comment.

**Runtime-content hash** -- SHA-256 over the UTF-8 bytes of: for every file in the runtime-content
set, one line `"<path> <sha256>"` (image-internal path, one space, lowercase hex SHA-256 of that
file's bytes, no trailing path metadata), sorted lexicographically by the full line, joined by
`"\n"`, with a trailing `"\n"` after the last line. Reported as `sha256:<hex>`.

Computed by two independent implementations that must agree (docs/open-questions.md question
164's captured-artifact precedent -- two implementations of one defined algorithm, so a bug in
one is unlikely to be masked by the same bug in the other):
- `services/cfs/build-image.sh` (bash: `docker run --rm --entrypoint sh ... find/sha256sum` on
  the just-built official image), which records the value here, beside the whole-image digest,
  by hand (this script never edits this file, exactly as it has never auto-edited the whole-image
  digest above).
- `services/cfs/tests/test_image_reproducibility.py`'s `runtime_content_hash()` (Python), used to
  assert the hash is EQUAL between that test's own two independent `--no-cache` builds -- an
  always-on assertion (whenever that opt-in test actually runs) that is never weakened. As of
  round 6 (question 190) it is also the **only** hard-asserted reproducibility guarantee there:
  the whole-image digest assertion is retired to reported-not-asserted, because the one bounded
  BuildKit + `SOURCE_DATE_EPOCH` experiment question 190 authorised could not be attempted at all
  on this host (the Docker CLI's `buildx` component is absent and cannot be installed without
  network). See `services/cfs/R6_4_REPORT.md` and question 190.

Recorded digest (re-pinned 2026-09-09 by the round-6 manager after the **third** disappearance of
this tag, see "Re-pinned 2026-09-09 (round 6, after the third tag disappearance)" below for what
was rebuilt, what it measured, and what has now been root-caused about the disappearances
themselves -- `third_party/cfs` still pinned at
`088b2fa828db9ff7e00733f1908e0eeb59f66ce3`, see `third_party/fetch-cfs.sh`):

```
sha256:c1b727066421202082991afb8e2697d65571e796166f70cb7510a76cad273b0f
```

Recorded runtime-content hash for this pin (question 185, see the definition above):
```
sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef
```

## Re-pinned 2026-09-09 (round 6, after the third tag disappearance)

**What happened.** At the start of round 6 the `altavista-cfs-lockstep:local` tag was gone from
this host's Docker store for the third time, and this time its digest-pinned `ubuntu:22.04` base
was gone with it. Verified by the manager directly, not taken from a report: `docker image
inspect` failed for both, and `docker image ls -a` showed **105 images of which only 16 carried
any tag at all** -- every surviving tag belonging to unrelated projects on this shared host
(supabase, rancher/k8s, `registry:2`, `python:3.13-slim`), and every AltaVista-related tag gone
while the underlying image data survived as `<none>:<none>`. The images were **untagged, not
deleted**.

**Root cause of the untagging, now proven rather than suspected.**
`av_lockstep::docker::prune_stale_test_resources` (`crates/av-lockstep/src/docker.rs`) collects
image **IDs** with `docker images -q --filter label=av.test` and then runs `docker rmi -f <ID>`
on each. R4.3 had probed this suspect and excluded it, but the probe tested `docker rmi -f <TAG>`,
which is a different command with different semantics from the one the code actually runs. Probed
directly this round, on throwaway images created for the probe (script and output in the round-6
manager's scratchpad):

- Built one image, gave it two tags of its own, ran `docker rmi -f <IMAGE ID>` -- output
  `Untagged: av-r6-probe-a:...`, `Untagged: av-r6-probe-b:...`, `Deleted: sha256:...`. **One
  `rmi -f` on an ID removes EVERY tag on that image, not only the one the caller knows about.**
- The label filter itself is not the leak: an image's base is *not* returned by
  `docker images -q --filter label=av.test` (checked against the probe image's own base, which
  was not in the list), so a base image can only be caught this way by sharing an image ID with
  a labelled one.

So the sweep is capable of stripping tags it did not create, in direct violation of question
185's amendment ("a test suite must never retag, push or remove an image tag it did not create in
that run"). The remaining link -- exactly which labelled ID the two lost tags sat on -- is not
established, and with the daemon's event history already flushed there is no path to establishing
it retrospectively for the disappearances that already happened; the `docker events` capture this
script now writes is what will attribute the next one. Recorded here and escalated; the fix to
`prune_stale_test_resources` is not made in this section.

**The rebuild, and what it measured.** The base was restored by `docker pull
ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc` (question
154's permitted build-fetch window, the same one R5.3 used) -- the pulled digest matched the
Dockerfile's own pin exactly -- and the image rebuilt with `services/cfs/build-image.sh`.

- New image ID: `sha256:c1b727066421202082991afb8e2697d65571e796166f70cb7510a76cad273b0f`
  (recorded above). The previous pin,
  `sha256:b1300c6fd3c0e323feae1be0db3f3c4b740ea84229c510ceaae7ee0ed5f8ad12`, is superseded.
- Runtime-content hash:
  `sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef` -- **identical to
  R5.3's own value**, and now confirmed across the strongest perturbation anyone has applied to
  it: the base image itself was evicted from this host and re-pulled from the registry between
  the two measurements. That is a fourth independent confirmation, by a third code path, that
  the runtime-content hash is stable while the whole-image digest is not -- the evidence question
  190's decision rests on.
- `services/cfs/build-image.sh` run a second time immediately afterwards produced the
  **identical** image ID `sha256:c1b7270664...`, so this pin is stable against a re-run and not
  an artefact of one invocation.
- `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` regenerated: 38 entries, unchanged in count.

**A defect in this round's own `docker events` capture, found in review and fixed.** The first
real capture wrote 93 events of which **72 (77%) were `exec_create`/`exec_start`/`exec_die` from
unrelated always-on containers' health checks on this shared host**, burying the image events the
capture exists to preserve at roughly 4:1. `stop_events_capture` now drops those actions (and
`health_status`) when it finalises the log, never dropping an image event whatever its action --
an exec inside an already-running container cannot tag, untag or delete an image. Measured after
the fix on a real run: **6 of 78 captured events kept**, and the one `image tag` event for
`altavista-cfs-lockstep:local` is among them, which is precisely the class of event an untag has
to be attributed against. The filtering deliberately happens at finalisation rather than in a
pipe, because `$!` on a pipeline names the last command, so killing it would have left `docker
events` itself running as an orphan.

**Manifest regeneration, 2026-09-09 (round-5 manager's acceptance run), and what it measured.**
R5.3's own last edit to `services/cfs/build/targets.cmake` was a documentation-only comment
(the root-caused account of the `global_build_options.cmake` build failure) written AFTER its
final `services/cfs/build-image.sh` run, so the manifest it had already written recorded the
pre-comment hash and `test_manifest_paths_exist_and_hash_match` failed in the acceptance gate,
naming that one file -- exactly the attributable-drift behaviour question 179 asked that test
for. Fixed by re-running `services/cfs/build-image.sh` once (never a hand-rolled `docker
build`), which regenerated the manifest and re-pinned the digest above.

The measurement this accidentally produced is worth keeping, because it is the first direct
evidence that the runtime-content hash does the job question 185 defined it for: a
**documentation-only** change to a COPYed build-input file
- moved the whole-image digest (`sha256:3477865d...` -> `sha256:9bcb253d...`), because the
  changed `COPY` invalidated the layer and every layer after it, and
- left the runtime-content hash **exactly unchanged** (`sha256:5049bf8f...`), because not one
  byte of what the image actually ships and runs changed.

So the whole-image digest still moves for reasons that have nothing to do with the image's
runtime content, and the runtime-content hash is the value that answers "did what this image
runs change?" See the escalation in `services/cfs/R5_3_REPORT.md` section 8 for the remaining
whole-image reproducibility gap.

**Rebuild from a cold cache, 2026-09-09 (same acceptance run), and the stronger measurement it
gave.** Later the same day both `altavista-cfs-lockstep:local` and the `ubuntu:22.04` tag its
Dockerfile builds from disappeared from this host's image store while 16 unrelated tagged images
(including `registry:2`, which our own container tests DO use) survived -- the second occurrence
of the tag-disappearance R4.3 recorded. The suspected actor there, `crates/av-kernel/tests/
drm_attitude_control_cfs.rs`'s `DockerImageGuard` (`docker rmi -f <its own throwaway tag>`), is
now **excluded by measurement, not merely doubted**: tagging one present image under two names
and running `docker rmi -f` on the second untags only that second name and leaves both the first
name and the original tag intact (probe run and captured this round). No path remains to identify
the real actor from inside this repository without a Docker daemon event log, so it is recorded
here rather than guessed at.

Recovering from it required a genuinely cold, from-scratch rebuild (no image, no base image, no
layer cache -- the network window per question 154), which is the strongest reproducibility
evidence this pin has: that build's runtime-content hash came out `sha256:5049bf8f...` again,
identical to the two earlier builds, while the whole-image digest moved a third time. Three
builds -- one warm-cache, one comment-only-change, one fully cold -- agree exactly on what the
image runs.

**Operational note for the next person.** The four `drm_attitude_control_cfs.rs` container tests
short-circuit and report `ok` when this image is absent (measured: 0.17s for all four, against
200.86s when it is present). A green `cargo test -p av-kernel` therefore does NOT by itself mean
the cFS container path was exercised -- check the elapsed time of that binary, or run
`services/cfs/build-image.sh` first. Raised for the lead as a defect in its own right: unlike
`test_image_digest.py`, which skips visibly with a reason, these skip invisibly as passes.

Previous digest (R5.3 / question 185, superseded by the manifest regeneration immediately
above -- same runtime-content hash, digest moved only for the comment-only `targets.cmake`
edit described there):
```
sha256:3477865d381a91a307fdc2e05c62d55efa426dcbfb6aafb7d33ca4a90115ad30
```

Previous digest (R4.3 / question 182, re-pinned 2026-09-08, superseded by the R5.3 re-pin
above):
```
sha256:a1303bcef95d870d609c1965d09ec26807030d197ea36d559748e71398081e0a
```

Previous digest (R4.3 / question 182, first build, superseded by the mid-task incident
addendum below -- same fix, same verified `core-cpu1` content, digest moved only because of the
already-known-non-deterministic final-stage `apt-get install libc6` layer): `sha256:9d7757ef4dd9
463e6347692b1e35748229e23a9df8f8b3e12875d1509c0861fa`.

Previous digest (M25.4a / question 179, 2026-09-08):
`sha256:29eb1bec64b7e826de157468c79dd82103b80d42b7debc5fa809727dea994cde`.

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

## R6.4 (`docs/open-questions.md` question 190's bounded BuildKit experiment; question 194's
## `docker events` record): no re-pin -- see `services/cfs/R6_4_REPORT.md` for the full account

**Question 190's bounded experiment, run.** Question 190 (round 5, R5.3) authorized exactly one
bounded experiment -- build with BuildKit, `SOURCE_DATE_EPOCH` set to a fixed recorded value, and
a deterministic tar for the final `COPY --from=builder .../cpu1 /cfs/cpu1` layer, then compare two
genuinely cold (`--no-cache`) builds' whole-image digests -- and, if it did not close the gap,
retirement of the whole-image assertion with this question as the record. R6.4 checked this host's
actual capability *before* building anything, per the question's own instruction: `docker version`
(Client 29.6.2 / Server 29.5.2, Colima-backed), `docker buildx version` / `docker buildx ls` ->
`unknown command: docker buildx`, `DOCKER_BUILDKIT=1 docker build` -> `ERROR: BuildKit is enabled
but the buildx component is missing or broken`. A real, exhaustive search (`which buildx`, `~/
.docker/cli-plugins/`, `brew list --formula`, a `find` across both Homebrew Cellar prefixes,
`colima version`) found the `buildx` CLI plugin genuinely absent from this host and not
installable without a network fetch, which this round's constraints forbid. **This host has zero
BuildKit build capability** -- not merely a missing flag on an older `buildx`, but no BuildKit
mechanism of any kind; the legacy (Docker's own words: "deprecated") builder is the only builder
`docker build` can run here, and it has no `SOURCE_DATE_EPOCH`-aware frontend, no `--output` flag,
and no deterministic-tar/timestamp-rewrite mechanism. No build was attempted with this
configuration, because there is no configuration on this host that exercises what question 190
asked to be tested -- a real, empirical "capability is missing" answer, per the question's own
anticipation of that outcome, not an assumption. Full transcript:
`services/cfs/R6_4_REPORT.md` section 3 (and its own cited scratch logs).

**Consequently, per question 190's own decision: the whole-image digest assertion in
`services/cfs/tests/test_image_reproducibility.py` is retired to reported-not-asserted.** The
runtime-content hash assertion is unchanged -- still the one hard-asserted, always-on
reproducibility guarantee. A whole-image mismatch is now printed (both digests, and, if they
differ, the same dynamic per-`/cfs`-file diff as before) but never fails the test; the code
comment at the assertion site names question 190 as the record. No `Dockerfile` or
`build-image.sh` change was made for the experiment (there was nothing to build with), so **the
currently-recorded digest and runtime-content hash above are unchanged and no re-pin was
needed or attempted this round.**

**Measurements for the record** (per question 190's "if they do not match" instruction), cited
from R5.3's own already-real, already-captured two-`--no-cache`-build comparison (2026-09-09, the
most recent one that exists for this Dockerfile -- R6.4 could not run a fresh pair; see the
incident below):
- Whole-image digest 1: `sha256:d568bd98b3f8...`; digest 2: `sha256:972df8296c38...` (differ).
- Runtime-content hash, both builds: `sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef`
  (MATCHED).
- File-level diff (the exact check the test itself performs): **no file under `/cfs` differs
  between the two images at all** -- the gap is confined to image metadata/OCI layer history, not
  file content.
- `docker history --no-trunc` / per-layer digest detail for this specific pair: **still not
  available** -- R5.3's own test cleanup removed both disposable images before it could be
  captured, and R6.4 could not reproduce a fresh pair either (see below). Recorded as an open item,
  not fabricated.

**Live incident discovered during R6.4, before any Docker-mutating command of my own: the
officially pinned image AND its exact digest-pinned base image were both absent from this host's
local Docker store.** `docker image inspect altavista-cfs-lockstep:local`, `docker image inspect
ubuntu:22.04`, and `docker image inspect
ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc` all returned
"No such image". This is a live, **third** occurrence of the disappearance question 194 already
tracks (the first two are recorded earlier in this file, R4.3's "Mid-task incident" and R5.3's
"Rebuild from a cold cache" section), and this time it took the exact digest-pinned base reference
too, not just a floating tag. A retrospective `docker events --since <2 hours ago>` query found
only about 40 seconds of actual daemon history by the time it was run -- flushed by unrelated
`cohort_backend` Supabase container health-check chatter sharing this daemon -- so no
attribution was possible from inside this repository this time either, exactly as question 194
already anticipated. **This blocked any real `docker build` of the actual `services/cfs/Dockerfile`
this round** (resolving the pinned base digest right now would require a real network fetch,
which this round's brief forbids and does not authorize) -- both the fresh Part-A measurements
above and a full end-to-end test of Part B's `docker events` wrapper (below) against the real
image could not be completed. **Escalated to the manager as the top priority of this round; full
account in `services/cfs/R6_4_REPORT.md` sections 2 and 6.**

**Question 194's `docker events` capture, added to `build-image.sh`.** See this file's own top
section ("`docker events` capture") for the design, and `services/cfs/R6_4_REPORT.md` section 5
for how it was verified given the incident above blocked a real end-to-end run: the exact function
bodies now in `services/cfs/build-image.sh` were exercised, unmodified, against a trivial
local-only (`python:3.13-slim`-based, no network) scratch build in three real, executed cases --
normal (events genuinely captured, real `image`/`container` JSON lines recorded), events-capture
start failure (real `chmod 000` on the log directory -> a `WARNING` printed, build still succeeded)
and build failure (a real bad `RUN` command -> the build's own real failure text surfaced, exit
code propagated correctly via the trap's saved `$?`, not swallowed, and the events log still
contained the partial capture up to the failure). Not yet verified against the real,
multi-minute, many-layer `services/cfs/Dockerfile` build -- recorded as the next thing to confirm
once the incident above is resolved.

## Re-pinned 2026-09-09 (R5.3, `docs/open-questions.md` question 185): what changed and why

Question 185 (round 5, and its own 2026-09-08 "after the manager's review" amendment) asked for
five things: (1) build-artifact manifest entries for question 179's amendment (`services/cfs/bin/
av-lockstep-shim`, untracked); (2) cFE's unit-test/coverage build off for `native_std`; (3)
`-Wl,--build-id=none` in the same mission-config layer, belt-and-braces; (4) both Dockerfile
stages pinned to one digest-identified `ubuntu:22.04` base, with the final stage's `apt-get
install libc6` dropped entirely; (5) a runtime-content hash (defined above), recorded here and
asserted equal across two independent builds by `services/cfs/tests/test_image_reproducibility.
py`, without narrowing or replacing the existing whole-image digest assertion.

**1. Build-artifact manifest entries.** `services/cfs/build-image.sh` now marks a manifest entry
`BUILD_ARTIFACT` when `git check-ignore` reports its path as ignored (a property of the path, not
a hardcoded filename list) -- currently only `services/cfs/bin/av-lockstep-shim`. `services/cfs/
tests/test_image_digest.py`'s `test_manifest_paths_exist_and_hash_match` verifies a
`BUILD_ARTIFACT` entry's hash only when the file is present, and skips VISIBLY (naming the file
and the Dockerfile's own rebuild recipe) when it is not; every other entry stays strictly
verified, and a missing/changed non-build-artifact entry still fails the test outright. Proven
with a real break-and-restore: the shim was moved aside, the test was re-run (captured in
`services/cfs/_r5_3_scratch/15_break_item1_shim_absent.txt`) and skipped with the exact expected
reason, the shim was moved back, and `git status` for `services/cfs/bin/` was confirmed empty
throughout (the path is `.gitignore`d, so this move never touches tracked state).

**2. cFE unit tests off.** `services/cfs/build/targets.cmake` now does `set(ENABLE_UNIT_TESTS
FALSE CACHE BOOL "Enable build of unit tests" FORCE)`. `third_party/cfs/target-configs.mk`'s own
`PREP_OPTS_native_std += -DENABLE_UNIT_TESTS=TRUE` (a fetched file, not edited) pre-populates the
CMake cache as TRUE before any project code runs; a plain `set(... CACHE BOOL ...)` without FORCE
(what `third_party/cfs/cfe/cmake/mission_build.cmake`'s own `initialize_globals()` does) cannot
override an already-cached value, so `services/cfs/build/targets.cmake`'s FORCE -- placed at the
earliest point this task's own files run (`include(${MISSION_DEFS}/targets.cmake)`, before
`read_targetconfig()`/`prepare()` and before every module's own `if (ENABLE_UNIT_TESTS)
add_subdirectory(...)` gate) -- is what actually takes effect, for both the mission-level build
and every per-architecture sub-build (the sub-build imports a plain-variable TRUE from the
mission's own `mission_vars.cache`, which this FORCE cache set still overrides -- confirmed
empirically with a standalone CMake reproduction before relying on it, `services/cfs/
R5_3_REPORT.md` section 2).

Expected count stated before measuring: R4.3 recorded 143 files under `/cfs/cpu1` (86 UT/coverage
+ 57 "runtime-relevant", by R4.3's own count, which turned out to be wrong -- see the finding
below). Measured: **10 files** (`core-cpu1`, six mission `.so` app/PSP modules, `container-start`,
`cf/cfe_test_tbl.tbl`, `cf/cfe_es_startup.scr`). **Finding, not R4.3's fault to have caught but
worth recording:** R4.3's own "57 runtime-relevant" count included 21 `utmod/MODULE*.so` files,
which are dummy fixture modules built by OSAL's OWN `osal/src/unit-tests/osloader-test/`
directory (confirmed by reading that directory, gated by the very same `ENABLE_UNIT_TESTS`
switch, `osal/CMakeLists.txt` line 437) -- these are unit-test artifacts too, not runtime
content, and their removal here is correct, not a regression. Break-and-restore: the
`ENABLE_UNIT_TESTS FALSE` line was commented out, a real `docker build` (disposable tag) showed
**143 files** (matching the pre-fix baseline exactly, including `coverage-*-testrunner`/`*_UT`
binaries), the line was restored, and `git diff services/cfs/build/targets.cmake` was confirmed
back to exactly the intended addition. Full logs: `services/cfs/_r5_3_scratch/13_break_item2.log`.

**3. `-Wl,--build-id=none`.** Also in `services/cfs/build/targets.cmake` (see below for why NOT
a separate `global_build_options.cmake` file): `CMAKE_EXE_LINKER_FLAGS`, `CMAKE_SHARED_LINKER_
FLAGS` and `CMAKE_MODULE_LINKER_FLAGS` each get `-Wl,--build-id=none` appended via `CACHE STRING
... FORCE`. Verified by a raw byte-string search for the ELF section name `.note.gnu.build-id`
inside `core-cpu1` and `io_lockstep.so` (no `readelf`/`objdump` in the minimal final-stage image,
so `grep -a -c '\.note\.gnu\.build-id'` was used instead -- the same signal, a different tool):
**0 matches** (absent) with the fix applied (both the official image and a disposable
fix-verification build), **1 match** (present) with the fix broken. Break-and-restore: the
`foreach`/`set(... FORCE)` block was commented out, a real `docker build` (disposable tag) showed
the section present in both files, the block was restored, and `git diff` confirmed clean. Full
log: `services/cfs/_r5_3_scratch/14_break_item3.log`.

**A real defect found and fixed while implementing this item:** the first attempt put this in a
NEW file, `services/cfs/build/global_build_options.cmake`, wired through cFE's own OPTIONAL
"global-scope build customization" hook (`third_party/cfs/cfe/CMakeLists.txt` line 122) -- the
more textbook extension point, and the one this task tried first. It broke the build:
`es/fsw/src/cfe_es_api.c.o` failed with `fatal error: global_core_api_base_msgid_values.h: No
such file or directory` (a header `third_party/cfs/sample_defs/cpu1/cfe_core_api_base_msgid_
values.h` itself unconditionally `#include`s). Root-caused with 9 real `docker build` runs, one
variable isolated at a time (full account in `services/cfs/R5_3_REPORT.md` section 2): NOT the
digest-pinned base image, NOT `ENABLE_UNIT_TESTS`, NOT the new file's own content (proven by
neutralizing it and rebuilding) -- purely the presence of one MORE `COPY` instruction in the
Dockerfile's builder stage, regardless of what it copies. The generator for `global_core_api_
base_msgid_values.h` was not located after reading every `cfe/cmake/*.cmake` file and every
`MISSION_CORE_MODULES` module's own `arch_build.cmake`/`mission_build.cmake` in full; the leading
hypothesis is a latent, filesystem-enumeration-order-dependent defect somewhere in cFE's own
fetched build system, perturbed by Docker layer/container-filesystem state that shifts merely
from adding one more `COPY` layer -- not something this task fixes under `third_party/cfs/`.
**Fix:** move the linker-flags block into `services/cfs/build/targets.cmake` (already `COPY`ed,
no new `COPY` instruction needed) instead of a new file; `services/cfs/build/global_build_
options.cmake` was deleted and its `COPY` line removed from the Dockerfile. Verified with a real
build after the fix (succeeded; see `services/cfs/R5_3_REPORT.md`).

**4. Digest-pinned base, both stages; final-stage `apt-get` dropped.** `services/cfs/Dockerfile`
now pins `FROM ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc`
for BOTH stages (resolved via `docker pull ubuntu:22.04` during the one permitted network window,
question 154 -- confirmed identical to the value this file's own R4.3 section already recorded
for "the local ubuntu:22.04"). The final stage's `RUN apt-get update && apt-get install ...
libc6` is removed entirely. **Hypothesis stated before verifying:** the final stage's own
`libc6` need is already satisfied by the pinned base rootfs itself, which is now byte-identical
to what the builder stage's binaries linked against, since both stages start from the exact same
digest. **Verified, not assumed:** `ldd /cfs/cpu1/core-cpu1` and `ldd /cfs/av-lockstep-shim`
inside the built official image both resolve every shared library (`libc.so.6`, `libgcc_s.so.1`,
`libm.so.6`, the dynamic linker) with no "not found" entries.

**5. Runtime-content hash.** Defined above ("Runtime-content hash definition"). `services/cfs/
build-image.sh` computes and prints it (bash); `services/cfs/tests/test_image_reproducibility.py`
computes it independently (Python) for each of its two `--no-cache` builds and asserts equality,
as an ADDITIONAL always-on check that never narrows or replaces the whole-image digest assertion.
**Actually run, opted in** (`AV_CFS_RUN_REPRO_BUILD=1`, two genuine `docker build --no-cache`
runs, `services/cfs/_r5_3_scratch/17_repro_test_run.txt`): **the runtime-content hash MATCHED
exactly** (`sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef` both times) --
every file this image actually ships and runs is byte-identical across two independent builds,
confirming items 2-4 above are sufficient for everything the container actually runs. **The
whole-image digest still did NOT match** (`sha256:d568bd98b3f8...` vs `sha256:972df8296c38...`),
and the test's own dynamic file-diff (not a static, possibly-stale paragraph) reported: no file
under `/cfs` differs between the two images at all -- the difference is confined to something
outside the runtime-content set, i.e. image metadata or OCI layer history, not file content.
**Not root-caused further in this round** (would need at least one more `--no-cache` build pair
to inspect `docker history --no-trunc` on the specific failing images, which are disposable and
already removed by the test's own cleanup): the leading hypothesis is non-deterministic tar-entry
ordering or metadata (e.g. mtimes) within the multi-file `COPY --from=builder .../cpu1 /cfs/cpu1`
layer, a class of Docker/OCI reproducibility gap that is independent of and beyond any of items
2-4 -- **escalated to the manager, not guessed at further here** (an image-content change beyond
the five items, if fixable at all without changing build tooling). Per this task's own rule, the
whole-image assertion is left strict and failing, honestly, rather than weakened.

**Consequence for this section's own recorded digest and hash above:** the digest and
runtime-content hash recorded at the top of this file come from the single official
`services/cfs/build-image.sh` run (not from either disposable `--no-cache` reproducibility-test
build), exactly as every previous re-pin in this file's history has done.

## Re-pinned 2026-09-08 (R4.3, `docs/open-questions.md` question 182): what changed and why

Question 182 is the decided follow-on to question 179 (named below, "Re-pinned 2026-09-08
(M25.4a, question 179)"): "pin all three [`BUILDDATE`, `BUILDUSER`, `BUILDHOST`] in the
Dockerfile to fixed values recorded in `IMAGE_DIGEST.md`, re-pin once, and add a test that two
consecutive builds from the same manifest give the same digest."

**Checked before changing anything, not taken on question 179's own summary:** question 179's
write-up below quotes only `generate_build_env.cmake` lines 15-23 (the `BUILDDATE` block) as
its evidence for all three variables. Reading that file's full 48 lines shows the other two do
**not** read an env var of the same name as the cFE variable:

```
set(BUILDDATE $ENV{BUILDDATE})    # reads env var BUILDDATE
set(BUILDHOST $ENV{HOSTNAME})     # reads env var HOSTNAME, not BUILDHOST
set(BUILDUSER $ENV{USER})         # reads env var USER, not BUILDUSER
```

So `services/cfs/Dockerfile` now sets, in the builder stage, via `ENV` (not `ARG` -- `ARG` is
not implicitly exported into a later `RUN`'s environment, and these must be visible to
`RUN make native_std.compile`, the step that actually invokes `generate_build_env.cmake` per
that file's own "runs at build time, not prep time" comment):

```
ENV BUILDDATE=202609080000
ENV USER=altavista
ENV HOSTNAME=altavista-build
```

`202609080000` matches the `date +%Y%m%d%H%M` format `generate_build_env.cmake` itself uses
for its fallback (12 digits, no seconds). `USER`/`HOSTNAME` were grepped across
`third_party/fetch-cfs.sh` and the whole fetched `third_party/cfs/` tree and are read nowhere
else, so pinning them for the builder stage has no other observable effect. Confirmed baked in
by grepping the compiled `core-cpu1` binary inside the rebuilt image
(`docker run --rm --entrypoint sh altavista-cfs-lockstep:local -c "grep -a -o ... /cfs/cpu1/
core-cpu1"`): `202609080000`, `altavista-build`, and `altavista` are all present.

Re-pinned via `services/cfs/build-image.sh` (never a hand-rolled `docker build`), from the
repository root, with the network window question 154 permits. The full build log
(`services/cfs/_r4_3_scratch/build_repin.log`) shows every builder-stage step (1-20, through
`FROM ubuntu:22.04 AS builder` .. `RUN make native_std.install`) as `Running in ...` -- none
read `Using cache` -- so the builder stage genuinely re-executed; only the unrelated final-stage
`apt-get libc6` layer was served from cache, matching every prior rebuild recorded in this file.

New image ID: `sha256:9d7757ef4dd9463e6347692b1e35748229e23a9df8f8b3e12875d1509c0861fa` (recorded
above). Previous (question 179) digest:
`sha256:29eb1bec64b7e826de157468c79dd82103b80d42b7debc5fa809727dea994cde`.
`services/cfs/IMAGE_CONTEXT_MANIFEST.txt` was regenerated by the same `build-image.sh` run (38
file entries, unchanged set of paths/hashes from the question-179 manifest -- only
`services/cfs/Dockerfile` itself changed, and it is not a `COPY` source, so it does not appear
in the manifest; the digest moved purely because the builder stage's linked `CONFIGDATA` now
differs).

`services/cfs/tests/test_image_digest.py` re-run immediately after this update (see this task's
own `R4_3_REPORT.md`) -- **PASS** against the new pin.

### A deeper finding: the whole-image digest is still not fully reproducible (out of scope here)

Per question 182's own instruction to add "a test that two consecutive builds ... give the same
digest," `services/cfs/tests/test_image_reproducibility.py` was written and **actually run**
(`AV_CFS_RUN_REPRO_BUILD=1`, two genuine `docker build --no-cache` runs of the now-pinned
Dockerfile). Result: **it failed** -- the two builds produced different image IDs
(`sha256:841a01b1...` vs `sha256:08808106...`). Investigated rather than dismissed (full account
in `R4_3_REPORT.md`): every one of the 143 files under `/cfs/cpu1` was sha256'd in both a fresh
`--no-cache` build and the official pin; **all 57 runtime-relevant files are byte-identical**
(`core-cpu1` itself, every mission `.so` app/PSP module, `cf/cfe_es_startup.scr`,
`cf/cfe_test_tbl.tbl`, `container-start`, all 21 `utmod/MODULE*.so`), and the only 86 files that
differ are cFE/OSAL's own bundled `coverage-*-testrunner` / `*-test` / `*_UT` unit-test/coverage
harness executables (never invoked by `container-entrypoint.sh`), each differing only in its ELF
`.note.gnu.build-id` section -- classic GNU-linker build-id non-determinism, unrelated to
`generate_build_env.cmake`. **Separately**, the final image stage's own
`RUN apt-get install ... libc6` layer was independently confirmed non-deterministic across
`--no-cache` runs (differing layer diffID even though the same package presumably installs).

**The question-182 fix is confirmed correct and sufficient for everything the image actually
runs; it is not sufficient to make the whole image's content-addressed `.Id` itself
reproducible**, because of these two additional, independent, out-of-scope sources. Fixing
either is an image-content change beyond the three pinned variables (stripping/excluding the
bundled UT/coverage harnesses, passing a deterministic build-id flag into cFE's toolchain file,
or pinning apt package versions/a frozen mirror for both `apt-get` invocations) -- **escalated
to the lead, not guessed at here.**

### Mid-task incident: the local image tag was deleted by something else, and restoring it
### incidentally re-demonstrated the finding above

After the R4.3 re-pin above (`sha256:9d7757ef...`) and the reproducibility-test investigation,
`docker image inspect altavista-cfs-lockstep:local` unexpectedly returned "No such image" --
the tag and its image had been removed by something other than this task's own commands (this
task never ran `docker rmi`/`docker system prune` against that tag). At the time, several
concurrent `cargo test -p av-kernel ...` processes were running on this host (acceptable per
this task's own contention rule, which only restricts a *second concurrent `docker build`*).
This file's own top section already documents that `crates/av-kernel/tests/
drm_attitude_control_cfs.rs`'s `BINDING_KIND_CONTAINER` tests "separately tag and push this same
already-built image to a throwaway local ... registry," which is the most likely actor, though
this was not proven (no log from that process was captured). **Flagged for the manager as a
process/environment gap: the contention rule that guards against a second concurrent
`docker build` does not guard against a concurrent test suite that retags, pushes, or prunes an
already-built image this task depends on having stay put.**

Restored by re-running `services/cfs/build-image.sh` again (still "once" in the sense of one
deliberate re-pin decision; this second invocation was recovery from external interference, not
a second intentional content change). The build log
(`services/cfs/_r4_3_scratch/build_restore.log`) shows **every builder-stage step (1-20) hit
`Using cache`** -- i.e. the compiled `/cfs/cpu1` tree is provably the exact same bytes as the
first R4.3 build -- and confirmed directly: `core-cpu1`'s own sha256
(`39941a529a802e16327418d699a0dda86374fb64bb56ad81533cb9590d5c8b43`) is IDENTICAL between the
first R4.3 build and this restore build. **The only step that re-executed was Step 22, the
final stage's `RUN apt-get install ... libc6`** -- exactly the second non-determinism source
named above, now caught red-handed as the sole cause of this particular digest move. New (and
now recorded, above) digest: `sha256:a1303bcef95d870d609c1965d09ec26807030d197ea36d559748e71398081e0a`.

`services/cfs/tests/test_image_digest.py` re-run after the restore -- **PASS** against this
final digest (see `R4_3_REPORT.md`).

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
