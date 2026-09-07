/* See lockstep_local_framing.h for the module doc comment. */
#include "lockstep_local_framing.h"

#include <string.h>

static void write_u16_le(uint8_t *out, uint16_t v)
{
    out[0] = (uint8_t)(v & 0xFFu);
    out[1] = (uint8_t)((v >> 8) & 0xFFu);
}

static uint16_t read_u16_le(const uint8_t *in)
{
    return (uint16_t)((uint16_t)in[0] | ((uint16_t)in[1] << 8));
}

static void write_u32_le(uint8_t *out, uint32_t v)
{
    out[0] = (uint8_t)(v & 0xFFu);
    out[1] = (uint8_t)((v >> 8) & 0xFFu);
    out[2] = (uint8_t)((v >> 16) & 0xFFu);
    out[3] = (uint8_t)((v >> 24) & 0xFFu);
}

static uint32_t read_u32_le(const uint8_t *in)
{
    return (uint32_t)in[0] | ((uint32_t)in[1] << 8) | ((uint32_t)in[2] << 16) | ((uint32_t)in[3] << 24);
}

static void write_i64_le(uint8_t *out, int64_t v)
{
    uint64_t u = (uint64_t)v;
    for (int i = 0; i < 8; ++i)
    {
        out[i] = (uint8_t)((u >> (8 * i)) & 0xFFu);
    }
}

void lockstep_encode_hello(uint16_t version, uint8_t *out)
{
    memcpy(out, LOCKSTEP_HELLO_MAGIC, 4);
    write_u16_le(out + 4, version);
}

lockstep_framing_status_t lockstep_decode_hello(const uint8_t *payload, size_t payload_len, char magic_out[4], uint16_t *version_out)
{
    if (payload_len != LOCKSTEP_HELLO_PAYLOAD_LEN) return LOCKSTEP_FRAMING_ERR_BUFFER_TOO_SMALL;
    memcpy(magic_out, payload, 4);
    *version_out = read_u16_le(payload + 4);
    return LOCKSTEP_FRAMING_OK;
}

lockstep_framing_status_t lockstep_encode_error(uint8_t code, int64_t expected, int64_t actual, const char *message, uint8_t *out, size_t out_cap, size_t *out_len)
{
    size_t message_len = strlen(message);
    size_t total = 1u + 8u + 8u + 2u + message_len;
    if (out_cap < total) return LOCKSTEP_FRAMING_ERR_BUFFER_TOO_SMALL;
    out[0] = code;
    write_i64_le(out + 1, expected);
    write_i64_le(out + 9, actual);
    write_u16_le(out + 17, (uint16_t)message_len);
    memcpy(out + 19, message, message_len);
    *out_len = total;
    return LOCKSTEP_FRAMING_OK;
}

lockstep_framing_status_t lockstep_encode_length_field(size_t payload_len, uint8_t out[4])
{
    if (1u + payload_len > LOCKSTEP_LOCAL_MAX_FRAME_LEN) return LOCKSTEP_FRAMING_ERR_FRAME_TOO_LARGE;
    write_u32_le(out, (uint32_t)(1u + payload_len));
    return LOCKSTEP_FRAMING_OK;
}

lockstep_framing_status_t lockstep_decode_length_field(const uint8_t in[4], uint32_t *length_out)
{
    uint32_t length = read_u32_le(in);
    if (length == 0) return LOCKSTEP_FRAMING_ERR_ZERO_LENGTH;
    if (length > LOCKSTEP_LOCAL_MAX_FRAME_LEN) return LOCKSTEP_FRAMING_ERR_FRAME_TOO_LARGE;
    *length_out = length;
    return LOCKSTEP_FRAMING_OK;
}
