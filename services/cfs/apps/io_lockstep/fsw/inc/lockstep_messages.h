/* Encode/decode for the `altavista.v1` messages "lockstep-local v1" carries as its non-HELLO,
 * non-ERROR payloads (services/cfs/README.md's frame-type table) -- built on pbmini.h's generic
 * wire-format primitives, one function per message, matching field numbers directly from
 * proto/altavista/v1/lockstep.proto (read-only for this task; read, never generated from, here).
 *
 * Scope note: `LockstepBindRequest.ports` (repeated `Port`) and `.parameters` (a map) are
 * skipped on decode (`pbmini_skip_field`), not parsed field-by-field -- the lockstep I/O app's
 * own port set is configured statically per deployment (`io_lockstep_port_table.c`), not learned
 * from the wire, so validating the kernel's declared port list against it is a documented gap,
 * not a silent one. `LockstepStepResponse.named_outputs` (a map) is never emitted -- this app
 * has no named scalar outputs, only framed packets in `outputs`.
 */
#ifndef AV_CFS_LOCKSTEP_MESSAGES_H
#define AV_CFS_LOCKSTEP_MESSAGES_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "pbmini.h"

#ifdef __cplusplus
extern "C" {
#endif

#define LOCKSTEP_MSG_MAX_STRING 128u
#define LOCKSTEP_MSG_MAX_PORT_MESSAGES 16u
#define LOCKSTEP_MSG_MAX_PAYLOAD 512u

typedef struct
{
    char port[LOCKSTEP_MSG_MAX_STRING];
    int64_t tai_ns;
    uint8_t payload[LOCKSTEP_MSG_MAX_PAYLOAD];
    size_t payload_len;
} lockstep_port_message_t;

typedef struct
{
    char run_id[LOCKSTEP_MSG_MAX_STRING];
    char instance[LOCKSTEP_MSG_MAX_STRING];
    int64_t start_tai_ns;
    int64_t base_period_ns;
    int64_t step_period_ns;
    uint64_t seed;
} lockstep_bind_request_t;

typedef struct
{
    uint64_t sequence;
    int64_t until_tai_ns;
    lockstep_port_message_t inputs[LOCKSTEP_MSG_MAX_PORT_MESSAGES];
    size_t input_count;
} lockstep_step_request_t;

typedef struct
{
    uint64_t sequence;
    int64_t tai_ns;
    char reason[LOCKSTEP_MSG_MAX_STRING];
} lockstep_reset_request_t;

typedef struct
{
    char run_id[LOCKSTEP_MSG_MAX_STRING];
} lockstep_shutdown_request_t;

/* Decode. All return pbmini_status_t; PBMINI_OK on success. A string/bytes field longer than
 * its fixed buffer is PBMINI_ERR_BUFFER_TOO_SMALL (never silently truncated); more than
 * LOCKSTEP_MSG_MAX_PORT_MESSAGES inputs is the same. */
pbmini_status_t lockstep_decode_bind_request(const uint8_t *data, size_t len, lockstep_bind_request_t *out);
pbmini_status_t lockstep_decode_step_request(const uint8_t *data, size_t len, lockstep_step_request_t *out);
pbmini_status_t lockstep_decode_reset_request(const uint8_t *data, size_t len, lockstep_reset_request_t *out);
pbmini_status_t lockstep_decode_shutdown_request(const uint8_t *data, size_t len, lockstep_shutdown_request_t *out);

/* Encode. `out_cap` is the caller's buffer size; `*out_len` is set on success. */
pbmini_status_t lockstep_encode_bind_response(bool lockstep_capable, const char *binding_hash, const char *version, const char *refusal_reason, uint8_t *out, size_t out_cap, size_t *out_len);
pbmini_status_t lockstep_encode_step_response(uint64_t sequence, int64_t reached_tai_ns, const lockstep_port_message_t *outputs, size_t output_count, uint8_t *out, size_t out_cap, size_t *out_len);
pbmini_status_t lockstep_encode_reset_response(uint64_t sequence, uint8_t *out, size_t out_cap, size_t *out_len);
pbmini_status_t lockstep_encode_shutdown_response(uint8_t *out, size_t out_cap, size_t *out_len);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_LOCKSTEP_MESSAGES_H */
