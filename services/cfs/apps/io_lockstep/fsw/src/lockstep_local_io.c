/* See lockstep_local_io.h for the module doc comment.
 *
 * No wall-clock read, sleep, or bounded-retry-then-give-up polling loop appears here: a short
 * read/write is retried unconditionally until it either completes or the syscall itself fails
 * or the peer closes -- this module never "waits a while and then decides" to drop a frame.
 */
#include "lockstep_local_io.h"

#include <errno.h>
#include <unistd.h>

static lockstep_io_status_t write_all(int fd, const uint8_t *buf, size_t len)
{
    size_t written = 0;
    while (written < len)
    {
        ssize_t n = write(fd, buf + written, len - written);
        if (n < 0)
        {
            if (errno == EINTR) continue;
            return LOCKSTEP_IO_ERR_SYSCALL;
        }
        written += (size_t)n;
    }
    return LOCKSTEP_IO_OK;
}

static lockstep_io_status_t read_all(int fd, uint8_t *buf, size_t len)
{
    size_t got = 0;
    while (got < len)
    {
        ssize_t n = read(fd, buf + got, len - got);
        if (n < 0)
        {
            if (errno == EINTR) continue;
            return LOCKSTEP_IO_ERR_SYSCALL;
        }
        if (n == 0) return LOCKSTEP_IO_ERR_PEER_CLOSED;
        got += (size_t)n;
    }
    return LOCKSTEP_IO_OK;
}

lockstep_io_status_t lockstep_write_frame(int fd, uint8_t frame_type, const uint8_t *payload, size_t payload_len)
{
    uint8_t length_field[4];
    if (lockstep_encode_length_field(payload_len, length_field) != LOCKSTEP_FRAMING_OK)
    {
        return LOCKSTEP_IO_ERR_FRAME_TOO_LARGE;
    }
    lockstep_io_status_t st = write_all(fd, length_field, sizeof(length_field));
    if (st != LOCKSTEP_IO_OK) return st;
    st = write_all(fd, &frame_type, 1);
    if (st != LOCKSTEP_IO_OK) return st;
    if (payload_len > 0)
    {
        st = write_all(fd, payload, payload_len);
        if (st != LOCKSTEP_IO_OK) return st;
    }
    return LOCKSTEP_IO_OK;
}

lockstep_io_status_t lockstep_read_frame(int fd, uint8_t *frame_type_out, uint8_t *payload_buf, size_t payload_buf_cap, size_t *payload_len_out)
{
    uint8_t length_field[4];
    lockstep_io_status_t st = read_all(fd, length_field, sizeof(length_field));
    if (st != LOCKSTEP_IO_OK) return st;

    uint32_t length;
    lockstep_framing_status_t fst = lockstep_decode_length_field(length_field, &length);
    if (fst == LOCKSTEP_FRAMING_ERR_ZERO_LENGTH) return LOCKSTEP_IO_ERR_ZERO_LENGTH;
    if (fst == LOCKSTEP_FRAMING_ERR_FRAME_TOO_LARGE) return LOCKSTEP_IO_ERR_FRAME_TOO_LARGE;

    uint32_t payload_len = length - 1u;
    if (payload_len > payload_buf_cap) return LOCKSTEP_IO_ERR_BUFFER_TOO_SMALL;

    uint8_t frame_type;
    st = read_all(fd, &frame_type, 1);
    if (st != LOCKSTEP_IO_OK) return st;

    if (payload_len > 0)
    {
        st = read_all(fd, payload_buf, payload_len);
        if (st != LOCKSTEP_IO_OK) return st;
    }

    *frame_type_out = frame_type;
    *payload_len_out = payload_len;
    return LOCKSTEP_IO_OK;
}
