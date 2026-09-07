/*
 * adcs_app.c -- reference cFS ADCS application (M23.3). See adcs_app.h's own top comment for
 * the explicit statement that this file has not been compiled against real cFE headers (the
 * pinned cFS tree is M23.2's scope and had not landed as of this writing).
 *
 * All correctness-critical logic (the control law, the CCSDS packet codec) lives in
 * adcs_control.c/adcs_packets.c, which have no cFE dependency and are unit-tested on the host
 * today (services/cfs/apps/adcs/unit-test/). This file is deliberately thin: subscribe,
 * decode-and-cache, evaluate-and-publish, with every decode/encode failure surfaced as a
 * visible CFE_EVS event, never a silent drop (docs/open-questions.md question 149).
 */
#include "adcs_app.h"

#include <string.h>

static ADCS_AppData_t ADCS_AppData;

int32 ADCS_AppInit(void)
{
    int32 status;

    memset(&ADCS_AppData, 0, sizeof(ADCS_AppData));
    ADCS_AppData.TargetQuat = ADCS_QuatIdentity();
    ADCS_AppData.Kp = ADCS_CONTROLLER_KP_DEFAULT;
    ADCS_AppData.Kd = ADCS_CONTROLLER_KD_DEFAULT;

    /* M23.4: this file's own top comment used to say it had never been compiled against real
     * cFE headers -- it hadn't, and this was the first defect that surfaced once it was.
     * `CFE_ES_RegisterApp()` does not exist in the pinned cFE 7.0.1 core API
     * (third_party/cfs/cfe/modules/core_api/fsw/inc/cfe_es.h has no such declaration --
     * confirmed by `docker build` failing with `implicit declaration of function
     * 'CFE_ES_RegisterApp'`, not by static reading alone): cFE 7's ES registers an app's AppID
     * automatically when it starts the app's task from `cfe_es_startup.scr`, before AppMain
     * ever runs, so there is nothing left for the app itself to call here. Removed rather than
     * papered over with a compatibility shim.
     */
    status = CFE_EVS_Register(NULL, 0, CFE_EVS_EventFilter_BINARY);
    if (status != CFE_SUCCESS) {
        return status;
    }

    status = CFE_SB_CreatePipe(&ADCS_AppData.CmdPipe, ADCS_PIPE_DEPTH, ADCS_APP_NAME ".CMD_PIPE");
    if (status != CFE_SUCCESS) {
        CFE_EVS_SendEvent(1, CFE_EVS_EventType_ERROR, "%s: CFE_SB_CreatePipe failed: 0x%08X", ADCS_APP_NAME, (unsigned int)status);
        return status;
    }

    status = CFE_SB_Subscribe(CFE_SB_ValueToMsgId(ADCS_STARTRACKER_IN_MID), ADCS_AppData.CmdPipe);
    if (status != CFE_SUCCESS) {
        CFE_EVS_SendEvent(2, CFE_EVS_EventType_ERROR, "%s: subscribe to star tracker MID failed: 0x%08X", ADCS_APP_NAME, (unsigned int)status);
        return status;
    }
    status = CFE_SB_Subscribe(CFE_SB_ValueToMsgId(ADCS_IMU_IN_MID), ADCS_AppData.CmdPipe);
    if (status != CFE_SUCCESS) {
        CFE_EVS_SendEvent(3, CFE_EVS_EventType_ERROR, "%s: subscribe to IMU MID failed: 0x%08X", ADCS_APP_NAME, (unsigned int)status);
        return status;
    }
    status = CFE_SB_Subscribe(CFE_SB_ValueToMsgId(ADCS_WAKEUP_MID), ADCS_AppData.CmdPipe);
    if (status != CFE_SUCCESS) {
        CFE_EVS_SendEvent(4, CFE_EVS_EventType_ERROR, "%s: subscribe to wakeup MID failed: 0x%08X", ADCS_APP_NAME, (unsigned int)status);
        return status;
    }

    /* M23.4 FIX (adcs_app.h's own top comment has the full account): no `CFE_MSG_Init` call
     * here -- `ADCS_WheelTorqueOutMsg_t` no longer has a separate envelope field to initialize
     * once at startup. `ADCS_RunControlLawAndPublish`'s own `ADCS_EncodeWheelTorquePacket` call
     * writes a complete, freshly-sequenced CCSDS primary header into `WheelTorqueOutMsg.Packet`
     * on every publish, which doubles as this message's `CFE_MSG_Message_t` (matching
     * `io_lockstep`'s own "primary header at byte 0, no secondary header" convention exactly).
     */

    CFE_EVS_SendEvent(0, CFE_EVS_EventType_INFORMATION, "%s: initialized, kp=%f kd=%f", ADCS_APP_NAME, ADCS_AppData.Kp, ADCS_AppData.Kd);
    return CFE_SUCCESS;
}

