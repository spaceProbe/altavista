/* See sch_lockstep_app.h for the module doc comment.
 *
 * M23.4 FIX -- confirmed real, not hypothetical (found only by actually running this app
 * against a real kernel Step, not by static reading alone): M23.2's own top comment here used
 * to flag, as an open risk, "whether a single large elapsed-time jump reported by one
 * `external_sync` call can cause `OS_TimeBase_CallbackThread` to invoke this callback more than
 * once for that one tick" -- it does, badly. `third_party/cfs/osal/src/os/shared/src/
 * osapi-timebase.c`'s own `OS_TimeBase_CallbackThread` ties an `OS_TimerAdd`ed callback's
 * *count* to `tick_time / interval_time` (a `while (timecb->wait_time <= 0) { wait_time +=
 * interval_time; ...; if (saved_wait_time > 0) callback(); }` catch-up loop -- `saved_wait_time`
 * is captured ONCE before the loop and never re-checked per iteration, so the callback fires
 * once per `interval_time` unit of ACCUMULATED `tick_time`, not once per `external_sync`
 * return). With the old `SCH_LOCKSTEP_MIN_INTERVAL_US = 1` (1 microsecond), a single 100 ms
 * kernel Step (`tick_time = 100,000` us) fired the wakeup-dispatch callback **100,000 times**
 * for that one Step -- a real `docker run` against a real kernel wedged for minutes on the very
 * first Step (ADCS's own 16-deep `CmdPipe` flooded with ~100,000 wakeup transmits before it
 * ever caught up enough to actually process the star tracker/IMU packets already queued ahead
 * of them), not a fast, merely-redundant no-op. Raising `interval_time` instead (to something
 * larger than any real step period) does not fix this cleanly either -- once `wait_time`
 * overshoots positive by a large margin on the first tick, it never goes negative again on
 * later (much smaller) ticks, so the callback then fires ONCE and never again. `OS_TimerAdd`'s
 * own interval-counting design assumes a roughly-known, roughly-constant real timer period this
 * platform's lockstep-driven ticks do not have (a kernel Step's own elapsed `until_tai_ns` delta
 * is a configuration input, not knowable inside this app -- `sch_lockstep_app.h`'s own doc
 * comment on `step_period_ns` not being plumbed here).
 *
 * The actual fix: stop using `OS_TimerAdd`'s interval-counting callback dispatch for this at
 * all. `OS_TimeBaseCreate(..., psp_lockstep_external_sync)` alone (no `OS_TimerAdd`) already
 * spawns a background thread (`OS_TimeBase_CallbackThread`) that calls
 * `psp_lockstep_external_sync` in a tight loop forever, blocking inside it (via
 * `psp_lockstep.c`'s own `pthread_cond_wait`) until a tick is released -- exactly the
 * consumption this app needs, and it runs with zero registered timers just fine (the
 * timer-dispatch section of that loop is simply skipped when none exist). This app's own
 * `SCH_LS_AppMain` loop instead directly polls `psp_lockstep_tick_count()` (the same counter
 * that background thread increments exactly once per real tick consumed,
 * `psp_lockstep.h`'s own public, mutex-protected accessor) and transmits exactly one wakeup per
 * unit increase -- correct by construction, independent of any step period this app does not
 * know, and immune to the interval-counting bug above because it never uses that mechanism.
 * `crates/av-kernel/tests/drm_attitude_control_cfs.rs::run_the_cfs_bound_loop_settles_and_
 * tracks_the_native_run` is what would fail against a regression back to the `OS_TimerAdd`-based
 * design (a wedged container never reaches a passing measured pointing error).
 *
 * M23.4 reconciliation: this M23.2-era comment used to say the schedule-table dispatch (sending
 * each due slot's wakeup command onto the software bus, as SCH_LAB's own callback does) "belongs
 * here... is the reference ADCS app's own concern (M23.3)... and is not fabricated here" -- a
 * stated gap between the two concurrent workers' own scopes. This app and
 * `services/cfs/apps/adcs` are the only two flight apps this mission builds, so the "schedule
 * table" is exactly one fixed slot today: this app transmits the shared `LOCKSTEP_WAKEUP_MID`
 * command (`services/cfs/apps/shared/mission/lockstep_wakeup_mid.h`) once per tick, and
 * `adcs_app.c` subscribes to that same MsgId (`ADCS_WAKEUP_MID`) to run one control-law
 * evaluation per receipt -- see that shared header's own doc comment for why the MsgId lives in
 * one place now instead of two independently-guessed placeholders.
 */
#include "sch_lockstep_app.h"

#include "cfe.h"

#include "psp_lockstep.h"
#include "lockstep_wakeup_mid.h"

/* This app's own poll granularity for noticing a new tick via psp_lockstep_tick_count() (see
 * this file's own top comment for why polling a counter, not an OS_TimerAdd callback, is what
 * drives the wakeup). 1 ms adds at most ~1 ms of real wall-clock latency per kernel Step to a
 * container run -- immaterial next to a real gRPC/Unix-socket round trip, and nowhere near slow
 * enough to be mistaken for a wall-clock-driven step rate (the actual TAI time this app reports
 * anywhere, `psp_lockstep_current_tai_ns()`, comes only from released ticks, never from how
 * often this loop happens to wake up -- unaffected by this constant's own value). */
#define SCH_LOCKSTEP_POLL_DELAY_MS 1

