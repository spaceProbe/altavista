/*
 * adcs_app.h -- reference cFS ADCS application (M23.3), the cFE/OSAL-dependent glue that moves
 * bytes between the cFS software bus and the standalone, host-testable adcs_control.h/
 * adcs_packets.h modules.
 *
 * M23.4 UPDATE: this file is now built for real. `services/cfs/apps/adcs/CMakeLists.txt`
 * (added in M23.4) compiles `adcs_app.c`/`adcs_control.c`/`adcs_packets.c` against the real,
 * fetched `third_party/cfs` headers as part of `services/cfs/Dockerfile`'s image build --
 * M23.3's own top comment here used to say this had never been compiled ("a file that has
 * never been through a compiler is not evidence of anything," M23.4's own brief); that gap is
 * closed. `services/cfs/apps/adcs` is now in `services/cfs/build/targets.cmake`'s
 * `cpu1_APPLIST` and `services/cfs/build/generate_startup.cmake`'s startup script.
 *
 * The control law (adcs_control.h) and the CCSDS packet codec (adcs_packets.h) that this file
 * wires together have NO cFE dependency and ARE fully unit-tested on the host today
 * (services/cfs/apps/adcs/unit-test/) -- that is deliberate: this app's correctness-critical
 * logic does not wait on the cFS tree landing, only this thin glue layer does.
 */
#ifndef ADCS_APP_H
#define ADCS_APP_H

#include "cfe.h"
#include "adcs_control.h"
#include "adcs_packets.h"
#include "lockstep_wakeup_mid.h"

