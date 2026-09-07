/* See pbmini.h for the module doc comment. */
#include "pbmini.h"

#include <string.h>

void pbmini_writer_init(pbmini_writer_t *w, uint8_t *buf, size_t cap)
{
    w->buf = buf;
    w->cap = cap;
    w->len = 0;
}

static pbmini_status_t write_byte(pbmini_writer_t *w, uint8_t b)
{
    if (w->len >= w->cap) return PBMINI_ERR_BUFFER_TOO_SMALL;
    w->buf[w->len++] = b;
    return PBMINI_OK;
}

pbmini_status_t pbmini_write_varint(pbmini_writer_t *w, uint64_t value)
{
    do
    {
        uint8_t byte = (uint8_t)(value & 0x7Fu);
        value >>= 7;
        if (value != 0) byte |= 0x80u;
        pbmini_status_t st = write_byte(w, byte);
        if (st != PBMINI_OK) return st;
    } while (value != 0);
    return PBMINI_OK;
}

pbmini_status_t pbmini_write_tag(pbmini_writer_t *w, uint32_t field_number, uint32_t wire_type)
{
    return pbmini_write_varint(w, ((uint64_t)field_number << 3) | (uint64_t)wire_type);
}

pbmini_status_t pbmini_write_varint_field(pbmini_writer_t *w, uint32_t field_number, uint64_t value)
{
    if (value == 0) return PBMINI_OK; /* proto3: default value is never encoded */
    pbmini_status_t st = pbmini_write_tag(w, field_number, PBMINI_WIRETYPE_VARINT);
    if (st != PBMINI_OK) return st;
    return pbmini_write_varint(w, value);
}

pbmini_status_t pbmini_write_bool_field(pbmini_writer_t *w, uint32_t field_number, bool value)
{
    if (!value) return PBMINI_OK;
    pbmini_status_t st = pbmini_write_tag(w, field_number, PBMINI_WIRETYPE_VARINT);
    if (st != PBMINI_OK) return st;
    return pbmini_write_varint(w, 1);
}

pbmini_status_t pbmini_write_bytes_field(pbmini_writer_t *w, uint32_t field_number, const uint8_t *data, size_t len)
{
    if (len == 0) return PBMINI_OK; /* proto3: empty bytes/string/repeated is never encoded */
    pbmini_status_t st = pbmini_write_tag(w, field_number, PBMINI_WIRETYPE_LEN);
    if (st != PBMINI_OK) return st;
    st = pbmini_write_varint(w, (uint64_t)len);
    if (st != PBMINI_OK) return st;
    if (w->len + len > w->cap) return PBMINI_ERR_BUFFER_TOO_SMALL;
    memcpy(w->buf + w->len, data, len);
    w->len += len;
    return PBMINI_OK;
}

pbmini_status_t pbmini_write_string_field(pbmini_writer_t *w, uint32_t field_number, const char *s)
{
    return pbmini_write_bytes_field(w, field_number, (const uint8_t *)s, strlen(s));
}

pbmini_status_t pbmini_write_submessage_field(pbmini_writer_t *w, uint32_t field_number, const uint8_t *submessage, size_t len)
{
    return pbmini_write_bytes_field(w, field_number, submessage, len);
}

void pbmini_reader_init(pbmini_reader_t *r, const uint8_t *buf, size_t len)
{
    r->buf = buf;
    r->len = len;
    r->pos = 0;
}

bool pbmini_reader_at_end(const pbmini_reader_t *r)
{
    return r->pos >= r->len;
}

pbmini_status_t pbmini_read_varint(pbmini_reader_t *r, uint64_t *value)
{
    uint64_t result = 0;
    int shift = 0;
    while (1)
    {
        if (r->pos >= r->len) return PBMINI_ERR_TRUNCATED;
        uint8_t byte = r->buf[r->pos++];
        result |= ((uint64_t)(byte & 0x7Fu)) << shift;
        if ((byte & 0x80u) == 0) break;
        shift += 7;
        if (shift >= 64) return PBMINI_ERR_MALFORMED_VARINT;
    }
    *value = result;
    return PBMINI_OK;
}

pbmini_status_t pbmini_read_tag(pbmini_reader_t *r, uint32_t *field_number, uint32_t *wire_type)
{
    uint64_t tag;
    pbmini_status_t st = pbmini_read_varint(r, &tag);
    if (st != PBMINI_OK) return st;
    *field_number = (uint32_t)(tag >> 3);
    *wire_type = (uint32_t)(tag & 0x7u);
    return PBMINI_OK;
}

pbmini_status_t pbmini_read_len(pbmini_reader_t *r, const uint8_t **data, size_t *len)
{
    uint64_t length;
    pbmini_status_t st = pbmini_read_varint(r, &length);
    if (st != PBMINI_OK) return st;
    if (r->pos + length > r->len) return PBMINI_ERR_TRUNCATED;
    *data = r->buf + r->pos;
    *len = (size_t)length;
    r->pos += (size_t)length;
    return PBMINI_OK;
}

pbmini_status_t pbmini_skip_field(pbmini_reader_t *r, uint32_t wire_type)
{
    switch (wire_type)
    {
        case PBMINI_WIRETYPE_VARINT:
        {
            uint64_t discard;
            return pbmini_read_varint(r, &discard);
        }
        case PBMINI_WIRETYPE_LEN:
        {
            const uint8_t *discard_data;
            size_t discard_len;
            return pbmini_read_len(r, &discard_data, &discard_len);
        }
        case PBMINI_WIRETYPE_FIXED64:
            if (r->pos + 8 > r->len) return PBMINI_ERR_TRUNCATED;
            r->pos += 8;
            return PBMINI_OK;
        case PBMINI_WIRETYPE_FIXED32:
            if (r->pos + 4 > r->len) return PBMINI_ERR_TRUNCATED;
            r->pos += 4;
            return PBMINI_OK;
        default:
            return PBMINI_ERR_WRONG_WIRE_TYPE;
    }
}
