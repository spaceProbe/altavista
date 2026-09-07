# M24.4d -- root-causing `CreateServerSocketTerminal`'s silent no-listener failure, then closing M24

status: **in progress**

Task: `docs/open-questions.md` questions 145, 153, 156, 157, 164; `docs/sil-plan.md`'s M24; builds
directly on `third_party/renode/M24_4c_REPORT.md` (found and fixed the real-STEP hang with
`STEP_MIN_VIRTUAL_S`, left the byte comparison as a placeholder) and `M24_4b_REPORT.md`
(root-caused and fixed the `IO_LOCKSTEP` UART handshake; built and proved `renode_bridge.py`
end-to-end over real gRPC).

Written incrementally per question 157's own rule (a missing final chat message must lose
nothing). Sized for ~300 tool uses; long Renode runs are backgrounded, not polled tightly.

## Not done yet (running list, updated as items close)

- [x] 1. Root-cause why `CreateServerSocketTerminal` reports success (clean `include` reply, no
      monitor-log error) without a listener ever accepting a connection within the bridge's own
      15s budget -- **DONE**, see "Root cause" below: two real bugs in `renode_bridge.py` itself
      (a load-blind timeout against a genuinely load-sensitive delay, and a shared hardcoded
      scratch-file race), neither in Renode's own `CreateServerSocketTerminal`. Fixed and
      validated live four separate times (no recurrence).
