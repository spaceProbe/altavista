/* See lockstep_messages.h for the module doc comment. */
#include "lockstep_messages.h"

#include <string.h>

static pbmini_status_t copy_string_field(const uint8_t *data, size_t len, char *out, size_t out_cap)
{
    if (len + 1 > out_cap) return PBMINI_ERR_BUFFER_TOO_SMALL;
    memcpy(out, data, len);
    out[len] = '\0';
    return PBMINI_OK;
}

static pbmini_status_t decode_port_message(const uint8_t *data, size_t len, lockstep_port_message_t *out)
{
    memset(out, 0, sizeof(*out));
    pbmini_reader_t r;
    pbmini_reader_init(&r, data, len);
    while (!pbmini_reader_at_end(&r))
    {
        uint32_t field_number, wire_type;
        pbmini_status_t st = pbmini_read_tag(&r, &field_number, &wire_type);
        if (st != PBMINI_OK) return st;
        switch (field_number)
        {
            case 1: /* port */
            {
                const uint8_t *s;
                size_t slen;
                st = pbmini_read_len(&r, &s, &slen);
                if (st != PBMINI_OK) return st;
                st = copy_string_field(s, slen, out->port, sizeof(out->port));
                if (st != PBMINI_OK) return st;
                break;
            }
            case 2: /* tai_ns */
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->tai_ns = (int64_t)v;
                break;
            }
            case 3: /* payload */
            {
                const uint8_t *p;
                size_t plen;
                st = pbmini_read_len(&r, &p, &plen);
                if (st != PBMINI_OK) return st;
                if (plen > sizeof(out->payload)) return PBMINI_ERR_BUFFER_TOO_SMALL;
                memcpy(out->payload, p, plen);
                out->payload_len = plen;
                break;
            }
            default:
                st = pbmini_skip_field(&r, wire_type);
                if (st != PBMINI_OK) return st;
        }
    }
    return PBMINI_OK;
}

static pbmini_status_t encode_port_message(const lockstep_port_message_t *msg, uint8_t *out, size_t out_cap, size_t *out_len)
{
    pbmini_writer_t w;
    pbmini_writer_init(&w, out, out_cap);
    pbmini_status_t st = pbmini_write_string_field(&w, 1, msg->port);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_varint_field(&w, 2, (uint64_t)msg->tai_ns);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_bytes_field(&w, 3, msg->payload, msg->payload_len);
    if (st != PBMINI_OK) return st;
    *out_len = w.len;
    return PBMINI_OK;
}

pbmini_status_t lockstep_decode_bind_request(const uint8_t *data, size_t len, lockstep_bind_request_t *out)
{
    memset(out, 0, sizeof(*out));
    pbmini_reader_t r;
    pbmini_reader_init(&r, data, len);
    while (!pbmini_reader_at_end(&r))
    {
        uint32_t field_number, wire_type;
        pbmini_status_t st = pbmini_read_tag(&r, &field_number, &wire_type);
        if (st != PBMINI_OK) return st;
        switch (field_number)
        {
            case 1: /* run_id */
            {
                const uint8_t *s;
                size_t slen;
                st = pbmini_read_len(&r, &s, &slen);
                if (st != PBMINI_OK) return st;
                st = copy_string_field(s, slen, out->run_id, sizeof(out->run_id));
                if (st != PBMINI_OK) return st;
                break;
            }
            case 2: /* instance */
            {
                const uint8_t *s;
                size_t slen;
                st = pbmini_read_len(&r, &s, &slen);
                if (st != PBMINI_OK) return st;
                st = copy_string_field(s, slen, out->instance, sizeof(out->instance));
                if (st != PBMINI_OK) return st;
                break;
            }
            case 4: /* start_tai_ns */
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->start_tai_ns = (int64_t)v;
                break;
            }
            case 5: /* base_period_ns */
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->base_period_ns = (int64_t)v;
                break;
            }
            case 6: /* step_period_ns */
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->step_period_ns = (int64_t)v;
                break;
            }
            case 7: /* seed */
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->seed = v;
                break;
            }
            /* field 3 (ports, repeated Port) and field 8 (parameters, a map) -- see this
             * module's own doc comment for why these are skipped, not parsed. */
            default:
                st = pbmini_skip_field(&r, wire_type);
                if (st != PBMINI_OK) return st;
        }
    }
    return PBMINI_OK;
}

pbmini_status_t lockstep_decode_step_request(const uint8_t *data, size_t len, lockstep_step_request_t *out)
{
    memset(out, 0, sizeof(*out));
    pbmini_reader_t r;
    pbmini_reader_init(&r, data, len);
    while (!pbmini_reader_at_end(&r))
    {
        uint32_t field_number, wire_type;
        pbmini_status_t st = pbmini_read_tag(&r, &field_number, &wire_type);
        if (st != PBMINI_OK) return st;
        switch (field_number)
        {
            case 1: /* sequence */
            {
                st = pbmini_read_varint(&r, &out->sequence);
                if (st != PBMINI_OK) return st;
                break;
            }
            case 2: /* until_tai_ns */
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->until_tai_ns = (int64_t)v;
                break;
            }
            case 3: /* inputs, repeated PortMessage */
            {
                const uint8_t *sub;
                size_t sublen;
                st = pbmini_read_len(&r, &sub, &sublen);
                if (st != PBMINI_OK) return st;
                if (out->input_count >= LOCKSTEP_MSG_MAX_PORT_MESSAGES) return PBMINI_ERR_BUFFER_TOO_SMALL;
                st = decode_port_message(sub, sublen, &out->inputs[out->input_count]);
                if (st != PBMINI_OK) return st;
                out->input_count += 1;
                break;
            }
            default:
                st = pbmini_skip_field(&r, wire_type);
                if (st != PBMINI_OK) return st;
        }
    }
    return PBMINI_OK;
}