void ADCS_ProcessStarTrackerMsg(ADCS_AppData_t *app, const ADCS_StarTrackerInMsg_t *msg)
{
    ADCS_StarTrackerMeas_t meas;
    uint16 seq = 0;
    ADCS_CodecStatus_t st = ADCS_DecodeStarTrackerPacket(msg->Packet, sizeof(msg->Packet), &meas, &seq);
    if (st != ADCS_CODEC_OK) {
        /* Typed, visible failure -- never a dropped or zero-filled reading (question 149). The
         * previously cached measurement (if any) is left exactly as it was. */
        CFE_EVS_SendEvent(10, CFE_EVS_EventType_ERROR, "%s: malformed star tracker packet: %s", ADCS_APP_NAME, ADCS_CodecStatusName(st));
        return;
    }
    app->LastMeasuredQuat.x = meas.qx;
    app->LastMeasuredQuat.y = meas.qy;
    app->LastMeasuredQuat.z = meas.qz;
    app->LastMeasuredQuat.w = meas.qw;
    if (!app->HasStarTrackerMeas) {
        /* One-shot (this event id is never re-sent after the first success): visible
         * confirmation that a real, decoded star tracker measurement reached this app at least
         * once -- useful for diagnosing "the loop never closes" reports without needing a
         * per-step log. */
        CFE_EVS_SendEvent(14, CFE_EVS_EventType_INFORMATION, "%s: first star tracker measurement received (seq=%u)", ADCS_APP_NAME, (unsigned int)seq);
    }
    app->HasStarTrackerMeas = true;
}

void ADCS_ProcessImuMsg(ADCS_AppData_t *app, const ADCS_ImuInMsg_t *msg)
{
    ADCS_ImuMeas_t meas;
    uint16 seq = 0;
    ADCS_CodecStatus_t st = ADCS_DecodeImuPacket(msg->Packet, sizeof(msg->Packet), &meas, &seq);
    if (st != ADCS_CODEC_OK) {
        CFE_EVS_SendEvent(11, CFE_EVS_EventType_ERROR, "%s: malformed IMU packet: %s", ADCS_APP_NAME, ADCS_CodecStatusName(st));
        return;
    }
    app->LastMeasuredOmega.x = meas.wx;
    app->LastMeasuredOmega.y = meas.wy;
    app->LastMeasuredOmega.z = meas.wz;
    if (!app->HasImuMeas) {
        CFE_EVS_SendEvent(15, CFE_EVS_EventType_INFORMATION, "%s: first IMU measurement received (seq=%u)", ADCS_APP_NAME, (unsigned int)seq);
    }
    app->HasImuMeas = true;
}

