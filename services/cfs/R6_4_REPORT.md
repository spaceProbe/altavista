# R6.4 report: question 190's bounded BuildKit experiment, and the `docker events` record (question 194)

Written incrementally as the work happened. Hypotheses/expectations are stated before each
measurement, then the measurement is recorded immediately after.

## 0. Required reading, confirmed myself (not taken on any summary alone)

Read in full:
- `docs/open-questions.md` questions 179 (and its unindexed 2026-09-08 amendment), 182 (and its
  2026-09-08 amendment), 185 (and its own 2026-09-08 "after the manager's review" amendment), 190,
  194.
- `services/cfs/R5_3_REPORT.md` (349 lines, full).
- `services/cfs/R4_3_REPORT.md` (392 lines, full).
- `services/cfs/IMAGE_DIGEST.md`, `services/cfs/build-image.sh`, `services/cfs/Dockerfile`,
  `services/cfs/tests/test_image_reproducibility.py`, `services/cfs/tests/test_image_digest.py`.

## 1. Baseline, confirmed before any change

- Contention check clear at the start (`ps -Ao pid,etime,command | grep -E "docker build|pytest"`
  empty) before I began. A second worker's own `pytest` process appeared on the host later, while
  I was doing read-only investigation (not a `docker build`, so not something the brief's
  contention rule asks me to wait on before starting my own read-only `pytest -q
  services/cfs/tests/` run) -- it likely explains why that baseline run below took 538.89s instead
  of the ~1-2 minutes a Docker-skip-only run should take.
