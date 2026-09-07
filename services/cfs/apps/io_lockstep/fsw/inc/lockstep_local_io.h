/* Blocking read/write of a whole "lockstep-local v1" frame over a connected stream socket (a
 * Unix domain socket in the real deployment; the host tests use `socketpair(AF_UNIX, ...)`
 * directly, which is the same kernel object type with no network stack involved). Built on
 * `lockstep_local_framing.h`'s pure encode/decode functions; this module owns the actual
 * `read`/`write` syscalls -- see this .c file's own top comment for the "never a wall clock, a
 * poll, or a retry loop that hides a dropped frame" rule this module follows.
 */
#ifndef AV_CFS_LOCKSTEP_LOCAL_IO_H
#define AV_CFS_LOCKSTEP_LOCAL_IO_H

#include <stddef.h>
#include <stdint.h>

#include "lockstep_local_framing.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum
{
    LOCKSTEP_IO_OK = 0,
    LOCKSTEP_IO_ERR_SYSCALL,      /* read()/write() failed; errno is left set by the failing call */
    LOCKSTEP_IO_ERR_PEER_CLOSED,  /* read() returned 0 mid-frame -- a typed error, never treated as EOF-is-fine */
    LOCKSTEP_IO_ERR_FRAME_TOO_LARGE,
    LOCKSTEP_IO_ERR_ZERO_LENGTH,
    LOCKSTEP_IO_ERR_BUFFER_TOO_SMALL, /* caller's payload buffer is smaller than the incoming payload */
} lockstep_io_status_t;

/* Writes one whole frame (`length` field, `frame_type`, then `payload`) to `fd`, retrying only
 * on a short `write()` (a normal, non-error condition for a stream socket) -- never silently
 * dropping the remainder. */
lockstep_io_status_t lockstep_write_frame(int fd, uint8_t frame_type, const uint8_t *payload, size_t payload_len);

/* Reads one whole frame from `fd` into `payload_buf` (capacity `payload_buf_cap`). A short
 * `read()` is retried (looping until the requested byte count has actually arrived, exactly
 * like `crates/av-lockstep-shim/src/framing.rs::read_frame` and
 * `tests/lockstep_local_peer.py::recv_exact` on the other two implementations of this same
 * protocol) -- this function never returns a partial frame. `read()` returning 0 (peer closed)
 * at any point is `LOCKSTEP_IO_ERR_PEER_CLOSED`, never treated as "frame complete". */
lockstep_io_status_t lockstep_read_frame(int fd, uint8_t *frame_type_out, uint8_t *payload_buf, size_t payload_buf_cap, size_t *payload_len_out);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_LOCKSTEP_LOCAL_IO_H */
