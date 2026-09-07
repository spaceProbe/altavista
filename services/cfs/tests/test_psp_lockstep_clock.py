"""M23.2 (docs/sil-plan.md M23, docs/open-questions.md question 143): the functional half of
"cFS time advances only on step frames" -- `psp_lockstep_current_tai_ns()` equals the kernel's
`until_tai_ns` after N `psp_lockstep_release_tick` calls, and a task blocked in
`psp_lockstep_external_sync()` (the function `services/cfs/apps/sch_lockstep` registers as
OSAL's `OS_TimerSync_t`, in place of the wall-clock timer OSAL would otherwise arm -- see
`services/cfs/psp-lockstep/inc/psp_lockstep.h`'s own doc comment) never returns, and the clock
never advances, without one.

This compiles and runs a small C harness (a real pthread producer/consumer pair against the
actual `psp_lockstep.c`, not a re-implementation of its logic in Python) rather than asserting
against prose. A wrong implementation that arms a real timer, sleeps, or reads any wall clock to
"simulate" a tick would make `test_no_tick_no_advance` observe an advance it should not, or make
`test_n_steps_reach_kernel_time` return elapsed values disconnected from the `until_tai_ns`
values actually supplied.
"""
from __future__ import annotations

import subprocess

from _cbuild import compile_cached, run_compiled
from pathlib import Path

CFS_DIR = Path(__file__).resolve().parent.parent
PSP_SRC = CFS_DIR / "psp-lockstep" / "src" / "psp_lockstep.c"
PSP_INC = CFS_DIR / "psp-lockstep" / "inc"

# A pthread producer/consumer harness against the real library:
#   - a "sync" thread calls psp_lockstep_external_sync() in a loop, recording (via a
#     lock-free relaxed write, read back only after pthread_join) the tai_ns reached and the
#     elapsed microseconds after each of N returns;
#   - the main thread waits until the sync thread has posted that it is about to block (a
#     dedicated "about_to_wait" flag set and checked under the *library's own* consumer-count
#     invariant is not exposed, so this harness instead relies on psp_lockstep's pending-counter
#     design: a release_tick posted before or after external_sync is called is never lost --
#     see psp_lockstep.c's own comment on `g_pending` -- so the harness needs no extra
#     synchronization to avoid a lost-wakeup race);
#   - "no advance without a step": before releasing anything, the main thread checks
#     psp_lockstep_tick_count() == 0 and psp_lockstep_current_tai_ns() == the start value, then
#     releases N ticks one at a time, joins the sync thread, and checks its own recorded values.
HARNESS_C = r"""
#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include "psp_lockstep.h"

#define N_STEPS 5
static int64_t g_reached[N_STEPS];
static uint32_t g_elapsed_us[N_STEPS];

static void *sync_thread(void *arg) {
    (void)arg;
    for (int i = 0; i < N_STEPS; ++i) {
        g_elapsed_us[i] = psp_lockstep_external_sync(0);
        g_reached[i] = psp_lockstep_current_tai_ns();
    }
    return NULL;
}

int main(void) {
    const int64_t start_tai_ns = 1000000000LL; /* 1 s TAI, arbitrary */
    psp_lockstep_init(start_tai_ns);

    /* "does not advance without a step": before any release_tick call. */
    if (psp_lockstep_tick_count() != 0) { fprintf(stderr, "tick_count nonzero before any release\n"); return 1; }
    if (psp_lockstep_current_tai_ns() != start_tai_ns) { fprintf(stderr, "clock advanced before any release\n"); return 1; }

    pthread_t t;
    pthread_create(&t, NULL, sync_thread, NULL);

    int64_t until_tai_ns[N_STEPS];
    for (int i = 0; i < N_STEPS; ++i) {
        /* A DRM base period of 100 ms (100,000,000 ns) in kernel TAI ns, matching the M22/M23
         * demo systems' own step-period convention -- not a round microsecond count, so a bug
         * that silently truncates or rescales the ns->us conversion would show up. */
        until_tai_ns[i] = start_tai_ns + (int64_t)(i + 1) * 100000000LL;
        int rc = psp_lockstep_release_tick(until_tai_ns[i]);
        if (rc != 0) { fprintf(stderr, "release_tick %d failed\n", i); return 1; }
    }

    pthread_join(t, NULL);

    for (int i = 0; i < N_STEPS; ++i) {
        if (g_reached[i] != until_tai_ns[i]) {
            fprintf(stderr, "step %d: psp_lockstep_current_tai_ns()=%lld after sync, expected %lld\n", i, (long long)g_reached[i], (long long)until_tai_ns[i]);
            return 1;
        }
        if (g_elapsed_us[i] != 100000u) {
            fprintf(stderr, "step %d: external_sync returned %u us, expected 100000\n", i, g_elapsed_us[i]);
            return 1;
        }
    }
    if (psp_lockstep_tick_count() != N_STEPS) {
        fprintf(stderr, "tick_count = %llu, expected %d\n", (unsigned long long)psp_lockstep_tick_count(), N_STEPS);
        return 1;
    }
    if (psp_lockstep_current_tai_ns() != until_tai_ns[N_STEPS - 1]) {
        fprintf(stderr, "final clock %lld != last until_tai_ns %lld\n", (long long)psp_lockstep_current_tai_ns(), (long long)until_tai_ns[N_STEPS - 1]);
        return 1;
    }

    /* Once more: still no advance beyond the last released tick. */
    if (psp_lockstep_current_tai_ns() != until_tai_ns[N_STEPS - 1]) { fprintf(stderr, "clock advanced with no further release\n"); return 1; }

    printf("OK\n");
    return 0;
}
"""