pbmini_status_t lockstep_decode_reset_request(const uint8_t *data, size_t len, lockstep_reset_request_t *out)
{
    memset(out, 0, sizeof(*out));
    pbmini_reader_t r;
    pbmini_reader_init(&r, data, len);
    while (!pbmini_reader_at_end(&r))
    {
        uint32_t field_number, wire_type;
        pbmini_status_t st = pbmini_read_tag(&r, &field_number, &wire_type);
        if (st != PBMINI_OK) return st;
        switch (field_number)
        {
            case 1:
                st = pbmini_read_varint(&r, &out->sequence);
                if (st != PBMINI_OK) return st;
                break;
            case 2:
            {
                uint64_t v;
                st = pbmini_read_varint(&r, &v);
                if (st != PBMINI_OK) return st;
                out->tai_ns = (int64_t)v;
                break;
            }
            case 3:
            {
                const uint8_t *s;
                size_t slen;
                st = pbmini_read_len(&r, &s, &slen);
                if (st != PBMINI_OK) return st;
                st = copy_string_field(s, slen, out->reason, sizeof(out->reason));
                if (st != PBMINI_OK) return st;
                break;
            }
            default:
                st = pbmini_skip_field(&r, wire_type);
                if (st != PBMINI_OK) return st;
        }
    }
    return PBMINI_OK;
}

pbmini_status_t lockstep_decode_shutdown_request(const uint8_t *data, size_t len, lockstep_shutdown_request_t *out)
{
    memset(out, 0, sizeof(*out));
    pbmini_reader_t r;
    pbmini_reader_init(&r, data, len);
    while (!pbmini_reader_at_end(&r))
    {
        uint32_t field_number, wire_type;
        pbmini_status_t st = pbmini_read_tag(&r, &field_number, &wire_type);
        if (st != PBMINI_OK) return st;
        if (field_number == 1)
        {
            const uint8_t *s;
            size_t slen;
            st = pbmini_read_len(&r, &s, &slen);
            if (st != PBMINI_OK) return st;
            st = copy_string_field(s, slen, out->run_id, sizeof(out->run_id));
            if (st != PBMINI_OK) return st;
        }
        else
        {
            st = pbmini_skip_field(&r, wire_type);
            if (st != PBMINI_OK) return st;
        }
    }
    return PBMINI_OK;
}

pbmini_status_t lockstep_encode_bind_response(bool lockstep_capable, const char *binding_hash, const char *version, const char *refusal_reason, uint8_t *out, size_t out_cap, size_t *out_len)
{
    pbmini_writer_t w;
    pbmini_writer_init(&w, out, out_cap);
    pbmini_status_t st = pbmini_write_bool_field(&w, 1, lockstep_capable);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_string_field(&w, 2, binding_hash);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_string_field(&w, 3, version);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_string_field(&w, 4, refusal_reason);
    if (st != PBMINI_OK) return st;
    *out_len = w.len;
    return PBMINI_OK;
}

pbmini_status_t lockstep_encode_step_response(uint64_t sequence, int64_t reached_tai_ns, const lockstep_port_message_t *outputs, size_t output_count, uint8_t *out, size_t out_cap, size_t *out_len)
{
    pbmini_writer_t w;
    pbmini_writer_init(&w, out, out_cap);
    pbmini_status_t st = pbmini_write_varint_field(&w, 1, sequence);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_varint_field(&w, 2, (uint64_t)reached_tai_ns);
    if (st != PBMINI_OK) return st;
    uint8_t sub[LOCKSTEP_MSG_MAX_STRING + LOCKSTEP_MSG_MAX_PAYLOAD + 16];
    for (size_t i = 0; i < output_count; ++i)
    {
        size_t sublen;
        st = encode_port_message(&outputs[i], sub, sizeof(sub), &sublen);
        if (st != PBMINI_OK) return st;
        st = pbmini_write_submessage_field(&w, 3, sub, sublen);
        if (st != PBMINI_OK) return st;
    }
    *out_len = w.len;
    return PBMINI_OK;
}

pbmini_status_t lockstep_encode_reset_response(uint64_t sequence, uint8_t *out, size_t out_cap, size_t *out_len)
{
    pbmini_writer_t w;
    pbmini_writer_init(&w, out, out_cap);
    pbmini_status_t st = pbmini_write_varint_field(&w, 1, sequence);
    if (st != PBMINI_OK) return st;
    *out_len = w.len;
    return PBMINI_OK;
}

pbmini_status_t lockstep_encode_shutdown_response(uint8_t *out, size_t out_cap, size_t *out_len)
{
    (void)out;
    (void)out_cap;
    *out_len = 0; /* LockstepShutdownResponse has no fields */
    return PBMINI_OK;
}