void ADCS_RunControlLawAndPublish(ADCS_AppData_t *app)
{
    static bool s_logged_first_wakeup = false;
    if (!s_logged_first_wakeup) {
        s_logged_first_wakeup = true;
        CFE_EVS_SendEvent(16, CFE_EVS_EventType_INFORMATION, "%s: first wakeup received", ADCS_APP_NAME);
    }
    if (!app->HasStarTrackerMeas || !app->HasImuMeas) {
        /* Nothing to control on yet -- mirrors
         * crate::drm::controller::AttitudeControllerModel's own "emits nothing before the
         * first measurement arrives" rule, never a default/zero command in the meantime. */
        return;
    }

    static bool s_logged_first_publish = false;
    if (!s_logged_first_publish) {
        s_logged_first_publish = true;
        CFE_EVS_SendEvent(17, CFE_EVS_EventType_INFORMATION, "%s: first wheel-torque command published", ADCS_APP_NAME);
    }

    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(app->Kp, app->Kd, app->TargetQuat, app->LastMeasuredQuat, app->LastMeasuredOmega);

    ADCS_WheelTorqueCmd_t cmd = {tau.x, tau.y, tau.z};
    size_t out_len = 0;
    ADCS_CodecStatus_t st = ADCS_EncodeWheelTorquePacket(app->CommandSequenceCount, &cmd, app->WheelTorqueOutMsg.Packet, sizeof(app->WheelTorqueOutMsg.Packet), &out_len);
    if (st != ADCS_CODEC_OK) {
        /* Every declared field/gain is finite and in-range by construction here (this is an
         * encode of our own just-computed torque, not an inbound packet), so reaching this
         * branch is a programming error, not an expected runtime condition -- still a visible
         * event, never a silent drop of the command. */
        CFE_EVS_SendEvent(12, CFE_EVS_EventType_ERROR, "%s: failed to encode wheel-torque command: %s", ADCS_APP_NAME, ADCS_CodecStatusName(st));
        return;
    }
    app->CommandSequenceCount = (uint16)((app->CommandSequenceCount + 1u) & ADCS_CCSDS_MAX_SEQUENCE_COUNT);

    /* M23.4 FIX: `Packet` is already a complete, self-contained CCSDS packet (its own first 6
     * bytes are a valid CFE_MSG_Message_t/CCSDS primary header, written by
     * ADCS_EncodeWheelTorquePacket above, APID | 0x1000 for this is_command=true codec --
     * matching io_lockstep_port_table.c's own CCSDS_V1_MSGID subscription) -- transmitted
     * directly, reinterpreted, with no separate envelope (adcs_app.h's own top comment has the
     * full account of why there is no separate header field to initialize here). */
    CFE_SB_TransmitMsg((CFE_MSG_Message_t *)app->WheelTorqueOutMsg.Packet, true);
}

void ADCS_AppMain(void)
{
    int32 status;
    CFE_SB_Buffer_t *bufPtr;

    status = ADCS_AppInit();
    if (status != CFE_SUCCESS) {
        CFE_ES_WriteToSysLog("%s: init failed: 0x%08X\n", ADCS_APP_NAME, (unsigned int)status);
        return;
    }

    CFE_ES_PerfLogEntry(0);

    while (CFE_ES_RunLoop(NULL) == true) {
        CFE_ES_PerfLogExit(0);
        status = CFE_SB_ReceiveBuffer(&bufPtr, ADCS_AppData.CmdPipe, CFE_SB_PEND_FOREVER);
        CFE_ES_PerfLogEntry(0);

        if (status != CFE_SUCCESS) {
            continue;
        }

        CFE_SB_MsgId_t msgId;
        CFE_MSG_GetMsgId(&bufPtr->Msg, &msgId);

        if (CFE_SB_MsgId_Equal(msgId, CFE_SB_ValueToMsgId(ADCS_STARTRACKER_IN_MID))) {
            ADCS_ProcessStarTrackerMsg(&ADCS_AppData, (const ADCS_StarTrackerInMsg_t *)bufPtr);
        } else if (CFE_SB_MsgId_Equal(msgId, CFE_SB_ValueToMsgId(ADCS_IMU_IN_MID))) {
            ADCS_ProcessImuMsg(&ADCS_AppData, (const ADCS_ImuInMsg_t *)bufPtr);
        } else if (CFE_SB_MsgId_Equal(msgId, CFE_SB_ValueToMsgId(ADCS_WAKEUP_MID))) {
            ADCS_RunControlLawAndPublish(&ADCS_AppData);
        } else {
            CFE_EVS_SendEvent(20, CFE_EVS_EventType_ERROR, "%s: unexpected MsgId 0x%04X", ADCS_APP_NAME, (unsigned int)CFE_SB_MsgIdToValue(msgId));
        }
    }

    CFE_ES_PerfLogExit(0);
    CFE_ES_ExitApp(CFE_ES_RunStatus_APP_EXIT);
}