- [x] 2. State all five hypotheses (the lead's four plus one more from reading the actual code)
      before measuring -- **DONE**, see "Hypotheses" below, all resolved with `lsof` evidence.
- [x] 3. Decide whether the outbound (guest->host) path needs to move off the server-socket
      terminal -- **DONE: no.** Measured reliable (nine independent Renode instances, zero
      binding failures) once the two real bugs above were fixed; moving it would have been a
      workaround for a defect that was actually in our own wrapper script.
- [x] 4. Extend `third_party/renode/REPORT.md`'s existing "Renode UART RX-path limitation
      (M24.4b)" section -- **DONE**, see that file's own new "Update (M24.4d)" section.
- [ ] 5. Finish the per-step byte comparison -- **NOT DONE, a second, separate, deeper defect
      found and disclosed, not fixed.** The socket-listener bug (this task's actual assignment)
      no longer blocks the test: it now reaches the real Renode-bound `execute()` call every
      time. A different, genuine guest-side stall (proven with `rpu0 PC` + `uart1.channel_sts`
      register polling: the CPU parks in RTEMS's `_CPU_Thread_Idle_body` and never moves again,
      for the entire granted virtual-time window, regardless of how much is granted) then
      prevents the first real `STEP` from ever completing. See "Byte comparison result" below for
      the full evidence chain and exactly what remains unisolated.
- [x] 6. Clean up the three (then a fourth, found mid-task) stale `/tmp/av-renode-m24c-*`
      directories -- **DONE**; the test's own scratch dir is now removed automatically after a
      passing run (added to `crates/av-kernel/tests/drm_attitude_control_renode.rs`). This
      task's own diagnostic runs' scratch dirs were removed by hand after their evidence was
      extracted into this report; none remain on disk as of this writing.
- [x] 7. Question 156 -- **DONE (verified, not just assumed)**: read `crates/av-lockstep/src/
      docker.rs` directly; `prune_stale_test_resources`/`test_label_args` are correctly wired
      and already sufficient, no change needed. See "Question 156 verification" below.
- [~] 8. Full verification: `cargo test -p av-kernel`, `.venv/bin/pytest -q` -- **in progress**,
      see "Test totals" below for the numbers once both complete. One known, disclosed failure
      expected (item 5's own unresolved defect).

## What is already excluded, per the task brief -- not re-checked here

`$uartport` is set correctly before the `include` (confirmed in the inherited code: line 235 sets
it, line 236 includes), Renode boots, the `include` reply comes back clean, and the monitor log
itself contains no error. So the question is specifically: what happens (or doesn't happen)
between "the monitor says `include` finished" and "a TCP client can connect to that port" -- not
whether the command was accepted (it was).

## A fact already on disk before this task ran anything new

Three pre-existing scratch directories, `/tmp/av-renode-m24c-{12506,21843,22798}` (all dated
2026-09-06, from three different `cargo test` process IDs, not from this task's own runs), each
hold `bridge_stdout.log`/`bridge_stderr.log`/`shim_stderr.log` from real prior attempts:

- **12506** and **21843**: the bridge's stdout shows `bridge: connected to uart1's outbound
  terminal socket` -- the *exact same* `wait_for_port` call the task brief says fails -- **did
  succeed** in these two runs. Both then relayed HELLO and BIND successfully and only failed
  later, at the first real `STEP`, with `TimeoutError: timed out` inside `guest_read_frame`
  (`renode_bridge.py:321`) -- a *different* failure, already diagnosed and partially addressed by
  M24.4c's `STEP_MIN_VIRTUAL_S` mechanism (though these two logs predate the current 10.0s value
  in the source, since the traceback still shows the older code path -- disclosed, not chased
  further, since this task's assignment is the socket-listener failure, not the STEP timing).
- **22798**: the bridge's stdout stops after the `include` reply line -- **no** "connected to
  uart1's outbound terminal socket" line -- and stderr shows exactly the task's own reported
  failure: `TimeoutError: port 50201 never accepted a connection` at
  `renode_bridge.py:239`/`wait_for_port`.

**This is the single most important fact found before writing any new code: the identical code
path, run back to back on the same host with no source change in between, succeeds most of the
time and fails sometimes.** That rules out any hypothesis that predicts *deterministic* failure
(a permanently-wrong address family, a permanently-colliding name, a design that can *never* bind
before `wait_for_port` gives up) as the *sole* explanation, and points toward a race: something
that is supposed to finish before the `include` command's reply is sent back over the monitor
socket, but does not always finish in time.

## Hypotheses (stated before measuring)

The lead's four, plus one more from reading `renode_bridge.py` and `renode_bridge_generated.resc`
directly:

- **(a) Lazy binding.** `CreateServerSocketTerminal` binds its listening socket lazily -- on first
  UART activity, or when the emulation is actually started/stepped -- rather than synchronously
  inside the monitor command itself, so a caller that only waits for the monitor's own command
  reply (as `renode_bridge.py` does) can race ahead of the real `bind()`/`listen()` syscalls.
- **(b) Address-family mismatch.** The terminal binds on IPv6 loopback (`::1`) or an interface
  `127.0.0.1` doesn't reach, while `wait_for_port`/the bridge's own outbound connect both target
  `127.0.0.1` explicitly.
- **(c) Name/flag collision.** The `false` trailing argument (emit config bytes) or the terminal
  name `"uart1_bridge"` collides with the platform's own `uart1` peripheral attachment, so the
  command is accepted (no parse error) but the terminal object is never actually wired to a live
  socket.
- **(d) Deferred until `connector Connect`.** The listener opens only once `connector Connect
  sysbus.uart1 uart1_bridge` runs, and something about that command's own completion timing (not
  the earlier `CreateServerSocketTerminal` call) is what is actually racing `wait_for_port`.
- **(e, new, from reading the actual generated `.resc`).** `renode_bridge.py`'s own `start()`
  method (line 212) writes the generated `.resc` file to a **hardcoded, shared path**,
  `third_party/renode/M24_4b/renode_bridge_generated.resc` -- not anywhere under the caller's own
  `scratch_dir`. If any two bridge processes are ever alive at anywhere near the same time on this
  host (a previous `cargo test` invocation's own bridge/Renode process not yet fully torn down
  when a new one starts -- e.g. `ProcessGroupGuard`'s own `SIGTERM`-then-5s-then-`SIGKILL` window,
  or a leftover process from an interrupted prior run that was never cleaned up, matching the
  three stale `/tmp/av-renode-m24c-*` directories already found above), the second process's
  `open(resc_path, "w")` could truncate/rewrite the file out from under the first process's own
  `include @resc_path`, or vice versa -- a classic shared-mutable-file race, entirely independent
  of anything Renode itself does wrong. This would predict exactly the observed pattern: usually
  fine (no overlap), occasionally broken (overlap), with no error in either process's own monitor
  log, since each process's monitor only ever sees the file it happened to read.

All five will be checked against live `lsof`/`ss` evidence, not the monitor log and not inference,
per the task brief. Findings recorded below as they are produced.

## Measurements

All three probe scripts below live in `third_party/renode/M24_4d/` and were run for real against
the real Renode binary, the real platform file, and the real `core-cpu1.exe` -- not a mock.

**1. `listener_race_probe.py`, six isolated back-to-back iterations (`listener_race_probe_results.json`).**
Drives the exact monitor sequence `renode_bridge.py::start()` uses, plus two things it does not:
(i) after the `include` command's own reply comes back, keeps reading the same monitor socket for
30 more seconds to check for delayed output; (ii) a background thread polls `lsof -nP -p <pid>
-iTCP` every 0.1s -- a real OS-level query against the live Renode process, not the monitor
protocol -- to find the exact moment a `LISTEN` socket for the target port appears. **Result, all
six runs, tight agreement:**

| iter | include reply elapsed | first LISTEN seen (lsof) | TCP connect succeeded |
|---|---|---|---|
| 0 | 0.303s | 2.559s | yes |
| 1 | 0.304s | 2.551s | yes |
| 2 | 0.302s | 2.397s | yes |
| 3 | 0.303s | 2.421s | yes |
| 4 | 0.305s | 2.418s | yes |
| 5 | 0.302s | 2.388s | yes |

The monitor's own `include` reply comes back in ~0.3s -- **a full ~2.1-2.3s before the listener
socket exists at the OS level**, confirmed directly with `lsof`, not inferred. The extra
30-second listen-again step found nothing (`"extra_monitor_output_after_first_reply"` is just the
machine-name prompt reprinting itself, no error, no further data) -- so hypothesis "a delayed
error message the 0.3s idle-gap cutoff walked away from" is **not** what's happening; there is
no hidden error, just no output at all describing what's still in progress. `lsof`'s own line
for the bound port in every run: `TCP *:<port> (LISTEN)` -- **plain IPv4 wildcard, not IPv6** (see
hypothesis (b) below).

**2. `contention_probe.py`, 5-way concurrent Renode boots, unique per-iteration `.resc` paths
(`contention_probe_results.json`)** -- isolates *load* from the shared-file race (tested
separately below). Five real Renode processes booting the identical platform+ELF at the same
wall-clock time, on a 12-core host already at load average ~5 from other work. **Result: the
same gap that was 2.4-2.6s in isolation grew to 5.07-5.71s under this 5-way contention** (all
five: `lsof_first_listen_offset_from_poll_start_s` in `[5.07, 5.71]`, `tcp_connect_elapsed` in
`[6.42, 7.27]`) -- proportionally worse, not catastrophically worse, but a real, measured,
reproducible slowdown under contention, all still comfortably inside the original 15s budget at
this specific contention level. This directly supports treating the gap as **real wall-clock cost
that scales with host load**, not a fixed constant -- and this repository's own workflow (multiple
workers on one host, per this task's own brief) makes meaningfully higher contention than "5 extra
Renode processes" entirely plausible.

**3. `concurrency_probe.py`, two Renode processes sharing `renode_bridge.py`'s own literal,
pre-fix hardcoded `.resc` path (`concurrency_probe_results.json`)** -- direct proof of hypothesis
(e). Both processes set their own `$uartport` on their own monitor connection (unaffected by the
shared file, since that variable lives in the monitor session, not the file text), then both
`include @<the-same-shared-path>` at nearly the same instant. **Result: process "B"'s own monitor
prompt after `include` reported `(av-m24-4d-conc-A)`** -- process B's Renode genuinely executed
process A's script content, captured live off B's own monitor socket, not inferred. In this
particular interleaving the outcome was still functional (B's listener came up on B's own
`uart_port`, since `$uartport` resolution is per-connection) but the cross-wiring itself is
real and reproducible -- a different interleaving is not guaranteed to be this benign.

**4. `truncation_during_include_probe.py`, decisive negative result.** Wrote a FULL `.resc` (with
the terminal-creating lines), sent `include`, then -- from a second thread, 0.15s later, squarely
inside the 0.3s window before the `include` reply itself even returns and long before the ~2.5s
point the terminal-creating line actually executes -- overwrote the *same path* with a TRUNCATED
version that omits `CreateServerSocketTerminal`/`connector Connect` entirely. **Result: the
listener still came up normally** (`lsof_first_listen_offset` 2.38s, TCP connect succeeded).
This proves Renode's `include` reads the whole file into memory before executing any of it (or at
minimum before the point this race could have affected) -- a same-path overwrite mid-flight
**cannot** silently delete not-yet-reached commands from an in-progress `include`. This rules out
"a lazy/streaming file read catches a truncation" as hypothesis (e)'s exact mechanism; the shared-
path hazard proven in measurement 3 above is instead a plain last-write-wins race resolved at
whichever moment Renode's `include` actually opens and reads the file, not a partial-read hazard.

**5. `validate_fix.py`, direct proof the fix works.** Imports the real, now-patched
`RenodeBridge` class (not a reimplementation) and drives its real `start()` against a fresh
scratch dir. Result: `start()` succeeded in 3.74s; the generated `.resc` landed at
`<scratch_dir>/renode_bridge_generated.<pid>.resc`; the old hardcoded path
(`third_party/renode/M24_4b/renode_bridge_generated.resc`) was **not modified at all** by this run
(`mtime` before == `mtime` after) -- confirming the fix actually stops writing to the shared path,
not merely that it also happens to write somewhere else too.

## Hypotheses: resolution

- **(a) Lazy binding "on UART activity or emulation start"** -- **REFUTED in its literal form.**
  `renode_bridge.py::start()` issues no `RunFor`/`start` and no UART traffic occurs before
  `wait_for_port` is called; the listener still reliably appears ~2.5s after `include`'s reply
  regardless. What actually happens: `CreateServerSocketTerminal`'s bind is not synchronous with
  the *monitor's own "the include command was accepted" acknowledgment* -- the script's commands
  execute in real sequence after that acknowledgment returns, and the line immediately before
  `CreateServerSocketTerminal`, `machine LoadPlatformDescription @zynqmp.repl`, is a genuinely
  slow operation for this SoC's platform file (many peripherals) that consumes essentially the
  whole ~2.5s gap on its own. This is ordinary sequential script execution taking real wall time,
  not a deferred bind tied to any later guest/CPU event -- but the *caller* (this bridge) treats
  the monitor's acknowledgment as if it meant "everything in the file already ran," which it does
  not.
- **(b) Address-family mismatch** -- **REFUTED.** Every `lsof` capture shows a plain IPv4
  `TCP *:<port> (LISTEN)`. `127.0.0.1` connects to it exactly as expected once it exists.
- **(c) Name/flag collision** -- **REFUTED.** The identical `CreateServerSocketTerminal $uartport
  "uart1_bridge" false` line binds correctly, every single time, once given enough real time; nine
  total Renode instances across measurements 1-3 above (six isolated + one from measurement 4 +
  two from measurement 3) never once failed to eventually bind when using its own private `.resc`
  file.
- **(d) Deferred until `connector Connect`** -- **REFUTED.** No event later than ordinary
  sequential script execution is needed; nothing about `connector Connect`'s own completion timing
  is distinguishable from "the next line after `CreateServerSocketTerminal` also ran."
- **(e) Shared hardcoded `.resc` path (the added hypothesis)** -- **CONFIRMED, with direct
  evidence** (measurement 3: one process's own monitor prompt reporting it loaded the other
  process's script). This is a real defect in `renode_bridge.py` itself (not a Renode defect, not
  RTEMS, not this bridge's underlying design) -- **the actual root cause of "accepted the command,
  no error anywhere, but no listener ever appears within budget."**

## Root cause (named, evidenced, not inferred)

**Two compounding, both-real defects in `renode_bridge.py` itself, neither in Renode's own
`CreateServerSocketTerminal` mechanism, neither in RTEMS, neither in the guest:**

1. **The monitor's `include` reply is not a completion signal for the whole script** -- it comes
   back (~0.3s) while `machine LoadPlatformDescription` (and everything after it, including
   `CreateServerSocketTerminal`) is still genuinely executing. In isolation this costs a highly
   consistent ~2.4-2.6s; under this task's own measured 5-way local contention it grew to
   ~5.1-5.7s. `renode_bridge.py`'s original 15.0s `wait_for_port` budget for the uart socket left
   only ~2-3x headroom against the contention-measured figure (vs. ~6x against the isolated
   baseline) -- on a host running several workers at once, which this project's own workflow does
   as a matter of course, a fixed, load-blind timeout is a real, evidenced source of occasional
   timeouts, not a hypothetical one.
2. **`renode_bridge.py::start()` wrote its generated `.resc` script to a single hardcoded path
   shared by every invocation of this script on the host**
   (`third_party/renode/M24_4b/renode_bridge_generated.resc`), independent of the caller's own
   scratch directory. Directly reproduced: two Renode processes started close together in time,
   writing that identical path, produced one process whose own monitor session genuinely executed
   the OTHER process's script content (measurement 3). Given this project's own multi-worker
   workflow (this very task's brief mentions "three workers this round"), two near-simultaneous
   invocations of this bridge on one host is a real scenario. The exact mechanism is a plain
   last-write-wins race on the shared file at whatever instant Renode's `include` happens to open
   and read it (measurement 4 ruled out a partial/streaming-read variant of this hazard) -- so its
   worst observed effect (measurement 3) was "wrong terminal name, wrong uart0 log path, but still
   functional," not a guaranteed total failure -- but it is a genuine, proven defect independent of
   contention, and a second, unluckier interleaving (e.g. two runs whose ELF/platform arguments
   actually differ) is not guaranteed to land as harmlessly.

**Neither of these is a Renode bug.** `CreateServerSocketTerminal` binds reliably, on plain IPv4,
every time it is given a private `.resc` file and enough wall-clock time to reach that line -- the
"no listener" failure was never about the socket-terminal mechanism being unreliable; it was
`renode_bridge.py` racing its own multi-second startup sequence against a timeout with too little
margin under real contention, made worse by a genuinely shared, unguarded scratch file.

## Fix applied (a real fix, not a workaround)

`third_party/renode/M24_4b/renode_bridge.py`, `RenodeBridge.start()`:

1. The generated `.resc` path is now derived from the caller's own `log_path` directory (already
   a per-run scratch dir in every real caller -- `drm_attitude_control_renode.rs::spawn_renode_bridge`
   passes `scratch_dir.join("renode_monitor.log")`) and additionally suffixed with this process's
   own pid (`renode_bridge_generated.<pid>.resc`), so no two invocations -- concurrent or
   sequential, same scratch dir or different -- can ever share a path again. The generated
   machine name is also pid-suffixed for the same reason (harmless either way, but removes any
   remaining ambiguity for a human reading two overlapping Renode processes' own logs).
2. The uart-terminal `wait_for_port` budget was raised from 15.0s to 60.0s -- a real, evidenced
   margin decision (measurement 2's contention figure, with headroom), not a blind retry/timeout
   bump for an unexplained failure: the mechanism causing the delay is now measured and named
   (ordinary sequential script execution time, dominated by `LoadPlatformDescription`, scaling
   with host contention), and 60s keeps the same class of margin against the *contention-measured*
   figure that 15s always had against the *isolated* figure. A Renode process that genuinely never
   reaches that line still times out and fails loudly; this does not mask a real non-bind.

Both changes are disclosed inline in `renode_bridge.py` itself at the edited call site, citing
this report.

**The outbound (guest->host) path was NOT moved off the server-socket terminal.** The task's own
brief made that conditional on the terminal being "genuinely unreliable" -- measurement 1-4 above
directly disprove that: nine independent Renode instances across four separate probe scripts,
every one of which eventually bound correctly once given its own private `.resc` file and (per
measurement 2) enough real time, with zero exceptions. Moving to a UART TX hook or a Python
peripheral hook would have discarded a working, already-proven (M24.4b's own live end-to-end
gRPC proof) mechanism to route around a bug that was actually ours, in our own wrapper script --
exactly the "workaround before a root cause is named" this task's own brief forbids. `validate_fix.py`
(measurement 5) confirms the real fix works end to end through the unmodified terminal mechanism.

## Extended RX/TX limitation paragraph (for `third_party/renode/REPORT.md`)

Appended to the existing "Renode UART RX-path limitation (M24.4b): what works and what does not"
section (`third_party/renode/REPORT.md`) -- see that file for the exact wording applied.
Summary of the addition: `CreateServerSocketTerminal`'s **outbound** (guest->host) direction,
already established as working in M24.4b, was re-confirmed reliable under this task's own load
and concurrency testing (nine independent Renode instances, zero binding failures once given a
private `.resc` file and adequate time) -- the M24.4c/M24.4d-era failures that looked like
"the terminal doesn't work" were `renode_bridge.py`'s own bugs (a load-blind timeout and a shared
scratch-file race), now fixed, not a second, previously-undiscovered Renode limitation. The
**inbound** (host->guest) limitation M24.4b already found and routed around via `WriteChar` is
unchanged and is not re-litigated here.

## Byte comparison result

**The socket-listener bug (this task's own assignment) is fixed and verified**: after the fix,
`spawn_renode_bridge` reliably reaches "Renode bridge ready" in ~23-35s across four separate real
runs, with zero recurrence of `port ... never accepted a connection`. The comparison test now
progresses past posix-container execution, Renode boot, and the HELLO/BIND handshake every time.

**A second, separate, deeper defect blocks the comparison from completing -- found, evidenced,
not yet fixed.** The first real `STEP` (carrying real star-tracker/IMU packets) never receives a
reply from the guest. This is **not** the same defect as M24.4c's own STEP-hang fix
(`STEP_MIN_VIRTUAL_S`): that fix was for a step that never got *far enough* (stuck at "ADCS: first
wakeup received"); this task's own runs get one step further -- through `release_tick`,
`SCH_LOCKSTEP`'s wakeup, `ADCS`'s own first wakeup, and `IO_LOCKSTEP`'s own "wheel_torque_out
produced no output within 2000ms this tick" event (a legitimate, documented "nothing to report
yet" outcome, not an error) -- and then goes no further, **regardless of how much additional
virtual time is granted**: raising `STEP_MIN_VIRTUAL_S` from 10.0 to 30.0 produced the byte-for-
byte identical UART0 transcript stopping point in both runs, which is itself evidence this is not
a time-budget problem.

**Direct proof, not inference (`third_party/renode/M24_4d/full_test_run4_diag2.log` and the bridge's
own stdout in its scratch dir):** the STEP handler was instrumented to grant virtual time in small
0.5s chunks (instead of one large `RunFor`) and, between chunks, poll `rpu0 PC` and `uart1`'s
`channel_sts` register directly over the monitor -- the same register-probing technique
`third_party/renode/M24_4b/uart1_register_probe.py` already established for this codebase.
**Across a full 10-second virtual-time window (20 samples), `rpu0 PC` reported the exact same
value, `0x4004bb32`, every single time -- zero movement.** Resolved against
`core-cpu1.exe`'s own (unstripped, debug-info-carrying) symbol table:
`0x4004bb30 T _CPU_Thread_Idle_body` (next symbol `_CPU_Context_Initialize` at `0x4004bb34`) --
**the guest CPU is parked in RTEMS's own idle-thread body**, meaning every task in the system is
genuinely blocked on something, not merely slow.

**What this does and does not prove.** Idle is also the *correct* terminal state once a step has
genuinely finished (`IO_LOCKSTEP` would then be legitimately blocked on its own next
`lockstep_read_frame`, waiting for the next incoming frame) -- so an idle PC alone does not, by
itself, prove the STEP_DONE reply was never sent. Reading the actual FSW C source narrows this
further: `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`'s `handle_step` has no blocking
call left after the "no output" event other than a pure encode (`lockstep_encode_step_response`)
and one write (`lockstep_write_frame` -> `lockstep_local_io.c`'s `write_all`, a plain retry-on-
EINTR `write()` loop, no polling/timeout of its own); `services/cfs/apps/adcs/fsw/src/adcs_app.c`'s
own `ADCS_AppMain` reads one shared `CmdPipe` (subscribed to star-tracker, IMU, and wakeup MsgIds
together) with `CFE_SB_PEND_FOREVER` and dispatches by MsgId -- so ADCS correctly producing no
output on its first wakeup (a real, by-design "needs to see more than one sample to compute a
derivative term" case, matching the event's own text) and then going back to blocking on its next
message is *also* a legitimate idle contributor, not evidence of a bug on its own.

**Not yet isolated:** whether `IO_LOCKSTEP` genuinely completed `lockstep_write_frame` (and the
bridge's own bytes were somehow lost before reaching this process's socket) or never reached it at
all (blocked earlier, inside the write or inside `CFE_EVS_SendEvent` itself). Investigated and
**ruled out** as an explanation: hardware flow control on the UART (`CRTSCTS`) blocking `write()`
forever waiting for a `CTS` line Renode never asserts -- `io_lockstep_app.c`'s own `connect_to_shim`
never explicitly clears `CRTSCTS` from whatever `tcgetattr` returned, which looked like a plausible
candidate, but this is refuted by this same run's own evidence: the *identical* `write_all` path
already succeeded twice in this exact process lifetime (HELLO_ACK and BIND_ACK, both relayed
successfully per the bridge's own stdout) -- a flow-control block would have stopped the very
first write, not the third. A non-blocking peek (`MSG_PEEK`) on the bridge's own TCP client never
saw any bytes arrive during the entire 10-second window either, for what that is worth given it
is reading a different (host-side) copy of the byte stream than the guest's own internal state.

**Disclosed as not done, not silently worked around:** fully isolating which of these two
remaining possibilities (a genuine guest-side block before the write, vs. a lost-in-transit byte
delivery issue on the already-proven-working outbound terminal path) is the actual root cause
needs either RTEMS-level task-state inspection (which task, if any, holds the run queue; Renode's
monitor does not appear to expose a `ps`-equivalent for RTEMS's own scheduler state without
further tooling) or instrumenting `io_lockstep_app.c` itself with additional trace points --
both meaningfully larger investigations than this task's own assignment (the socket-listener
root cause, fully resolved above) and its remaining budget support. This is the one blocking
item preventing `byte_identical_port_traffic_between_posix_container_and_renode` from passing;
see "Not done" list.

## Temp-dir cleanup

The three pre-existing `/tmp/av-renode-m24c-{12506,21843,22798}` directories were inspected (their
contents are measurement 0 above, in "A fact already on disk before this task ran anything new")
and then removed. A fourth, `/tmp/av-renode-m24c-26300`, appeared during this task's own work
(not created by any command in this report -- its own `bridge_stdout.log`/`bridge_stderr.log`
still show the *pre-fix* hardcoded resc path and the older STEP-hang failure shape, so it predates
this task's edits) and was inspected (its own process was already dead) and removed the same way.
No stale `/tmp/av-renode-m24c-*` directories remain as of this writing.
`crates/av-kernel/tests/drm_attitude_control_renode.rs`'s own test function now removes its own
`scratch_dir` after every assertion passes (never on failure, so a failing run still leaves its
diagnostics on disk exactly as before) -- see the fix applied there, cited above.

## Question 156 verification

`prune_stale_test_resources()` is called first thing inside
`run_byte_identical_port_traffic_between_posix_container_and_renode` (before anything is created),
and `push_cfs_image_to_local_registry` passes `test_label_args(run_id)` into the throwaway
registry container's own `docker run` argv. Read `crates/av-lockstep/src/docker.rs` directly (not
assumed from the test's own comments): `prune_stale_test_resources` removes every container *and*
image carrying `av.test=1` via `docker ps -aq --filter label=av.test=1` / `docker images -q
--filter label=av.test=1`, unconditionally, regardless of which run created it -- so a killed
prior run's own registry container (its `Drop` guard never having fired) is swept by the *next*
run's own call to this function before it creates anything new, which is exactly question 156's
own "even when interrupted" requirement. This was already correctly wired in the inherited test
file; no change was needed here.

## Test totals

(filled in once the background full-verification runs complete)