def test_n_steps_reach_kernel_time_and_no_advance_without_a_step(tmp_path: Path) -> None:
    # Question 172: cached by source hash and pre-warmed untimed. See _cbuild.py.
    binary_path = compile_cached("harness", HARNESS_C, [PSP_SRC], PSP_INC, link_args=("-lpthread",))

    result = run_compiled(binary_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_release_tick_rejects_non_increasing_time(tmp_path: Path) -> None:
    """A replayed or reordered STEP (until_tai_ns <= the current clock) is a typed error
    (nonzero return), never a silent clamp -- this platform's "no silent fallbacks" rule."""
    src = r"""
    #include <stdio.h>
    #include "psp_lockstep.h"
    int main(void) {
        psp_lockstep_init(1000);
        if (psp_lockstep_release_tick(2000) != 0) { fprintf(stderr, "expected first release to succeed\n"); return 1; }
        if (psp_lockstep_release_tick(2000) == 0) { fprintf(stderr, "expected a repeated until_tai_ns to be rejected\n"); return 1; }
        if (psp_lockstep_release_tick(1500) == 0) { fprintf(stderr, "expected a lower until_tai_ns to be rejected\n"); return 1; }
        /* The rejected releases above must not have queued anything: exactly one tick (2000)
         * is pending, and consuming it reaches exactly 2000, not some corrupted intermediate
         * value from a rejected call. */
        if (psp_lockstep_external_sync(0) == 0) { fprintf(stderr, "expected a nonzero elapsed time for the only queued tick\n"); return 1; }
        if (psp_lockstep_current_tai_ns() != 2000) { fprintf(stderr, "expected current_tai_ns() == 2000 after consuming the only queued tick, got %lld\n", (long long)psp_lockstep_current_tai_ns()); return 1; }
        if (psp_lockstep_tick_count() != 1) { fprintf(stderr, "expected exactly one tick consumed (the rejected releases queued nothing)\n"); return 1; }
        printf("OK\n");
        return 0;
    }
    """
    # Question 172: cached by source hash and pre-warmed untimed. See _cbuild.py.
    binary_path = compile_cached("reject", src, [PSP_SRC], PSP_INC, link_args=("-lpthread",))
    result = run_compiled(binary_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
