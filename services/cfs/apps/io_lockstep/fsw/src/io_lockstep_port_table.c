/* See io_lockstep_port_table.h for the module doc comment.
 *
 * Port names here (`imu_in`, `startracker_in`, `wheel_torque_out`) match
 * `drms/demo_attitude_control_controller.system.yaml`'s/`_cfs.system.yaml`'s own port names
 * (M23.4 reconciliation -- the cFS-bound `SystemInstance` these fixtures declare) and the
 * codecs (apid, field layout) are copied field-for-field from the real, already-landed M22.4
 * fixtures.
 *
 * `msg_id` -- M23.4 FIX (found only by actually publishing a real command-type message and
 * watching `io_lockstep`'s own subscribed pipe never receive it, not by static reading alone):
 * this used to be `msg_id == apid` unconditionally ("this platform's convention is msg_id ==
 * apid" -- true, but only for a TELEMETRY packet). `third_party/cfs/cfe/modules/msg/fsw/src/
 * cfe_msg_msgid_v1.c`'s own `CFE_MSG_GetMsgId` reads the FULL 16-bit `StreamId[0..1]` as the
 * MsgId (`(StreamId[0] << 8) + StreamId[1]`), not just the low 11 APID bits -- so a COMMAND-type
 * CCSDS primary header (the "type" bit, bit 4 of byte 0, set for `is_command = true`) produces a
 * MsgId with bit 0x1000 ALSO set, different from the bare APID. `CFE_SB_ValueToMsgId(300)` (a
 * bare integer, no header bits at all) is NOT the same MsgId `CFE_MSG_GetMsgId` derives from a
 * REAL command packet whose primary header has APID 300 and the type bit set -- confirmed
 * directly: a real `docker run` showed `ADCS`'s own `CFE_SB_TransmitMsg` of the wheel-torque
 * command succeeding with `msgid=0x112C` (300 | 0x1000), while this table's own subscription
 * used the bare `300`, so the two NEVER matched and `io_lockstep`'s own `CFE_SB_PEND_FOREVER`
 * receive on `wheel_torque_out` blocked forever. [`CCSDS_V1_MSGID`] below derives the correct
 * value the same way `ccsds_encode_packet`'s own primary-header encoding does (apid plus the
 * type bit iff `is_command`, secondary_header_bytes always 0 for every codec this platform
 * declares so no other StreamId bit is ever set) -- one formula, not two independently-typed
 * numbers that can drift apart the way `apid`/bare `msg_id` just did.
 */
#include "io_lockstep_port_table.h"

#include <stddef.h>

/* cFE's own "V1" MsgId scheme (see this file's own top comment): the full StreamId, which for
 * a secondary_header_bytes=0 packet is just the APID with the command "type" bit (0x1000)
 * folded in when is_command is true. Matches `ccsds_encode_packet`'s own primary-header write
 * exactly (services/cfs/apps/shared/ccsds/src/ccsds_codec.c). */
#define CCSDS_V1_MSGID(apid, is_command) ((uint16_t)((apid) | ((is_command) ? 0x1000u : 0u)))

static const ccsds_field_t g_imu_fields[] = {
    {"wx", 0, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"wy", 64, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"wz", 128, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"ax", 192, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"ay", 256, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"az", 320, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
};

static const ccsds_field_t g_startracker_fields[] = {
    {"qx", 0, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"qy", 64, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"qz", 128, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"qw", 192, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
};

static const ccsds_field_t g_wheel_torque_fields[] = {
    {"tau_1", 0, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"tau_2", 64, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
    {"tau_3", 128, 64, CCSDS_FIELD_FLOAT64, 1.0, 0.0},
};

static const io_lockstep_port_entry_t g_port_table[] = {
    {
        .port_name = "imu_in",
        .direction = LOCKSTEP_PORT_TO_BUS,
        .msg_id = CCSDS_V1_MSGID(201, false),
        .codec = {.id = "imu_meas_codec", .apid = 201, .is_command = false, .secondary_header_bytes = 0, .user_data_bytes = 48, .fields = g_imu_fields, .field_count = 6},
    },
    {
        .port_name = "startracker_in",
        .direction = LOCKSTEP_PORT_TO_BUS,
        .msg_id = CCSDS_V1_MSGID(200, false),
        .codec = {.id = "star_meas_codec", .apid = 200, .is_command = false, .secondary_header_bytes = 0, .user_data_bytes = 32, .fields = g_startracker_fields, .field_count = 4},
    },
    {
        .port_name = "wheel_torque_out",
        .direction = LOCKSTEP_PORT_FROM_BUS,
        .msg_id = CCSDS_V1_MSGID(300, true),
        .codec = {.id = "wheel_torque_cmd_codec", .apid = 300, .is_command = true, .secondary_header_bytes = 0, .user_data_bytes = 24, .fields = g_wheel_torque_fields, .field_count = 3},
    },
};

const io_lockstep_port_entry_t *io_lockstep_port_table(size_t *count_out)
{
    *count_out = sizeof(g_port_table) / sizeof(g_port_table[0]);
    return g_port_table;
}
