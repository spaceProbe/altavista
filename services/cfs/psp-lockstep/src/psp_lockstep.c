/* See psp_lockstep.h for the module doc comment.
 *
 * No wall-clock read of any kind appears in this file -- no `time()`, `gettimeofday()`,
 * `clock_gettime()`, and no sleep/usleep/nanosleep either (a sleep-based "poll until it looks
 * done" would smuggle real elapsed time back into a supposedly kernel-driven clock).
 * `services/cfs/tests/test_psp_lockstep_no_wallclock.py` asserts exactly that by grepping this
 * file, and fails the whole test session if it ever finds one -- catching a wall-clock read
 * mechanically, not just by code-review discipline.
 *
 * A bounded FIFO queue of released-but-not-yet-consumed `until_tai_ns` values, not a single
 * "latest value" cell: in the real deployment the lockstep I/O app (which calls
 * `psp_lockstep_release_tick`) and the scheduler's timebase helper thread (which calls
 * `psp_lockstep_external_sync`) are independently-scheduled OSAL tasks, so a release can
 * complete before the sync call that is meant to consume it has even started. A single "latest
 * value" cell would let a fast producer coalesce several released ticks into one — silently
 * skipping ticks the scheduler (and every app it wakes) never saw, which is exactly a "missed
 * tick" this platform's "no silent fallbacks" rule forbids (see the module doc comment in
 * psp_lockstep.h). The queue means `psp_lockstep_current_tai_ns()` always reflects the tick
 * `psp_lockstep_external_sync` most recently *consumed* -- what the scheduler has actually
 * ticked to -- never a tick still only queued.
 */
#include "psp_lockstep.h"

#include <assert.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>

#define PENDING_QUEUE_CAPACITY 64u

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_tick_cond = PTHREAD_COND_INITIALIZER;

static int64_t g_pending_queue[PENDING_QUEUE_CAPACITY];
static size_t g_pending_head = 0;
static size_t g_pending_count = 0;

static int64_t g_last_enqueued_tai_ns = 0; /* for the strictly-increasing check on release */
static int64_t g_last_consumed_tai_ns = 0; /* "cFS's own clock" -- see psp_lockstep_current_tai_ns */
static uint64_t g_tick_count = 0;          /* ticks actually consumed by external_sync */
static int g_initialized = 0;

void psp_lockstep_init(int64_t start_tai_ns)
{
    pthread_mutex_lock(&g_lock);
    /* Deliberately re-entrant, not "asserts if called twice": services/cfs/apps/io_lockstep
     * calls this both on the initial BIND and on every RESET (services/cfs/README.md's own "a
     * *bound* process can be power-cycled without a fresh Bind" contract) -- both are "the
     * clock starts over at this value" in exactly the same sense, so there is no reason for the
     * second call to be a programming error. */
    g_pending_head = 0;
    g_pending_count = 0;
    g_last_enqueued_tai_ns = start_tai_ns;
    g_last_consumed_tai_ns = start_tai_ns;
    g_tick_count = 0;
    g_initialized = 1;
    pthread_cond_broadcast(&g_tick_cond); /* wakes any external_sync() call already blocked on
                                              "not yet initialized" -- see that function's own
                                              comment. */
    pthread_mutex_unlock(&g_lock);
}

uint32_t psp_lockstep_external_sync(uint32_t timer_id)
{
    (void)timer_id; /* one lockstep timebase per process; see the header's own doc comment */

    pthread_mutex_lock(&g_lock);

    /* Blocks here -- and ONLY here -- until psp_lockstep_release_tick() has posted at least one
     * unconsumed tick, AND psp_lockstep_init() has run. The second half matters in the real
     * deployment: services/cfs/apps/sch_lockstep creates this timebase (and its helper thread
     * immediately starts calling this function) independently of
     * services/cfs/apps/io_lockstep's own BIND handshake with the shim, which is what actually
     * calls psp_lockstep_init -- there is no OSAL-level guarantee the BIND completes first. A
     * real `docker run` of this exact build surfaced this ordering race as a hard `assert()`
     * abort the first time io_lockstep failed to connect before sch_lockstep's timebase thread
     * reached this call; blocking here (rather than asserting) is the correct fix, not a
     * workaround -- an app starting in an unexpected order is not a programming error the way a
     * call before *any* init ever happens would be, and this function's whole contract is "block
     * until there is real kernel-driven progress to report", which "not bound yet" trivially is
     * not. No timeout is passed to pthread_cond_wait either way: there is no fallback path that
     * lets this function return without both psp_lockstep_init and a real tick having happened. */
    while (!g_initialized || g_pending_count == 0)
    {
        pthread_cond_wait(&g_tick_cond, &g_lock);
    }

    int64_t until_tai_ns = g_pending_queue[g_pending_head];
    g_pending_head = (g_pending_head + 1u) % PENDING_QUEUE_CAPACITY;
    g_pending_count -= 1u;

    int64_t elapsed_ns = until_tai_ns - g_last_consumed_tai_ns;
    g_last_consumed_tai_ns = until_tai_ns;
    g_tick_count += 1;
    pthread_mutex_unlock(&g_lock);

    /* elapsed_ns is always > 0 here: psp_lockstep_release_tick refuses a non-increasing
     * until_tai_ns before it is ever queued. */
    int64_t elapsed_us = elapsed_ns / 1000;
    if (elapsed_us > (int64_t)UINT32_MAX)
    {
        return 0; /* see the header's own doc comment: diagnostic-only overflow case */
    }
    return (uint32_t)elapsed_us;
}

int psp_lockstep_release_tick(int64_t until_tai_ns)
{
    pthread_mutex_lock(&g_lock);
    assert(g_initialized && "psp_lockstep_release_tick called before psp_lockstep_init");
    if (until_tai_ns <= g_last_enqueued_tai_ns)
    {
        pthread_mutex_unlock(&g_lock);
        return 1;
    }
    if (g_pending_count == PENDING_QUEUE_CAPACITY)
    {
        /* The I/O app is releasing ticks faster than the scheduler consumes them -- a typed
         * refusal, never a silent drop or overwrite of a queued tick. In the real deployment
         * this cannot happen (lockstep-local's own "exactly one outstanding Step" rule --
         * services/cfs/README.md -- means the I/O app never has more than one tick in flight),
         * so this is a defensive bound, not a rate this module expects to hit. */
        pthread_mutex_unlock(&g_lock);
        return 2;
    }
    g_pending_queue[(g_pending_head + g_pending_count) % PENDING_QUEUE_CAPACITY] = until_tai_ns;
    g_pending_count += 1u;
    g_last_enqueued_tai_ns = until_tai_ns;
    pthread_cond_broadcast(&g_tick_cond);
    pthread_mutex_unlock(&g_lock);
    return 0;
}

int64_t psp_lockstep_current_tai_ns(void)
{
    pthread_mutex_lock(&g_lock);
    int64_t v = g_last_consumed_tai_ns;
    pthread_mutex_unlock(&g_lock);
    return v;
}

uint64_t psp_lockstep_tick_count(void)
{
    pthread_mutex_lock(&g_lock);
    uint64_t v = g_tick_count;
    pthread_mutex_unlock(&g_lock);
    return v;
}