#ifdef __cplusplus
extern "C" {
#endif

/** App name, as registered with CFE_ES_RegisterApp / used in event/telemetry identification. */
#define ADCS_APP_NAME "ADCS"

/* -------------------------------------------------------------------------------------------
 * Software bus message IDs -- RECONCILED (M23.4) against
 * `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_port_table.c`'s own `g_port_table`, the
 * single source of truth for which cFE `msg_id` carries which port's packets on the software
 * bus. M23.3 originally declared these as its own placeholders (`0x1A00`/`0x1A01`/`0x0A80`,
 * "whoever lands the I/O app's mission message ID table (M23.4) must either adopt these or
 * this file must be updated to match"); io_lockstep_port_table.c independently used its own
 * placeholder port names (`imu_in`/`startracker_in`/`wheel_torque_out`) with `msg_id` values
 * 201/200/300 (its own documented convention: `msg_id == apid`, matching the same
 * `drms/demo_attitude_control_*.system.yaml` `PacketCodec` apids this app's own
 * adcs_packets.h declares -- 200/201/300). Neither side had adopted the other's numbers. This
 * file now adopts io_lockstep's `msg_id` values directly (not the reverse), since `msg_id ==
 * apid` is the more legible convention and the port table names those apid/port-name pairings
 * with real doc-comment provenance (drms/demo_attitude_control_startracker.system.yaml/_imu./
 * _controller., cross-checked against the wire by services/cfs/tests/test_ccsds_golden.py).
 * ---------------------------------------------------------------------------------------- */
#ifndef ADCS_STARTRACKER_IN_MID
#define ADCS_STARTRACKER_IN_MID ADCS_STARTRACKER_APID /* 200 -- io_lockstep_port_table.c "startracker_in" */
#endif
#ifndef ADCS_IMU_IN_MID
#define ADCS_IMU_IN_MID ADCS_IMU_APID /* 201 -- io_lockstep_port_table.c "imu_in" */
#endif
/* NOT the actual on-wire cFE MsgId (see the M23.4 FIX note below) -- kept only as the bare
 * APID this port's packets declare (matching ADCS_STARTRACKER_IN_MID/ADCS_IMU_IN_MID's own
 * shape above), and unused by adcs_app.c today: ADCS never subscribes to its own output, and
 * ADCS_RunControlLawAndPublish's CFE_SB_TransmitMsg reads the MsgId straight out of the
 * primary header ADCS_EncodeWheelTorquePacket already wrote (apid | 0x1000 for this
 * is_command=true codec -- see io_lockstep_port_table.c's own CCSDS_V1_MSGID macro), never
 * this macro. M23.4 FIX (found only by actually publishing a real command message and
 * watching io_lockstep's subscribed pipe never receive it): a command-type CCSDS packet's
 * cFE "V1" MsgId is NOT the bare APID -- third_party/cfs/cfe/modules/msg/fsw/src/
 * cfe_msg_msgid_v1.c's own CFE_MSG_GetMsgId reads the full 16-bit primary-header StreamId
 * (APID plus the command "type" bit, 0x1000, for secondary_header_bytes=0), so
 * io_lockstep_port_table.c's own subscription for "wheel_torque_out" must (and, after that
 * fix, does) use apid|0x1000, not bare apid -- this macro is deliberately left as the bare
 * APID rather than "fixed" to match, since fixing it here would just move the same
 * apid-vs-msgid confusion to a constant nothing in this file actually uses for transmission. */
#ifndef ADCS_WHEEL_TORQUE_OUT_MID
#define ADCS_WHEEL_TORQUE_OUT_MID ADCS_WHEEL_TORQUE_APID /* 300 -- the bare APID, not the on-wire MsgId */
#endif
/** Wakeup/scheduling message (`services/cfs/apps/shared/mission/lockstep_wakeup_mid.h`,
 *  published by the lockstep scheduler app, `services/cfs/apps/sch_lockstep`'s own tick
 *  callback as of M23.4) that drives one control-law evaluation per receipt -- this app's
 *  control rate is therefore whatever rate the scheduler ticks it at, matching question 145's
 *  port-boundary determinism (RTEMS task order recorded, not asserted; the *rate* at which
 *  this app is scheduled is itself a configuration input, not decided by this file). */
#ifndef ADCS_WAKEUP_MID
#define ADCS_WAKEUP_MID LOCKSTEP_WAKEUP_MID
#endif

#define ADCS_PIPE_DEPTH 16

/* -------------------------------------------------------------------------------------------
 * Software bus message shapes -- M23.4 FIX (found only by actually compiling and running this
 * app against the real cFS image, not by static reading alone -- see this file's own top
 * comment's "M23.4 UPDATE"): these used to declare a full `CFE_MSG_TelemetryHeader_t`/
 * `CFE_MSG_CommandHeader_t` (6-byte primary header + a secondary header + padding, 12-16 bytes
 * total in this cFE 7.0.1 build's own default message-header layout,
 * third_party/cfs/cfe/modules/msg/option_inc/default_cfe_msg_hdr_pri.h) BEFORE `Packet`, on the
 * assumption that cFE inserts that full envelope ahead of every software-bus payload. It does
 * not, here: `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`'s own `handle_step`
 * transmits `req.inputs[i].payload` (a FRAMED port's raw CCSDS Space Packet -- primary header
 * ONLY, `secondary_header_bytes: 0` on every declared `PacketCodec` this platform uses) AS THE
 * ENTIRE cFE_SB message, starting at byte 0 -- its own doc comment says so plainly ("the packet
 * bytes this app puts on the bus are req.inputs[i].payload verbatim"). A real `docker run`
 * against this exact build surfaced the mismatch immediately: `ADCS: malformed star tracker
 * packet: WRONG_APID` (this app reading `Packet` starting `sizeof(CFE_MSG_TelemetryHeader_t)`
 * bytes too late, into what is actually padding past the real packet), which then meant
 * `HasStarTrackerMeas`/`HasImuMeas` never became true, `ADCS_RunControlLawAndPublish` never
 * published anything, and `io_lockstep`'s own `CFE_SB_PEND_FOREVER` receive on
 * `wheel_torque_out` blocked forever -- a real deadlock, not a slow step.
 *
 * The fix: `CFE_MSG_Message_t` (the BARE base header -- `third_party/cfs/cfe/modules/msg/
 * option_inc/default_cfe_msg_hdr_pri.h`'s own `struct CFE_MSG_Message { CCSDS_SpacePacket_t
 * CCSDS; }`, exactly 6 bytes, no secondary header) is EXACTLY `io_lockstep`'s own "primary
 * header, byte 0" convention -- so `Packet` itself (already a complete, self-contained CCSDS
 * packet: `ADCS_EncodePacket`/`ccsds_encode_packet` always write a full primary header +
 * user data into it) doubles as this software-bus message's own `CFE_MSG_Message_t`, with no
 * separate envelope field at all. `ADCS_ProcessStarTrackerMsg`/`ADCS_ProcessImuMsg` read
 * `Packet` starting at byte 0 of the received `CFE_SB_Buffer_t` (itself a union with `Msg` at
 * offset 0, `third_party/cfs/cfe/modules/core_api/fsw/inc/cfe_sb_api_typedefs.h`'s own
 * `CFE_SB_Buffer_t`) -- exactly what `io_lockstep` wrote. `ADCS_RunControlLawAndPublish`
 * transmits `Packet` reinterpreted as `CFE_MSG_Message_t *` the same way, needing no separate
 * `CFE_MSG_Init` call at all (each encode already writes a correct, freshly-sequenced primary
 * header) -- see `adcs_app.c`'s own updated `ADCS_AppInit`/`ADCS_RunControlLawAndPublish`.
 * ---------------------------------------------------------------------------------------- */
typedef struct {
    uint8 Packet[ADCS_STARTRACKER_PACKET_LEN];
} ADCS_StarTrackerInMsg_t;

typedef struct {
    uint8 Packet[ADCS_IMU_PACKET_LEN];
} ADCS_ImuInMsg_t;

typedef struct {
    uint8 Packet[ADCS_WHEEL_TORQUE_PACKET_LEN];
} ADCS_WheelTorqueOutMsg_t;

/* -------------------------------------------------------------------------------------------
 * App global state.
 * ---------------------------------------------------------------------------------------- */
typedef struct {
    CFE_SB_PipeId_t CmdPipe;

    bool HasStarTrackerMeas;
    ADCS_Quat_t LastMeasuredQuat;

    bool HasImuMeas;
    ADCS_Vec3_t LastMeasuredOmega;

    uint16 CommandSequenceCount; /* 14-bit CCSDS sequence count, wraps mod 16384 */

    ADCS_Quat_t TargetQuat;
    double Kp;
    double Kd;

    ADCS_WheelTorqueOutMsg_t WheelTorqueOutMsg;
} ADCS_AppData_t;

/** cFE app entry point (registered as this app's main task). */
void ADCS_AppMain(void);

/** One-time app initialization: register with ES/EVS, create the SB pipe, subscribe to the
 *  star tracker/IMU/wakeup MIDs, initialize the wheel-torque output message. */
int32 ADCS_AppInit(void);

/** Decode one inbound star tracker/IMU software-bus message and cache the measurement --
 *  a decode failure ([`ADCS_CodecStatus_t`] != OK) is sent as a CFE_EVS event and the cached
 *  measurement is left unchanged (never zero-filled or silently substituted -- question 149).
 */
void ADCS_ProcessStarTrackerMsg(ADCS_AppData_t *app, const ADCS_StarTrackerInMsg_t *msg);
void ADCS_ProcessImuMsg(ADCS_AppData_t *app, const ADCS_ImuInMsg_t *msg);

/** Runs one control-law evaluation (ADCS_ComputeWheelTorque) against the app's own currently
 *  cached measurements and publishes the resulting wheel-torque command -- a no-op (no publish)
 *  until at least one star tracker AND one IMU measurement have arrived, mirroring
 *  crate::drm::controller::AttitudeControllerModel's own "nothing to control on yet" rule. */
void ADCS_RunControlLawAndPublish(ADCS_AppData_t *app);

#ifdef __cplusplus
}
#endif

#endif /* ADCS_APP_H */
