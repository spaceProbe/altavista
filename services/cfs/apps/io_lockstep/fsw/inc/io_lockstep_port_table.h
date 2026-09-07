/* Static per-deployment configuration for the lockstep I/O app: which lockstep-local port name
 * (`PortMessage.port`) maps to which CCSDS `PacketCodec` (APID, field layout) and which cFE
 * software-bus `MsgId`. This is NOT learned from the wire -- `LockstepBindRequest.ports` is
 * skipped on decode (`lockstep_messages.h`'s own doc comment) -- it is compiled in per
 * deployment, the same way a real cFS mission's `cfe_es_startup.scr` and message-id table are
 * static per build, not negotiated at runtime.
 *
 * The table below matches the M22.4 demo's own real `PacketCodec`s
 * (drms/demo_attitude_control_imu.system.yaml, drms/demo_attitude_control_startracker.system.yaml,
 * drms/demo_attitude_control_controller.system.yaml) exactly -- apid/field layout copied
 * field-for-field, not re-derived -- so a cFS app bound to this table speaks the identical wire
 * format the kernel's own `av_kernel::codec` produces for that system, per M23.2's "the packets
 * on the software bus must be the same bytes the kernel encodes" requirement.
 *
 * `direction` says which way this app moves that port's packets: `LOCKSTEP_PORT_TO_BUS` means a
 * `STEP.inputs` `PortMessage` with this name is decoded and transmitted onto the cFE software
 * bus (a sensor measurement arriving from the kernel); `LOCKSTEP_PORT_FROM_BUS` means this
 * app subscribes to `msg_id` on the bus and, whenever a message with that id is received during
 * a step, encodes it as this port's outgoing `PortMessage` in the `STEP_DONE` response (an
 * actuator command the reference ADCS app produced).
 */
#ifndef AV_CFS_IO_LOCKSTEP_PORT_TABLE_H
#define AV_CFS_IO_LOCKSTEP_PORT_TABLE_H

#include "ccsds_codec.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum
{
    LOCKSTEP_PORT_TO_BUS = 1,
    LOCKSTEP_PORT_FROM_BUS = 2,
} lockstep_port_direction_t;

typedef struct
{
    const char *port_name; /* matches PortMessage.port / SystemDefinition.ports[].name */
    lockstep_port_direction_t direction;
    uint16_t msg_id; /* CFE_SB_MsgId_t value this port's packets are transmitted/received under
                         on the cFE software bus -- this platform's convention is msg_id == apid
                         (cFE's own "V1" message-id scheme is APID-shaped, see io_lockstep_app.c's
                         own module doc comment) */
    ccsds_codec_t codec;
} io_lockstep_port_entry_t;

/* Returns the static port table and its length. Owned by this translation unit (a real
 * deployment would generate or hand-edit `io_lockstep_port_table.c` per SystemDefinition; this
 * batch ships the M22.4 demo's own three FRAMED ports as the reference configuration). */
const io_lockstep_port_entry_t *io_lockstep_port_table(size_t *count_out);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_IO_LOCKSTEP_PORT_TABLE_H */