static uint64_t g_tick_callback_count = 0;

/* The one schedule slot this mission dispatches today (M23.4) -- a bare command header, no
 * payload, initialized once in SCH_LS_AppMain and re-transmitted unchanged on every tick
 * (nothing about "which slot is due" needs per-tick state while there is only one). */
typedef struct
{
    CFE_MSG_CommandHeader_t CmdHeader;
} SCH_LOCKSTEP_WakeupCmd_t;

static SCH_LOCKSTEP_WakeupCmd_t g_wakeup_msg;

void SCH_LS_AppMain(void)
{
    uint32 run_status = CFE_ES_RunStatus_APP_RUN;

    CFE_ES_PerfLogEntry(1);

    if (CFE_EVS_Register(NULL, 0, CFE_EVS_EventFilter_BINARY) != CFE_SUCCESS)
    {
        CFE_ES_WriteToSysLog("SCH_LOCKSTEP: CFE_EVS_Register failed\n");
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }

    /* Initialized before the timebase is created (below) -- its background thread can start
     * consuming ticks the instant OS_TimeBaseCreate succeeds, and this app's own main loop
     * (further below) can start transmitting the very first wakeup as soon as it observes
     * psp_lockstep_tick_count() advance, so g_wakeup_msg must already be a valid, addressed
     * CFE_MSG before that point, never filled in lazily on first use. */
    CFE_MSG_Init(CFE_MSG_PTR(g_wakeup_msg.CmdHeader), CFE_SB_ValueToMsgId(LOCKSTEP_WAKEUP_MID), sizeof(g_wakeup_msg));

    osal_id_t timebase_id;
    /* The substantive change from SCH_LAB (question 143): a non-NULL external_sync, so OSAL
     * blocks this timebase's helper thread on psp_lockstep_external_sync() -- the kernel's own
     * ticks -- instead of arming a real POSIX interval timer against the wall clock. Deliberately
     * no OS_TimerAdd/OS_TimerSet call here (this file's own top comment explains why) -- the
     * timebase's own background thread still runs and consumes ticks (incrementing
     * psp_lockstep_tick_count()) with zero registered timers.
     */
    int32 status = OS_TimeBaseCreate(&timebase_id, "SCH_LOCKSTEP_TB", psp_lockstep_external_sync);
    if (status != OS_SUCCESS)
    {
        CFE_EVS_SendEvent(1, CFE_EVS_EventType_ERROR, "SCH_LOCKSTEP: OS_TimeBaseCreate failed: %ld", (long)status);
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }
    /* OS_TimeBaseSet's start_time/interval_time "have no effect for time bases that are using a
     * BSP-provided external_sync function" (osapi-timebase.h's own doc comment) -- called
     * anyway, with zeros, purely for symmetry with SCH_LAB's own call sequence; the values are
     * ignored by OSAL for this timebase. */
    OS_TimeBaseSet(timebase_id, 0, 0);

    CFE_EVS_SendEvent(4, CFE_EVS_EventType_INFORMATION, "SCH_LOCKSTEP: lockstep timebase armed");

    uint64_t last_seen_tick_count = psp_lockstep_tick_count();
    while (CFE_ES_RunLoop(&run_status))
    {
        CFE_ES_PerfLogExit(1);

        /* Dispatch the one schedule slot this mission has (see this file's own top comment) --
         * exactly one wakeup per tick actually consumed since the last time this loop checked,
         * never more (the interval-counting bug this file used to have) and never fewer (a `for`
         * loop, not an `if`, so a burst of more than one newly-consumed tick between two polls --
         * which this platform's own single-outstanding-Step lockstep protocol should never
         * produce, but is not assumed away here -- still gets one wakeup per tick, not one
         * wakeup for the whole burst). */
        uint64_t current_tick_count = psp_lockstep_tick_count();
        for (; last_seen_tick_count < current_tick_count; ++last_seen_tick_count)
        {
            g_tick_callback_count += 1;
            CFE_Status_t wakeup_status = CFE_SB_TransmitMsg(CFE_MSG_PTR(g_wakeup_msg.CmdHeader), true);
            static bool s_logged_first_wakeup_tx = false;
            if (!s_logged_first_wakeup_tx)
            {
                s_logged_first_wakeup_tx = true;
                CFE_EVS_SendEvent(5, CFE_EVS_EventType_INFORMATION, "SCH_LOCKSTEP: first wakeup transmit, status=0x%08X, tick_count=%llu", (unsigned int)wakeup_status, (unsigned long long)current_tick_count);
            }
        }

        OS_TaskDelay(SCH_LOCKSTEP_POLL_DELAY_MS); /* this app's own poll granularity for noticing
                                                       a new tick (see this file's own top
                                                       comment) -- NOT the tick source itself
                                                       (that is entirely the timebase's own
                                                       helper thread, driven by
                                                       psp_lockstep_external_sync, running
                                                       independently of this loop) and never
                                                       influences psp_lockstep's reported TAI
                                                       time, so this is not a forbidden
                                                       wall-clock read the way this platform's
                                                       own no-wallclock test scans for -- see
                                                       that test's own module doc comment for
                                                       exactly what it checks (psp_lockstep.c/.h
                                                       and the lockstep-local wire modules, not
                                                       this app's own idle poll delay). */
        CFE_ES_PerfLogEntry(1);
    }

    CFE_ES_ExitApp(run_status);
}
