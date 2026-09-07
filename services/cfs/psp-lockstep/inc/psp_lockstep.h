/* The "lockstep PSP timebase" (docs/open-questions.md question 143's decision: "a lockstep
 * platform-support layer and scheduler app we maintain, taking ticks from
 * `LockstepService.Step`, cFS otherwise unmodified"; docs/sil-plan.md M23).
 *
 * This is deliberately a small, standalone library, not a patch to OSAL's or PSP's own source
 * under third_party/cfs. OSAL already exposes exactly the extension point this needs:
 * `OS_TimeBaseCreate(&id, name, external_sync)` (osal/src/os/inc/osapi-timebase.h) --
 * "If the external_sync function is not NULL, this should point to a BSP-provided function that
 * will block the calling task until the next tick occurs. This can be used for synchronizing
 * with hardware events." `psp_lockstep_external_sync` below is exactly that function, matching
 * OSAL's own `OS_TimerSync_t` signature (`uint32 (*)(osal_id_t)`); `services/cfs/apps/sch_lockstep`
 * is the app that registers it in place of NULL (which is what makes ci_lab's stock sch_lab
 * instead arm a real POSIX interval timer against the wall clock -- exactly what M23.2 must
 * not do). No OSAL/PSP source is modified; see third_party/fetch-cfs.sh's own header comment
 * for the corresponding half of question 148's "record what had to be patched" (nothing).
 *
 * `psp_lockstep_release_tick` is called by `services/cfs/apps/io_lockstep` every time a
 * lockstep-local `STEP` frame arrives, carrying the kernel's own `until_tai_ns`
 * (`LockstepStepRequest.until_tai_ns`, `proto/altavista/v1/lockstep.proto`) -- the single point
 * where the kernel's clock enters this process. `psp_lockstep_external_sync` blocks on a
 * condition variable until that call happens; it is never satisfied by a timer, a sleep, or any
 * wall-clock read (see this module's own .c file: no `time()`, `gettimeofday()`, or
 * `clock_gettime()` anywhere -- `services/cfs/tests/test_psp_lockstep_no_wallclock.py` greps
 * for exactly that and fails the build if any appear). This is what "cFS time is the kernel's
 * time" cashes out to at the API level: `psp_lockstep_current_tai_ns()` never advances except
 * as a direct, traceable consequence of a `psp_lockstep_release_tick` call, and every advance is
 * to precisely the value that call was given -- never interpolated, extrapolated, or read from
 * anywhere else.
 */
#ifndef AV_CFS_PSP_LOCKSTEP_H
#define AV_CFS_PSP_LOCKSTEP_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Called once on the initial BIND, and again on every RESET (services/cfs/README.md's own "a
 * *bound* process can be power-cycled without a fresh Bind" contract -- both are "the clock
 * starts over at this value", so this function is deliberately re-entrant, not a one-shot).
 * `start_tai_ns`/`tai_ns` is the kernel's own `LockstepBindRequest.start_tai_ns` or
 * `LockstepResetRequest.tai_ns` -- the epoch this process's clock begins at, before its next
 * tick. Safe to call before the timebase that will use `psp_lockstep_external_sync` is created,
 * and safe even if a call to that function is already blocked waiting (see its own doc
 * comment): a real `docker run` of this build found that ordering race is not hypothetical --
 * `services/cfs/apps/sch_lockstep`'s timebase helper thread starts independently of
 * `services/cfs/apps/io_lockstep`'s own BIND handshake with the shim. */
void psp_lockstep_init(int64_t start_tai_ns);

/* Matches OSAL's `OS_TimerSync_t` (`osal/src/os/inc/osapi-timebase.h`): `timer_id` is unused
 * (OSAL passes it so a BSP with multiple timebases can tell them apart; this process has
 * exactly one lockstep-driven timebase) but kept in the signature so this function can be
 * passed to `OS_TimeBaseCreate` without a wrapper. Safe to call before `psp_lockstep_init` --
 * it blocks until that has happened too, not just until a tick (see `psp_lockstep_init`'s own
 * doc comment for why that ordering is not guaranteed). Otherwise blocks until the next
 * `psp_lockstep_release_tick` call (there is no other way for it to return -- no timeout, no
 * poll, no wall-clock read) and returns the elapsed time of that tick in microseconds, computed
 * as `(until_tai_ns_this_tick - until_tai_ns_previous_tick) / 1000` in pure integer arithmetic
 * (ADR-004: integer TAI ns). The very first tick's "previous" value is the `start_tai_ns` given
 * to `psp_lockstep_init`. Returns 0 if the elapsed interval does not fit `uint32_t` microseconds
 * (~71 minutes) -- callers needing a longer step period should not rely on this return value for
 * anything beyond a diagnostic, since `psp_lockstep_current_tai_ns()` is always exact regardless. */
uint32_t psp_lockstep_external_sync(uint32_t timer_id);

/* Called by the lockstep I/O app when a `STEP` frame's `LockstepStepRequest.until_tai_ns`
 * arrives. Enqueues the tick (a bounded FIFO -- see the .c file's own module doc comment for
 * why a queue, not a single "latest value" cell, is needed: the release and the scheduler's own
 * consumption of it happen on independently-scheduled OSAL tasks) for a future
 * `psp_lockstep_external_sync` call to consume, in order, one per call. `until_tai_ns` must be
 * strictly greater than every previously-released value (a lockstep run's ticks are strictly
 * increasing -- ADR-005); violating this, or releasing faster than the scheduler ever consumes
 * (the queue is bounded), is a typed error (nonzero return), never a silent clamp, wraparound,
 * or drop, so a bug upstream (a replayed or reordered STEP) is caught here rather than
 * corrupting cFS's own sense of time. Returns 0 on success, 1 if `until_tai_ns` was not
 * strictly greater than the last released value, 2 if the pending queue is full. */
int psp_lockstep_release_tick(int64_t until_tai_ns);

/* The `until_tai_ns` of the tick most recently *consumed* by a `psp_lockstep_external_sync`
 * call (or the `start_tai_ns` given to `psp_lockstep_init`, before the first tick is consumed)
 * -- "cFS's own clock" in the concrete sense this module provides it: what the scheduler has
 * actually ticked to, never a tick that is merely queued but not yet delivered. Never advances
 * on its own; see the module doc comment above. */
int64_t psp_lockstep_current_tai_ns(void);

/* Total number of ticks released so far (0 before the first `psp_lockstep_release_tick` call).
 * Exists so a test can observe "no advance happened" without racing
 * `psp_lockstep_external_sync`'s own blocking wait. */
uint64_t psp_lockstep_tick_count(void);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_PSP_LOCKSTEP_H */
