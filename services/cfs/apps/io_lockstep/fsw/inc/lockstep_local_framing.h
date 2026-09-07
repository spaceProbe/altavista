/* "lockstep-local v1" frame layer (docs/open-questions.md question 153;
 * `services/cfs/README.md` is the byte-exact specification this implements -- read it first).
 * This is the flight-software side of the protocol `crates/av-lockstep-shim` already speaks;
 * this module is a from-scratch, independent implementation (not a port of any Rust source) so
 * that a bug shared between the two sides cannot hide -- the two implementations are checked
 * against the README's own worked example and against each other only through the wire bytes,
 * never by sharing code.
 *
 * Every multi-byte integer is little-endian (`services/cfs/README.md`'s own "Endianness"
 * section explains why, deliberately, unlike this platform's CCSDS framing elsewhere).
 */
#ifndef AV_CFS_LOCKSTEP_LOCAL_FRAMING_H
#define AV_CFS_LOCKSTEP_LOCAL_FRAMING_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LOCKSTEP_LOCAL_MAX_FRAME_LEN (16u * 1024u * 1024u) /* matches av-lockstep-shim's own MAX_FRAME_LEN */

typedef enum
{
    LOCKSTEP_FRAME_HELLO = 0x01,
    LOCKSTEP_FRAME_BIND = 0x02,
    LOCKSTEP_FRAME_BIND_ACK = 0x03,
    LOCKSTEP_FRAME_STEP = 0x04,
    LOCKSTEP_FRAME_STEP_DONE = 0x05,
    LOCKSTEP_FRAME_RESET = 0x06,
    LOCKSTEP_FRAME_RESET_ACK = 0x07,
    LOCKSTEP_FRAME_SHUTDOWN = 0x08,
    LOCKSTEP_FRAME_SHUTDOWN_ACK = 0x09,
    LOCKSTEP_FRAME_ERROR = 0xFF,
} lockstep_frame_type_t;

typedef enum
{
    LOCKSTEP_ERROR_VERSION_MISMATCH = 1,
    LOCKSTEP_ERROR_SEQUENCE_MISMATCH = 2,
    LOCKSTEP_ERROR_STEP_ALREADY_OUTSTANDING = 3,
    LOCKSTEP_ERROR_REACHED_MISMATCH = 4,
    LOCKSTEP_ERROR_UNEXPECTED_FRAME_TYPE = 5,
    LOCKSTEP_ERROR_MALFORMED_FRAME = 6,
    LOCKSTEP_ERROR_BAD_HELLO_MAGIC = 7,
    LOCKSTEP_ERROR_OTHER = 0,
} lockstep_error_code_t;

typedef enum
{
    LOCKSTEP_FRAMING_OK = 0,
    LOCKSTEP_FRAMING_ERR_FRAME_TOO_LARGE,
    LOCKSTEP_FRAMING_ERR_TRUNCATED, /* fewer bytes available than the caller supplied (a short buffer) */
    LOCKSTEP_FRAMING_ERR_BUFFER_TOO_SMALL,
    LOCKSTEP_FRAMING_ERR_ZERO_LENGTH, /* a frame must carry at least the frame_type byte */
} lockstep_framing_status_t;

#define LOCKSTEP_HELLO_MAGIC "AVL1"
#define LOCKSTEP_HELLO_PAYLOAD_LEN 6u
#define LOCKSTEP_PROTOCOL_VERSION 1u

/* Writes the 6-byte HELLO payload (magic + little-endian u16 version) into `out` (must be at
 * least LOCKSTEP_HELLO_PAYLOAD_LEN bytes). */
void lockstep_encode_hello(uint16_t version, uint8_t *out);

/* Parses a HELLO payload (exactly LOCKSTEP_HELLO_PAYLOAD_LEN bytes). Returns
 * LOCKSTEP_FRAMING_ERR_BUFFER_TOO_SMALL if `payload_len` is wrong. */
lockstep_framing_status_t lockstep_decode_hello(const uint8_t *payload, size_t payload_len, char magic_out[4], uint16_t *version_out);

/* Writes an ERROR payload (`code`, `expected`, `actual` little-endian, then the UTF-8 message)
 * into `out` (capacity `out_cap`); sets `*out_len`. Returns
 * LOCKSTEP_FRAMING_ERR_BUFFER_TOO_SMALL if `out_cap` is insufficient. */
lockstep_framing_status_t lockstep_encode_error(uint8_t code, int64_t expected, int64_t actual, const char *message, uint8_t *out, size_t out_cap, size_t *out_len);

/* Writes the 4-byte little-endian `length` field (`length = 1 + payload_len`, excluding
 * itself, per services/cfs/README.md's own "Frame layout" section) into `out` (must be at
 * least 4 bytes). Returns LOCKSTEP_FRAMING_ERR_FRAME_TOO_LARGE if `1 + payload_len` would not
 * fit the bound this module enforces on both read and write
 * (`LOCKSTEP_LOCAL_MAX_FRAME_LEN`). */
lockstep_framing_status_t lockstep_encode_length_field(size_t payload_len, uint8_t out[4]);

/* Parses a 4-byte `length` field already read from the wire -- the first thing a reader reads
 * for every frame, before it knows how many more bytes to read. Returns
 * LOCKSTEP_FRAMING_ERR_ZERO_LENGTH if `length == 0` (no room for even the `frame_type` byte)
 * and LOCKSTEP_FRAMING_ERR_FRAME_TOO_LARGE if `length` exceeds the bound -- checked *before* a
 * caller would size a buffer from it, so a garbled or hostile length field cannot drive an
 * unbounded allocation (services/cfs/README.md's own rule). On success the caller reads exactly
 * `*length_out` more bytes: the first is `frame_type`, the rest (`*length_out - 1` bytes) is
 * `payload`. */
lockstep_framing_status_t lockstep_decode_length_field(const uint8_t in[4], uint32_t *length_out);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_LOCKSTEP_LOCAL_FRAMING_H */
