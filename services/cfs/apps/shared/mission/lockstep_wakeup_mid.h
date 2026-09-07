/* M23.4 reconciliation: one mission-wide software-bus MsgId for the lockstep scheduler app's
 * (`services/cfs/apps/sch_lockstep`) own tick-driven wakeup command, shared by its one
 * publisher (`sch_lockstep_app.c`'s own tick callback) and its one subscriber today
 * (`services/cfs/apps/adcs/fsw/inc/adcs_app.h`'s `ADCS_WAKEUP_MID`).
 *
 * Why this exists: `sch_lockstep_app.c`'s own M23.2-era doc comment named exactly this gap --
 * "A real deployment's schedule table dispatch (sending each due slot's wakeup command onto the
 * software bus, as SCH_LAB's own callback does) belongs here... is the reference ADCS app's own
 * concern (M23.3)... and is not fabricated here" -- and M23.3's `adcs_app.h` independently
 * declared its own placeholder `ADCS_WAKEUP_MID`, with a doc comment saying "whoever lands the
 * I/O app's mission message ID table (M23.4) must either adopt these or this file must be
 * updated to match." Neither side had the other's number. This header is the one place that
 * number now lives, so `sch_lockstep` (the publisher) and `adcs` (the subscriber) cannot drift
 * back apart the way the two independent CCSDS codecs did (see
 * services/cfs/apps/shared/ccsds/inc/ccsds_codec.h's own doc comment for that story).
 *
 * A future app with a real per-slot schedule table (see sch_lockstep_app.c's own doc comment)
 * would replace this single fixed MsgId with a proper table-driven dispatch; today there is
 * exactly one flight app to wake (ADCS), so one fixed MsgId is the whole schedule.
 *
 * Value: kept as `sch_lockstep_app.c`/`adcs_app.h`'s own pre-existing placeholder
 * (`ADCS_WAKEUP_MID == 0x1A02` before this task) -- unchanged, not renumbered, since no other
 * MsgId in this mission's numbering (`io_lockstep_port_table.c`'s own `msg_id == apid`
 * convention: 200/201/300) collides with it.
 */
#ifndef AV_CFS_SHARED_LOCKSTEP_WAKEUP_MID_H
#define AV_CFS_SHARED_LOCKSTEP_WAKEUP_MID_H

#define LOCKSTEP_WAKEUP_MID 0x1A02

#endif /* AV_CFS_SHARED_LOCKSTEP_WAKEUP_MID_H */
