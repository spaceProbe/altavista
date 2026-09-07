/* The lockstep I/O app: M23.2's replacement for CI_LAB and TO_LAB (docs/sil-plan.md M23). One
 * app, not two, because both of CI_LAB/TO_LAB's jobs (packets in from the ground/kernel, packets
 * out to the ground/kernel) collapse onto the same single per-tick round trip a lockstep STEP
 * frame already is: `LockstepStepRequest.inputs` are the packets to place onto the software bus
 * before the tick runs, and `LockstepStepResponse.outputs` are the packets to collect off the
 * bus once it has. Splitting those into two independently-scheduled apps would reintroduce the
 * exact ordering ambiguity a single app avoids by construction (see io_lockstep_app.c's own
 * module doc comment for the full sequencing argument).
 *
 * This app owns the Unix domain socket connection to `crates/av-lockstep-shim` (the shim
 * listens; this app connects -- services/cfs/README.md's own "Who listens, who connects, who
 * speaks first" section) and drives `services/cfs/psp-lockstep`'s clock via
 * `psp_lockstep_release_tick` once per STEP frame, which is what makes cFS's own scheduler tick
 * (through `services/cfs/apps/sch_lockstep`'s registered `OS_TimerSync_t`) follow the kernel's
 * ticks and nothing else.
 */
#ifndef AV_CFS_IO_LOCKSTEP_APP_H
#define AV_CFS_IO_LOCKSTEP_APP_H

#ifdef __cplusplus
extern "C" {
#endif

void IO_LOCKSTEP_AppMain(void);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_IO_LOCKSTEP_APP_H */