- `.venv/bin/python -m pytest -q services/cfs/tests/ -rs`: **18 passed, 2 skipped in 538.89s**
  (`_r6_4_scratch/00_baseline_pytest.txt`, referenced here by its scratch path; full path
  `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r6_4/00_baseline_pytest.txt`).
  Both Docker-gated tests skip **visibly**, exactly as designed -- see the critical finding in
  section 2 below for *why* the image test skips this time (it isn't just "never built here").

## 2. Critical, live finding: the pinned image AND its base image have both vanished a third time

Before running any `docker` command of my own beyond `docker version`/`docker info`/`docker
context ls`, I checked the currently-recorded pin as a matter of course (the standing pattern
every prior round in this file used as its own "baseline measurement"). Real, captured output,
not summarized:

```
$ docker image inspect altavista-cfs-lockstep:local --format '{{.Id}}'
Error response from daemon: No such image: altavista-cfs-lockstep:local

$ docker image inspect ubuntu:22.04 --format '{{.Id}}'
Error response from daemon: No such image: ubuntu:22.04

$ docker image inspect ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc --format '{{.Id}}'
Error response from daemon: No such image: ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc
```

**All three -- the official pin, the floating base tag, and the exact digest-pinned base
reference this round's Dockerfile requires -- are absent from this host's local image store.**
`docker images -a` shows several dozen `<none>:<none>` dangling images (many "56 minutes ago",
some "About an hour ago") consistent with layers from a recent build/prune cycle, but none of
them resolve under either tag or the pinned digest; nothing in this repository's own tooling can
attribute this without an event log, exactly question 194's own diagnosis.

**This is a live, third occurrence of the exact incident `docs/open-questions.md` question 194
already tracks** ("the local cFS image tag and its base image vanished a second time ... no path
remains without a Docker daemon event log"), discovered by me before I ran a single `docker
build`, `docker rmi`, or any other mutating Docker command this round. I did not cause it.

**I tried to retrieve a retrospective explanation from the daemon's own live event buffer**
(`docker events --since <2 hours ago> --until now --filter type=image --filter type=container`,
full output `_r6_4_scratch/01_docker_events_retrospective.txt`, 232 lines): the daemon's in-memory
event buffer, on this host, turned out to hold only about 40 seconds of history
(2026-09-09T19:09:34 through 19:10:14) by the time I queried it -- entirely `container exec_*`
health-check chatter from an unrelated, already-running `cohort_backend` Supabase Docker Compose
stack sharing this daemon (dozens of `wget`/`curl`/`pg_isready` health probes firing every few
seconds; nothing related to `altavista-cfs-lockstep`, `ubuntu`, or any cFS-related tag anywhere in
it). **This is itself direct, empirical, first-hand confirmation of exactly why Part B (section 5
below) is necessary and cannot be done retroactively**: this daemon's own event ring buffer is
evidently short and gets flushed by unrelated high-frequency container activity within minutes,
so a `docker events --since` query run *after the fact* -- even a few tens of minutes after --
cannot be relied on to explain a tag's disappearance. Only a *live* capture, started before the
operation whose window you care about and running continuously through it, has a chance.

**Consequence for this round's own two absolute constraints** ("No network at test or run time...
do not `docker pull` anything new" and "the base image is already pinned by digest and present
locally"): that second premise is now false, discovered live, through no action of mine.
`services/cfs/Dockerfile`'s builder and final stages both pin `FROM
ubuntu:22.04@sha256:2edbbc5dc...`, and with that digest absent locally, **any `docker build`
invocation of this Dockerfile right now would require the Docker CLI/daemon to resolve that
digest from a registry** -- a real network fetch, functionally a `docker pull`, even though no
literal `docker pull` command would be typed. I judge this to fall squarely under this round's own
"do not `docker pull` anything new" prohibition, and this round's brief grants me no network
window (unlike R4.3/R5.3, which explicitly invoked question 154's one-time exception for their
re-pins) -- restoring the base image is a network-window decision that is the lead's/manager's to
make, not mine to take unilaterally under a brief that did not anticipate this.

**I therefore did not run `docker build` against the real `services/cfs/Dockerfile` at all this
round**, for either Part A or Part B. Every measurement below that would normally come from a real
build of this image is either (a) drawn from R4.3's/R5.3's own already-captured, already-recorded
real measurements (cited, not re-derived), or (b) substituted with a real, local-only, non-network
proof against a harmless scratch Dockerfile (already-present `python:3.13-slim` base, no pull
needed) that exercises the exact shell/Python logic I changed, clearly labelled as a substitute
and not as a claim that the real image was rebuilt. This is escalation 1 in section 7 -- the
single most important thing for the manager to act on from this round.

## 3. Part A: the bounded BuildKit + `SOURCE_DATE_EPOCH` experiment

### 3.1 Hypothesis, stated before checking anything

Before running `docker version`/`docker buildx version`/`docker buildx ls`, my expectation: this
host runs Docker Engine (via Colima) rather than Docker Desktop, and Colima's bundled Docker CLI
historically has not always shipped the `buildx` CLI plugin by default the way Docker Desktop
does. If `buildx` is present and recent enough (`buildx` >= 0.10 roughly), `SOURCE_DATE_EPOCH`
support in the Dockerfile frontend can rewrite file mtimes inside layers, but making that also
rewrite the *final image config/output* (which is what `docker image inspect .Id` hashes)
additionally requires the exporter to be told to, via `--output
type=docker,name=...,rewrite-timestamp=true` -- a comparatively recent buildx/exporter capability.
My expectation, before checking, was "maybe present, maybe an older buildx without
`rewrite-timestamp`" -- i.e., I expected to find *some* BuildKit, possibly missing one flag.

### 3.2 Versions checked, real output

```
$ docker version
Client: Docker Engine - Community
 Version:           29.6.2
 ...
Server:
 Engine: Version 29.5.2
 ...

$ docker buildx version
docker: unknown command: buildx

$ docker buildx ls
docker: unknown command: buildx

$ DOCKER_BUILDKIT=1 docker build --help
ERROR: BuildKit is enabled but the buildx component is missing or broken.
       Install the buildx component to build images with BuildKit:
       https://docs.docker.com/go/buildx/

$ docker build --help    # (no DOCKER_BUILDKIT set)
... --output flag is NOT present in the legacy builder's flag list at all ...

$ mkdir scratch && printf 'FROM scratch\n' > Dockerfile.trivial && docker build -f Dockerfile.trivial .
DEPRECATED: The legacy builder is deprecated and will be removed in a future release.
            Install the buildx component to build images with BuildKit:
            https://docs.docker.com/go/buildx/
Sending build context to Docker daemon  3.072kB
Step 1/1 : FROM scratch
 --->
No image was generated. Is your Dockerfile empty?
```

Full session transcript for this section:
`_r6_4_scratch/02_buildkit_capability_check.txt`.

**Real, empty-handed search for `buildx` anywhere on the host**, not merely trusting the CLI's own
error message: `which buildx` / `which docker-buildx` -> not found; `~/.docker/cli-plugins/` ->
does not exist; `brew list --formula` -> only `docker`, `docker-compose` (no `docker-buildx`
formula/cask installed); `find /opt/homebrew/Cellar /usr/local/Cellar -iname '*buildx*'` -> empty.
Colima itself (`colima version` -> 0.10.3, runtime docker) does not bundle `buildx` either.

### 3.3 Finding, empirical, definitive -- not assumed

**This host has zero BuildKit build capability from the Docker CLI.** It isn't "an older buildx
missing `rewrite-timestamp`" (my pre-check expectation) -- the `buildx` CLI plugin component that
BuildKit-mode `docker build` unconditionally requires (as of this Docker CLI's own behavior, 23+
generation) is **not installed at all**, and installing it would itself require a package/binary
fetch over the network, which this round's own "no network" constraint forbids. The legacy
(explicitly labelled "deprecated") builder is the *only* builder this host's `docker build` can
actually run, and it has no `--output` flag, no `SOURCE_DATE_EPOCH`-aware frontend, and no
deterministic-tar/timestamp-rewrite mechanism of any kind -- those are exclusively BuildKit
features.

This is exactly the situation the task brief's own wording anticipates and asks for: "Establish
empirically what this host supports rather than assuming; if a capability is missing, that is a
real measurement and part of the answer." The measurement is unambiguous: **the capability this
bounded experiment depends on (BuildKit itself, not merely one of its flags) does not exist on
this host and cannot be installed without violating the no-network constraint.**

### 3.4 Decision: this is "does not close the gap," recorded without a further build attempt

Given 3.3, there is no configuration of `docker build` on this host that can even *attempt*
`SOURCE_DATE_EPOCH`/deterministic-tar/`rewrite-timestamp`, let alone the "strongest single
configuration this host supports" the brief asks me to pick and justify -- there is no supported
configuration at all. Per "one bounded experiment ... is allowed, and if it does not close the gap
the whole-image assertion is retired": a missing mechanism cannot close the gap. I did not attempt
a `--no-cache` two-build comparison using the legacy builder with a `SOURCE_DATE_EPOCH` env var
set, because the legacy builder does not consume that variable at all (it is a BuildKit-frontend
concept) -- doing so would produce two real builds and a real "still differs" result, but it would
not have tested what question 190 actually asked for, and running it would misrepresent an
untested configuration as a tested one. It would also cost ~10-20 more minutes for a result I can
already predict with certainty from R4.3's/R5.3's own repeated, independent confirmations that the
legacy-builder path (which is *all* that has ever run against this Dockerfile in this repository's
history) leaves the whole-image digest non-reproducible for reasons unrelated to
`SOURCE_DATE_EPOCH`. Per "one task, no second attempt," I am not spending it on a build I already
know, from the tool's own documented feature set, tests nothing new.

Independently of 3.3, section 2's live incident (base image absent, no network permitted this
round) would have blocked any real build attempt anyway, BuildKit or not -- a second, independent
reason no build was run this round.

### 3.5 Measurements recorded for the "does not match" branch

Per the brief: "Record the measurements (both digests, and a real diff of what differs ... plus a
per-layer digest comparison, plus the file-level diff the existing test already computes)."
Since no new build was run this round (3.4, and section 2), I am citing R5.3's own already-real,
already-captured measurements -- the most recent actual two-`--no-cache`-build comparison that
exists for this Dockerfile, run 2026-09-09 (the same day as this round, R5.3), which is the
factual basis question 190 itself was written from:

- Whole-image digest 1: `sha256:d568bd98b3f8...`
- Whole-image digest 2: `sha256:972df8296c38...`
- Runtime-content hash (both builds): `sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef`
  (MATCHED).
- File-level diff (the exact check `test_image_reproducibility.py`'s `_describe_cfs_file_diff`
  performs): **no file under `/cfs` differs between the two images at all** -- R5.3's own dynamic
  diff output, not a static claim.
- `docker history --no-trunc` / per-layer digest comparison for that specific pair: **not
  available** -- R5.3's own report states plainly that the two disposable images were already
  removed by the test's own `finally: _docker_rmi(...)` cleanup before `docker history` could be
  run on them, and reproducing the exact pair would have cost another `--no-cache` build pair.
  **I could not close this specific gap this round either** (section 2's live incident blocks any
  new build). This one specific piece of evidence -- per-layer/`docker history` detail for a
  matched pair -- remains genuinely missing from the historical record; recorded as unresolved in
  section 7 below rather than fabricated.

Combined conclusion, matching R5.3's own: the whole-image `.Id` difference is confined to
something outside file content entirely (image metadata or OCI layer history) for a `COPY
--from=builder .../cpu1 /cfs/cpu1`-shaped multi-file layer under the legacy builder -- and no
mechanism exists on this host to test whether BuildKit would have closed that gap.

### 3.6 Change made: the whole-image assertion is retired to reported-not-asserted

`services/cfs/tests/test_image_reproducibility.py` -- the ONLY file this round touches for Part A
(no `Dockerfile` or `build-image.sh` change was made for the experiment, exactly as the brief asks
for the "does not match" branch, so **the currently pinned image stays valid and no re-pin is
needed**):

- The runtime-content hash assertion (`if rc_hash_1 != rc_hash_2: pytest.fail(...)`) is
  **unchanged** -- still the always-on, hard-asserted steady-state reproducibility guarantee, per
  question 190's own decision.
- The whole-image digest comparison changed from `assert digest_1 == digest_2, (...)` to a
  **report-only** code path: it always prints both digests and, when they differ, prints the same
  dynamic per-file diff as before (still real, still computed from the two just-built images, not
  a canned string) -- but no longer fails the test. A comment directly above it names question 190
  by number and states the two grounds for retirement recorded above (BuildKit unavailable on this
  host; the R5.3/R4.3 historical record already showed two independent out-of-scope non-determinism
  causes fixed and a third, OCI-layer-shaped one still open with no available mechanism to close
  it).
- The module docstring's "two assertions" framing is updated to say assertion 2 is now
  report-only, with the reason and a pointer to this file's own section 3 and to
  `IMAGE_DIGEST.md`.

See section 4 for how this specific change was verified given section 2's build-blocking incident.

## 4. Verification of the Part A code change (adapted for the live blocker in section 2)

**Disclosed explicitly, per the standing rule ("disclose explicitly anything you deliberately did
not break-and-restore, and say why"): a full, live break-and-restore -- two real
`docker build --no-cache` runs of the real `services/cfs/Dockerfile` against the old
(hard-assert) code, then the same two builds again against the new (report-only) code -- was NOT
executed this round.** Doing so requires resolving `ubuntu:22.04@sha256:2edbbc5dc...`, which is
absent locally (section 2); resolving it needs the network, and this round's brief grants no
network window and explicitly forbids `docker pull`. I judged attempting it anyway, on my own
authority, to be a bigger violation than leaving this specific verification lighter-weight and
disclosed.

What I verified instead, all real, all actually executed (not reasoned about):

1. **Static correctness.** `.venv/bin/python -m py_compile
   services/cfs/tests/test_image_reproducibility.py` -- real run, exit 0 (output:
   `_r6_4_scratch/03_py_compile.txt`).
2. **The full default suite still collects and the file's own default (non-opt-in) skip path
   still works**, unchanged from before my edit -- rerun of `.venv/bin/python -m pytest -q
   services/cfs/tests/test_image_reproducibility.py -rs` (no `AV_CFS_RUN_REPRO_BUILD`, so this
   never touches Docker or the network): real run, **1 skipped**, same skip reason text as
   baseline (output: `_r6_4_scratch/04_default_skip_after_edit.txt`).
3. **Real, executed exercise of the exact changed control flow, against real historical digests,
   with only the Docker I/O boundary substituted** (the three functions that shell out to `docker
   build`/`docker image inspect`/`docker run`: `_docker_build_no_cache`, `_image_id`,
   `runtime_content_hash`, `_all_cfs_file_hashes`) -- via `unittest.mock.patch` in a scratch driver
   (`_r6_4_scratch/05_mocked_break_restore.py`, kept for the record) that imports the REAL,
   unmodified `test_two_independent_builds_produce_the_same_image_id` function from the actual
   edited module and calls it directly, feeding the mocks R5.3's own real, already-recorded
   values: `digest_1="sha256:d568bd98b3f8..."`, `digest_2="sha256:972df8296c38..."` (genuinely
   different), `rc_hash_1=rc_hash_2="5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef"`
   (genuinely equal), and an empty `_all_cfs_file_hashes` diff (matching R5.3's own dynamic-diff
   result of "no file differs").
   - **Break (old code, restored temporarily via `Edit`, not `git`):** with the hard `assert
     digest_1 == digest_2` restored, running this driver against the same mocked-but-real values
     raised a real `AssertionError`, captured verbatim in
     `_r6_4_scratch/06_break_old_code_fails.txt`:
     ```
     AssertionError: two independent `docker build --no-cache` runs of .../Dockerfile produced
     DIFFERENT whole-image IDs ('sha256:d568bd98b3f8...' vs 'sha256:972df8296c38...') even though
     the runtime-content hash MATCHED ...
     ```
   - **Restore (new code, `Edit`ed back):** the same driver against the same mocked values
     completed with **no exception**, and printed the digest-differs report line to stdout,
     captured in `_r6_4_scratch/07_restore_new_code_passes.txt`.
   - `git diff services/cfs/tests/test_image_reproducibility.py` after this round of Edits was
     confirmed to show exactly the intended final change (see section 3.6), nothing else --
     `_r6_4_scratch/08_git_diff_after_restore.txt`.
4. **The runtime-content-hash assertion itself was not separately re-broken-and-restored this
   round**, because I did not touch that code path at all -- it is byte-identical to R5.3's own
   version, which R5.3 already broke-and-restored for real
   (`services/cfs/R5_3_REPORT.md` section 6, "Actually run, opted in"). Disclosed rather than
   silently assumed.

## 5. Part B: `docker events` beside the digest (question 194's second half)

### 5.1 Design

`services/cfs/build-image.sh` gains an events-capture wrapper around its existing work:

- Right after the existing Docker/git preconditions pass (before the `docker build` call), start
  `docker events --format '{{json .}}' --filter type=image --filter type=container` in the
  background, redirected to a new file, `services/cfs/build/last-build-events.jsonl` (a fixed
  name, overwritten each run -- it documents the *most recent* `build-image.sh` invocation's own
  window, exactly matching how `IMAGE_DIGEST.md`'s own top section always reflects the *current*
  pin rather than an ever-growing history; older windows are not needed once superseded, matching
  this file's own "beside the digest" framing). Located under `services/cfs/build/`, which is on
  my edit allowlist.
- A short liveness check (`kill -0` after a brief pause) confirms the background process actually
  started; if it didn't (or the log file couldn't be created), a **warning is logged and the
  script continues** -- events-capture failure never aborts or fails the build (verified in 5.3).
- The capture keeps running through the whole rest of the script (build, digest readback,
  runtime-content-hash computation, manifest write) -- "for its own build window" is read as
  `build-image.sh`'s own full execution window, not only the few minutes of the `docker build`
  call itself, since interference close to the build (e.g. immediately after) is exactly the
  pattern both prior incidents showed (R4.3's mid-task disappearance; the R5.3-acceptance-run
  "later the same day" disappearance).
- The capture is stopped (background process signalled, then waited on) in a `trap ... EXIT`
  handler so it always stops -- on success, on `docker build` failure (`set -euo pipefail`
  already aborts the script at that point; the trap still fires and flushes whatever was
  captured), or on any other early exit. This is combined with the script's pre-existing
  `TMP_MANIFEST` cleanup trap into one function so neither trap clobbers the other.
- The script's own final log lines name the events-log path explicitly (`stop_events_capture`'s
  own "docker events for this build's window written to ..." line, plus a final summary line
  naming the path again alongside the image ID), and `services/cfs/IMAGE_DIGEST.md` documents the
  path and the filter in its own top section.
- Filtered to `type=image` and `type=container` (per the brief, "image and container events at
  minimum, including untag/delete") rather than restricted only to those two actions, so `tag`,
  `pull`, `delete`, `untag`, `create`, `destroy`, etc. are all captured for context, not just the
  two named as the minimum.

### 5.2 Why the full, real, end-to-end proof (against the actual `services/cfs/Dockerfile`) could
not be executed this round

Section 2: the base image `services/cfs/Dockerfile` requires is absent locally, and restoring it
needs network this round's brief does not grant. Running the edited `build-image.sh` against the
real Dockerfile right now would either hang/fail attempting a registry fetch or (if network is in
fact reachable from this sandbox, which I did not test on purpose) silently perform exactly the
`docker pull`-equivalent operation the absolute constraints forbid. **I did not run it.**

### 5.3 What I verified instead: real, executed, local-only (no network, no pull)

Wrote a byte-for-byte copy of the new events-capture shell functions into a scratch harness,
`_r6_4_scratch/09_events_capture_harness.sh` (kept for the record -- NOT a reimplementation of
different logic; the `start_events_capture`/`stop_events_capture` function bodies are copy-pasted
verbatim from the actual edited `services/cfs/build-image.sh`, only the surrounding driver differs:
a trivial `docker build` of `FROM python:3.13-slim` + one `RUN` line, `python:3.13-slim` already
present locally per section 1's `docker images` listing, so this needs no network at all).

1. **Normal case**, real run (`_r6_4_scratch/10_events_normal_run.txt`): events file created,
   non-empty, contains real `image`/`container` typed JSON lines for the scratch build (`tag`,
   `create`, `start`, `die`, `destroy` actions observed for the throwaway container the `RUN` step
   spawns), capture process confirmed stopped after (`kill -0 $PID` -> no such process), scratch
   image removed (`docker rmi -f`, my own disposable tag only). This is real evidence the
   mechanism captures real events for a real (if trivial) build.
2. **Break: events capture cannot start** (real break, not simulated -- `chmod 000` on the scratch
   log directory before `start_events_capture` runs): real run
   (`_r6_4_scratch/11_break_events_capture_unwritable.txt`) shows the exact real warning
   (`[harness] WARNING: could not create .../last-build-events.jsonl.tmp -- continuing without a
   docker events capture for this build.`) and **the scratch build still completed successfully**
   (exit 0, image built and tagged) -- proving events-capture-start failure does not fail the
   build. Permissions restored (`chmod 755`) immediately after, inside the harness itself, before
   the build step (so the build's own context upload wasn't also blocked by the same chmod).
3. **Break: the underlying build itself fails** (real break -- scratch Dockerfile's second line
   changed to `RUN this-command-does-not-exist`): real run
   (`_r6_4_scratch/12_break_build_failure_not_hidden.txt`) shows the legacy builder's own real,
   visible, unmuffled failure text (`/bin/sh: 1: this-command-does-not-exist: not found` and
   `The command '/bin/sh -c this-command-does-not-exist' returned a non-zero code: 127`), and the
   harness's own exit code is **127** -- confirmed to be `docker build`'s own real exit code,
   propagated correctly through `set -e` and the `_cleanup_on_exit` trap's saved `$?` (verified
   this exact propagation mechanism in isolation first, `_r6_4_scratch/trap_test.sh`: a trap that
   captures `$?` into a `local` before running its own cleanup commands, then `return`s the saved
   value, reliably preserves the real exit code -- `false` inside a `set -e` script with such a
   trap exits 1, not 0). Nothing was hidden or swallowed: the harness did not add its own `die`
   call here (unlike the real `build-image.sh`, which would abort the same way via `set -e` on
   `docker build`'s own non-zero exit, with no explicit exit-code handling needed for that line
   either). The events log, inspected afterward, still contains whatever partial capture happened
   up to the failure (a real `create`/`attach`/`start`/`die` sequence with `exitCode:"127"` for the
   failed intermediate container) -- not empty, nothing hidden. Reverted the scratch Dockerfile
   after.
4. **`git diff` on the two real files this part touches** (`services/cfs/build-image.sh`) after
   settling on the final version: `_r6_4_scratch/13_git_diff_build_image_sh.txt` -- shows exactly
   the events-capture addition (functions + two call sites + one new `log` line), nothing else.

This is real execution of the exact logic that now lives in `services/cfs/build-image.sh`
(copy-paste identical function bodies, not "similar" logic), against real Docker operations, with
no network use anywhere in this section. What it does **not** prove is that the real
`services/cfs/Dockerfile` build (much longer, many more layers, the actual `apt-get`/`make`
sequence) behaves identically under this wrapper -- that is unverified this round and named
explicitly in section 8 as the top item remaining.

### 5.4 `IMAGE_DIGEST.md` updated

New note added to the top ("Recorded image digest") section: the events-log path
(`services/cfs/build/last-build-events.jsonl`), what it's filtered to, and a pointer to this
report's section 5 for the design rationale and the section-2 incident that motivated it. The
recorded digest/runtime-content-hash values themselves are **unchanged** -- no re-pin happened
this round (section 2 explains why one couldn't even be attempted).

## 6. Final verification

- `.venv/bin/python -m pytest -q services/cfs/tests/ -rs` (`_r6_4_scratch/14_final_verification.txt`):
  **1 failed, 17 passed, 2 skipped in 913.12s.** The one failure is
  `test_clean_fetch_patches.py::test_a_clean_fetch_applies_every_patch_and_the_patched_file_hash_matches`,
  a pre-existing test **not on this round's edit allowlist and not touched by either of my two
  changed files** -- it timed out after 900s inside `subprocess.run(["sh",
  ".../third_party/fetch-cfs.sh"], timeout=900)`, mid-`git clone` (stderr shows it got as far as
  "Cloning into .../cfs.tmp/cfe..." before stalling). Its own `_network_reachable()` pre-check
  evidently passed (a shallow reachability probe), but the actual sustained clone then stalled --
  this is itself a third, independent, real data point (beyond sections 2's live incident and the
  `docker events` retrospective-buffer finding) that network access from this host/sandbox is not
  simply "available or not" but can pass a shallow check and then hang under load, which is
  exactly the failure mode I was avoiding by not attempting an implicit `docker build`-triggered
  pull for the missing base image. **Confirmed NOT caused by my changes**: my very first baseline
  run this round (section 1, before any edit) collected the exact same test file and it passed
  cleanly (`18 passed, 2 skipped`, zero failures); this is intermittent, host-network-condition
  -dependent behavior in an out-of-scope file, not a regression I introduced. Not fixed here (out
  of my edit allowlist; another team's file). Noted as escalation 5 below.
- `git status --short services/cfs/` at the end of this round shows exactly:
  `build-image.sh`, `tests/test_image_reproducibility.py`, `IMAGE_DIGEST.md` modified;
  `R6_4_REPORT.md` new; `_r5_3_scratch/` untracked (pre-existing, R5.3's own, not touched by me).
  `services/cfs/Dockerfile` and `services/cfs/IMAGE_CONTEXT_MANIFEST.txt`: **unchanged**, as
  intended (no experiment-only Dockerfile edits were made, and the COPY list didn't change).

## 7. Escalations for the manager, in priority order

1. **(Most urgent, and blocking) The officially pinned image and its exact digest-pinned base
   image are both absent from this host's local Docker store right now** -- discovered live, by
   me, before any Docker-mutating command of my own (section 2). This is a live, third occurrence
   of the disappearance question 194 already tracks, and it happened to a digest-pinned reference
   this time, not just a floating tag -- meaning even the workaround of "just re-pull the floating
   tag" is not obviously safe/equivalent without knowing whether the registry still serves the
   exact same bytes under that digest. **Neither Part A's originally-planned fresh measurements
   nor Part B's full real-Dockerfile verification could be completed this round because of this.**
   A network-window decision (question 154-style, explicitly authorized, not assumed by me) is
   needed before anyone can next run `services/cfs/build-image.sh` for real. Recommend: (a)
   authorize one narrowly-scoped network window to `docker pull ubuntu:22.04@sha256:2edbbc5dc...`
   (the exact previously-verified digest, not a floating re-resolve) and re-run
   `services/cfs/build-image.sh`, confirming the digest lands back on the currently-recorded pin
   (it should, since nothing in the build context changed) before anyone relies on the image
   again; (b) separately, now that Part B's events-capture code exists, consider whether it's
   worth a short-lived, continuously-running `docker events` capture (outside any single
   `build-image.sh` invocation) on this shared host, since the daemon's own retrospective event
   buffer was measured this round (section 2) to hold only tens of seconds of history under
   typical load from unrelated containers on this host -- a `build-image.sh`-scoped capture alone
   will only catch a disappearance that happens to occur during an actual re-pin's own narrow
   window, not one that happens hours later, as apparently occurred at least once before.
2. **Part A's whole-image assertion retirement is code-complete and locally verified (section 4),
   but not verified end-to-end against a real `docker build --no-cache` pair of the actual
   `services/cfs/Dockerfile`** -- because of escalation 1. Once the base image is restored, I
   recommend a follow-up task run the real opt-in test once
   (`AV_CFS_RUN_REPRO_BUILD=1 .venv/bin/python -m pytest -q -s
   services/cfs/tests/test_image_reproducibility.py`) purely to confirm it now **passes** (report
   printed, no failure) rather than leaving that confirmation only as the mocked-boundary proof in
   section 4.
3. **Part B's events-capture wrapper is verified only against a trivial local scratch build
   (section 5.3), not the real, ~5-13-minute, many-layer `services/cfs/Dockerfile` build.** Once
   escalation 1 is resolved, recommend one real `services/cfs/build-image.sh` run purely to
   confirm the wrapper behaves the same at that scale (a longer-running background `docker events`
   capture across a multi-minute, multi-stage build, not just a few-second one) and that
   `services/cfs/build/last-build-events.jsonl` ends up populated and referenced correctly by
   `IMAGE_DIGEST.md`. **Also noted, not fixed here (root `.gitignore` is outside this round's edit
   allowlist):** the repo's `.gitignore` line 6 (`/build/`) is anchored to the repository root and
   does NOT cover `services/cfs/build/last-build-events.jsonl` -- once a real run creates that
   file, `git status` will show it as untracked. Worth a one-line `.gitignore` addition (e.g.
   `services/cfs/build/last-build-events.jsonl` or `services/cfs/build/*.jsonl`) in a round that
   can touch the root `.gitignore`.
4. **The per-layer/`docker history --no-trunc` diagnostic for a matched digest-mismatch pair is
   still missing from the historical record** (section 3.5) -- R5.3 lost the chance when its own
   test cleanup ran first, and I could not reproduce it this round (escalation 1). Not needed to
   act on question 190 (the file-level diff already proves the mismatch is metadata/history-only,
   not content), but would be the natural next artifact to capture the first time someone next
   runs a `--no-cache` pair with Docker available, if anyone ever wants to pin down the OCI-layer
   mechanism precisely rather than accepting it as an open, unresolved class of gap.
5. **(Minor, out of my scope) `services/cfs/tests/test_clean_fetch_patches.py` timed out on a real
   `git clone` this round** (section 6): 900s timeout mid-clone, on a file I neither touched nor
   was permitted to touch. Not caused by my changes (my own baseline run, before any edit, passed
   this exact test cleanly). Flagged because it's a third, independent, real data point that this
   host's network access can pass a shallow reachability check and then stall under sustained
   load -- relevant context for whoever acts on escalation 1's network-window request, and possibly
   evidence of resource contention from concurrent team activity on this shared host worth its own
   look, though I did not investigate further (out of scope, another team's file).

## 8. What remains, in priority order

1. Escalation 1 (restore the base image / pinned tag under an authorized network window) --
   blocks everything else below.
2. Escalation 2: real end-to-end confirmation that the retired (report-only) whole-image assertion
   behaves correctly against a genuine `--no-cache` pair of the real Dockerfile.
3. Escalation 3: real end-to-end confirmation of the events-capture wrapper at the real image
   build's scale and duration.
4. Escalation 4 (optional, lower priority): capture `docker history --no-trunc` / per-layer digest
   detail for a real mismatched pair, for the permanent record.
5. Escalation 5 (not mine to fix): `test_clean_fetch_patches.py`'s network-timeout flakiness, for
   whichever team owns that file.

## 9. Files touched this round

- `services/cfs/tests/test_image_reproducibility.py` -- whole-image assertion retired to
  reported-not-asserted (question 190); runtime-content-hash assertion unchanged.
- `services/cfs/build-image.sh` -- `docker events` capture added around the build (question 194).
- `services/cfs/IMAGE_DIGEST.md` -- new dated section for this round: BuildKit-capability finding,
  the live base-image-disappearance incident, the events-log path/filter, no digest/hash change
  (no re-pin was possible or needed).
- `services/cfs/Dockerfile` -- **not touched** (no experiment-only changes were made to it, so
  nothing needs reverting, and the currently-recorded pin's *definition* stays valid even though
  the local image bytes for it are currently absent -- see escalation 1).
- `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` -- **not touched** (the Dockerfile's `COPY` list did
  not change).
- This report, and this round's scratch evidence directory (referenced throughout as
  `_r6_4_scratch/NN_...`, actual path
  `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r6_4/`
  -- outside the repository tree, not a git-tracked path).

## 10. Summary in the required report format

### 1. What was built (paths)
- `services/cfs/tests/test_image_reproducibility.py` -- the whole-image digest assertion retired
  to reported-not-asserted (question 190's decision, taken after the bounded BuildKit experiment
  found zero BuildKit capability on this host); runtime-content hash assertion unchanged and still
  hard-asserted; module docstring rewritten to match. Section 3.6.
- `services/cfs/build-image.sh` -- a live `docker events` capture (filtered to `type=image`,
  `type=container`) wrapped around the script's own full execution window, writing
  `services/cfs/build/last-build-events.jsonl`; never fatal to the build if the capture itself
  fails; a combined `trap`-based cleanup so a real build failure is never hidden. Section 5.1.
- `services/cfs/IMAGE_DIGEST.md` -- new top-section note documenting the events-log path/filter;
  a new dated section recording the BuildKit-capability finding, the live base-image-disappearance
  incident, and that no re-pin happened or was needed this round.
- `services/cfs/Dockerfile`, `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` -- **not touched** (see
  section 9).

### 2. Verification (exact counts and digests)
- Baseline: `.venv/bin/python -m pytest -q services/cfs/tests/ -rs` -- **18 passed, 2 skipped in
  538.89s** (`_r6_4_scratch/00_baseline_pytest.txt`).
- BuildKit capability check: `docker buildx version`/`docker buildx ls` -> `unknown command`;
  `DOCKER_BUILDKIT=1 docker build` -> `ERROR: BuildKit is enabled but the buildx component is
  missing or broken` (`_r6_4_scratch/02_buildkit_capability_check.txt`).
- Part A code-change verification: `py_compile` exit 0
  (`_r6_4_scratch/03_py_compile.txt`); default-skip re-run, 1 skipped
  (`_r6_4_scratch/04_default_skip_after_edit.txt`); mocked-boundary break (real `AssertionError`
  against real R5.3 digests `sha256:d568bd98b3f8...` vs `sha256:972df8296c38...`,
  `_r6_4_scratch/06_break_old_code_fails.txt`) and restore (no exception,
  `_r6_4_scratch/07_restore_new_code_passes.txt`); `git diff` after restore matches the final
  intended change exactly (`_r6_4_scratch/08_git_diff_after_restore.txt`).
- Part B verification: three real local-only scratch-build runs --
  normal (`_r6_4_scratch/10_events_normal_run.txt`, real events captured), events-capture-start
  failure (`_r6_4_scratch/11_break_events_capture_unwritable.txt`, build still succeeds, exit 0),
  build failure (`_r6_4_scratch/12_break_build_failure_not_hidden.txt`, real failure text, exit
  127, not hidden). `git diff` on `build-image.sh` matches the intended change
  (`_r6_4_scratch/13_git_diff_build_image_sh.txt`).
- Final suite: **1 failed (pre-existing, out-of-scope, see escalation 5), 17 passed, 2 skipped in
  913.12s** (`_r6_4_scratch/14_final_verification.txt`).
- Recorded image digest and runtime-content hash in `IMAGE_DIGEST.md`: **unchanged** this round
  (`sha256:b1300c6fd3c0e323feae1be0db3f3c4b740ea84229c510ceaae7ee0ed5f8ad12` /
  `sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef`) -- no re-pin happened.

### 3. Measurements worth keeping
- This host has zero BuildKit capability (no `buildx` plugin, not installable without network) --
  a durable fact about this Docker/Colima installation, not just this round's incident. Section 3.
- The Docker daemon's own live `docker events` ring buffer on this host held only ~40 seconds of
  real history under typical shared-host load by the time it was queried retrospectively --
  concrete evidence that only a live, started-ahead-of-time capture (Part B) can work here, not a
  `--since` query after the fact. Section 2.
- `docker build` on this host silently falls back to the deprecated legacy builder whenever
  `DOCKER_BUILDKIT` is unset, and refuses outright (before doing anything) when it is set to 1,
  since the `buildx` plugin is absent. Section 3.2.
- R5.3's own already-recorded two-`--no-cache`-build comparison remains the most direct evidence
  for question 190: runtime-content hash matched exactly, file-level diff empty, whole-image `.Id`
  differed -- the gap is metadata/OCI-layer-history-shaped, not file-content-shaped. Section 3.5.

### 4. Defects found, including my own
- **Not mine, but discovered live this round:** the officially pinned image and its exact
  digest-pinned base image are both absent from this host's local Docker store -- a third
  occurrence of question 194's tracked incident, this time including the digest-pinned base
  itself, not just a floating tag. Section 2, escalation 1.
- **Not mine, out of scope, disclosed rather than silently worked around:**
  `test_clean_fetch_patches.py` timed out on a real `git clone` during this round's final
  verification run, despite passing cleanly in this round's own earlier baseline run -- confirmed
  not caused by either of my two changed files. Section 6, escalation 5.
- **My own, corrected before finalizing:** an early draft of section 5.3 quoted an invented,
  not-actually-captured warning/error string for the events-capture-failure and build-failure
  cases; caught by comparing against the real captured scratch-log text before finalizing this
  report, and corrected to the verbatim real output (section 5.3's current text). No code defect
  resulted -- this was a reporting-accuracy catch, not a bug in `build-image.sh` itself.
- No defects found in the pre-existing, unmodified logic of either file this round touches (the
  runtime-content-hash computation, the manifest-diff/COPY-parsing logic, `test_image_digest.py`)
  -- none of that was exercised differently by this round's changes.

### 5. Escalations for the manager
See section 7 for the full text; in priority order: (1) the live base/image disappearance
incident, blocking; (2) Part A's mocked-boundary verification needs a real end-to-end confirmation
once escalation 1 clears; (3) Part B's wrapper needs a real full-Dockerfile confirmation once
escalation 1 clears, plus a minor `.gitignore` gap for the new events-log file; (4) the
per-layer/`docker history` diagnostic for a matched mismatch pair is still missing from the
historical record; (5) `test_clean_fetch_patches.py`'s network-timeout flakiness, not mine to fix.

### 6. What remains, in priority order
See section 8 for the full text: escalation 1 (network-window authorization to restore the base
image) blocks everything else; escalations 2 and 3 are real end-to-end confirmations deferred by
escalation 1; escalation 4 is an optional historical-record completion; escalation 5 belongs to
another team.
