/* The lockstep scheduler app: M23.2's replacement for SCH_LAB (docs/open-questions.md question
 * 143, docs/sil-plan.md M23). Registers `psp_lockstep_external_sync`
 * (`services/cfs/psp-lockstep`) as the OSAL timebase's `OS_TimerSync_t` in place of NULL --
 * which is the one substantive difference from SCH_LAB's own `OS_TimeBaseCreate` call -- so the
 * timebase's helper thread blocks on the kernel's own ticks (delivered by
 * `services/cfs/apps/io_lockstep` calling `psp_lockstep_release_tick`) instead of arming a real
 * POSIX interval timer against the wall clock. Everything downstream of that (a schedule table
 * driving other apps' wakeups) is unmodified cFS practice; this app changes only *where the tick
 * comes from*, per question 143's decision ("cFS otherwise unmodified").
 *
 * M23.4: the schedule table itself -- see `fsw/src/sch_lockstep_app.c`'s own top comment and
 * `services/cfs/apps/shared/mission/lockstep_wakeup_mid.h` for the one wakeup MsgId this app's
 * tick callback now publishes, and `services/cfs/apps/adcs` for its one subscriber today.
 */
#ifndef AV_CFS_SCH_LOCKSTEP_APP_H
#define AV_CFS_SCH_LOCKSTEP_APP_H

#ifdef __cplusplus
extern "C" {
#endif

void SCH_LS_AppMain(void);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_SCH_LOCKSTEP_APP_H */
