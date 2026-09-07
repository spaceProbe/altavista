/* See ccsds_codec.h for the module doc comment. */
#include "ccsds_codec.h"

#include <math.h>
#include <string.h>

static const uint8_t SEQUENCE_FLAGS_UNSEGMENTED = 0x3u;

const char *ccsds_status_str(ccsds_status_t s)
{
    switch (s)
    {
        case CCSDS_OK: return "ok";
        case CCSDS_ERR_APID_RANGE: return "apid out of range (0..=2047)";
        case CCSDS_ERR_SEQUENCE_RANGE: return "sequence_count out of range (0..=16383)";
        case CCSDS_ERR_SECONDARY_HEADER_LENGTH: return "secondary_header length mismatch";
        case CCSDS_ERR_INVALID_USER_DATA_BYTES: return "user_data_bytes must be 1..=65536";
        case CCSDS_ERR_FIELD_EXTENT: return "field bit_offset+bit_width exceeds user_data_bytes";
        case CCSDS_ERR_UNSUPPORTED_FIELD_TYPE: return "unsupported field type (only UINT/INT/FLOAT32/FLOAT64)";
        case CCSDS_ERR_VALUE_OUT_OF_RANGE: return "engineering value does not fit its field";
        case CCSDS_ERR_BUFFER_TOO_SMALL: return "output buffer too small";
        case CCSDS_ERR_PACKET_TOO_SHORT: return "packet shorter than the primary header";
        case CCSDS_ERR_UNKNOWN_APID: return "apid does not match this codec";
        case CCSDS_ERR_LENGTH_FIELD_MISMATCH: return "packet data length field disagrees with codec";
        case CCSDS_ERR_PACKET_LENGTH_MISMATCH: return "packet byte length disagrees with codec";
        case CCSDS_ERR_TOO_MANY_FIELDS: return "codec declares more than CCSDS_MAX_FIELDS fields";
        default: return "unknown ccsds_status_t";
    }
}

/* Mirrors av_kernel::codec::read_bitfield_u64 exactly: MSB-first, bit 0 of a byte is its MSB. */
static uint64_t read_bitfield_u64(const uint8_t *data, uint32_t bit_offset, uint32_t bit_width)
{
    uint64_t value = 0;
    for (uint32_t i = 0; i < bit_width; ++i)
    {
        uint32_t bit_pos = bit_offset + i;
        size_t byte_idx = bit_pos / 8u;
        uint32_t bit_in_byte = 7u - (bit_pos % 8u);
        uint64_t bit = (uint64_t)((data[byte_idx] >> bit_in_byte) & 1u);
        value = (value << 1) | bit;
    }
    return value;
}

/* Mirrors av_kernel::codec::write_bitfield_u64 exactly. */
static void write_bitfield_u64(uint8_t *data, uint32_t bit_offset, uint32_t bit_width, uint64_t value)
{
    for (uint32_t i = 0; i < bit_width; ++i)
    {
        uint32_t bit_pos = bit_offset + i;
        size_t byte_idx = bit_pos / 8u;
        uint32_t bit_in_byte = 7u - (bit_pos % 8u);
        uint8_t bit = (uint8_t)((value >> (bit_width - 1u - i)) & 1u);
        data[byte_idx] = (uint8_t)((data[byte_idx] & ~(1u << bit_in_byte)) | (bit << bit_in_byte));
    }
}

static int64_t sign_extend(uint64_t raw, uint32_t bit_width)
{
    if (bit_width >= 64) return (int64_t)raw;
    uint32_t shift = 64u - bit_width;
    return ((int64_t)(raw << shift)) >> shift;
}

static uint64_t max_uint(uint32_t bit_width)
{
    if (bit_width >= 64) return UINT64_MAX;
    return (1ull << bit_width) - 1ull;
}

static void int_range(uint32_t bit_width, int64_t *min_out, int64_t *max_out)
{
    if (bit_width >= 64)
    {
        *min_out = INT64_MIN;
        *max_out = INT64_MAX;
        return;
    }
    *max_out = (1ll << (bit_width - 1)) - 1;
    *min_out = -(1ll << (bit_width - 1));
}

static double effective_scale(double scale)
{
    return scale == 0.0 ? 1.0 : scale;
}

static bool field_extent_ok(const ccsds_field_t *f, uint32_t user_data_bytes)
{
    uint64_t bit_end = (uint64_t)f->bit_offset + (uint64_t)f->bit_width;
    return bit_end <= (uint64_t)user_data_bytes * 8u;
}

ccsds_status_t ccsds_encode_packet(const ccsds_codec_t *codec, uint16_t sequence_count, const uint8_t *secondary_header, const double *values, uint8_t *out, size_t out_cap, size_t *out_len)
{
    if (codec->apid > CCSDS_MAX_APID) return CCSDS_ERR_APID_RANGE;
    if (sequence_count > CCSDS_MAX_SEQUENCE_COUNT) return CCSDS_ERR_SEQUENCE_RANGE;
    if (codec->user_data_bytes == 0 || codec->user_data_bytes > 65536u) return CCSDS_ERR_INVALID_USER_DATA_BYTES;
    if (codec->field_count > CCSDS_MAX_FIELDS) return CCSDS_ERR_TOO_MANY_FIELDS;

    size_t total = CCSDS_PRIMARY_HEADER_LEN + (size_t)codec->secondary_header_bytes + (size_t)codec->user_data_bytes;
    if (out_cap < total) return CCSDS_ERR_BUFFER_TOO_SMALL;
    memset(out, 0, total);

    uint8_t sec_hdr_flag = codec->secondary_header_bytes > 0 ? 1u : 0u;
    uint8_t type_bit = codec->is_command ? 1u : 0u;
    out[0] = (uint8_t)((0u << 5) | (type_bit << 4) | (sec_hdr_flag << 3) | ((codec->apid >> 8) & 0x7u));
    out[1] = (uint8_t)(codec->apid & 0xFFu);
    out[2] = (uint8_t)((SEQUENCE_FLAGS_UNSEGMENTED << 6) | ((sequence_count >> 8) & 0x3Fu));
    out[3] = (uint8_t)(sequence_count & 0xFFu);

    /* CCSDS 133.0-B 4.1.2.5: packet_data_length counts the whole Packet Data Field (secondary
     * header + user data) minus one -- this module's own doc comment / the Rust codec's
     * identical convention. */
    uint32_t packet_data_length = codec->secondary_header_bytes + codec->user_data_bytes - 1u;
    out[4] = (uint8_t)((packet_data_length >> 8) & 0xFFu);
    out[5] = (uint8_t)(packet_data_length & 0xFFu);

    if (codec->secondary_header_bytes > 0)
    {
        memcpy(out + CCSDS_PRIMARY_HEADER_LEN, secondary_header, codec->secondary_header_bytes);
    }

    uint8_t *user_data = out + CCSDS_PRIMARY_HEADER_LEN + codec->secondary_header_bytes;

    for (size_t i = 0; i < codec->field_count; ++i)
    {
        const ccsds_field_t *f = &codec->fields[i];
        if (!field_extent_ok(f, codec->user_data_bytes)) return CCSDS_ERR_FIELD_EXTENT;
        double eng = values[i];
        double scale = effective_scale(f->scale);
        uint64_t raw;
        switch (f->type)
        {
            case CCSDS_FIELD_FLOAT64:
            {
                if (f->bit_width != 64) return CCSDS_ERR_UNSUPPORTED_FIELD_TYPE;
                double phys = (eng - f->offset) / scale;
                memcpy(&raw, &phys, sizeof(raw));
                break;
            }
            case CCSDS_FIELD_FLOAT32:
            {
                if (f->bit_width != 32) return CCSDS_ERR_UNSUPPORTED_FIELD_TYPE;
                float phys = (float)((eng - f->offset) / scale);
                uint32_t raw32;
                memcpy(&raw32, &phys, sizeof(raw32));
                raw = raw32;
                break;
            }
            case CCSDS_FIELD_UINT:
            {
                double phys = (eng - f->offset) / scale;
                if (phys < 0.0 || phys > (double)max_uint(f->bit_width)) return CCSDS_ERR_VALUE_OUT_OF_RANGE;
                raw = (uint64_t)(phys + 0.5);
                break;
            }
            case CCSDS_FIELD_INT:
            {
                double phys = (eng - f->offset) / scale;
                int64_t min_v, max_v;
                int_range(f->bit_width, &min_v, &max_v);
                if (phys < (double)min_v || phys > (double)max_v) return CCSDS_ERR_VALUE_OUT_OF_RANGE;
                int64_t signed_raw = (int64_t)phys;
                raw = (uint64_t)signed_raw & max_uint(f->bit_width);
                break;
            }
            default:
                return CCSDS_ERR_UNSUPPORTED_FIELD_TYPE;
        }
        write_bitfield_u64(user_data, f->bit_offset, f->bit_width, raw);
    }

    *out_len = total;
    return CCSDS_OK;
}

ccsds_status_t ccsds_decode_apid(const uint8_t *data, size_t data_len, uint32_t *apid_out)
{
    if (data_len < CCSDS_PRIMARY_HEADER_LEN) return CCSDS_ERR_PACKET_TOO_SHORT;
    *apid_out = (((uint32_t)(data[0] & 0x07u)) << 8) | (uint32_t)data[1];
    return CCSDS_OK;
}

ccsds_status_t ccsds_decode_sequence_count(const uint8_t *data, size_t data_len, uint16_t *sequence_count_out)
{
    if (data_len < CCSDS_PRIMARY_HEADER_LEN) return CCSDS_ERR_PACKET_TOO_SHORT;
    *sequence_count_out = (uint16_t)(((uint16_t)(data[2] & 0x3Fu) << 8) | (uint16_t)data[3]);
    return CCSDS_OK;
}

ccsds_status_t ccsds_decode_packet(const ccsds_codec_t *codec, const uint8_t *data, size_t data_len, double *values_out)
{
    if (data_len < CCSDS_PRIMARY_HEADER_LEN) return CCSDS_ERR_PACKET_TOO_SHORT;

    uint32_t apid;
    ccsds_status_t st = ccsds_decode_apid(data, data_len, &apid);
    if (st != CCSDS_OK) return st;
    if (apid != codec->apid) return CCSDS_ERR_UNKNOWN_APID;

    uint32_t length_field = ((uint32_t)data[4] << 8) | (uint32_t)data[5];
    uint32_t expected_length_field = codec->secondary_header_bytes + codec->user_data_bytes - 1u;
    if (length_field != expected_length_field) return CCSDS_ERR_LENGTH_FIELD_MISMATCH;

    size_t expected_total = CCSDS_PRIMARY_HEADER_LEN + (size_t)codec->secondary_header_bytes + (size_t)codec->user_data_bytes;
    if (data_len != expected_total) return CCSDS_ERR_PACKET_LENGTH_MISMATCH;

    const uint8_t *user_data = data + CCSDS_PRIMARY_HEADER_LEN + codec->secondary_header_bytes;

    for (size_t i = 0; i < codec->field_count; ++i)
    {
        const ccsds_field_t *f = &codec->fields[i];
        if (!field_extent_ok(f, codec->user_data_bytes)) return CCSDS_ERR_FIELD_EXTENT;
        double scale = effective_scale(f->scale);
        uint64_t raw = read_bitfield_u64(user_data, f->bit_offset, f->bit_width);
        double eng;
        switch (f->type)
        {
            case CCSDS_FIELD_FLOAT64:
            {
                if (f->bit_width != 64) return CCSDS_ERR_UNSUPPORTED_FIELD_TYPE;
                double phys;
                memcpy(&phys, &raw, sizeof(phys));
                eng = phys * scale + f->offset;
                break;
            }
            case CCSDS_FIELD_FLOAT32:
            {
                if (f->bit_width != 32) return CCSDS_ERR_UNSUPPORTED_FIELD_TYPE;
                uint32_t raw32 = (uint32_t)raw;
                float phys;
                memcpy(&phys, &raw32, sizeof(phys));
                eng = (double)phys * scale + f->offset;
                break;
            }
            case CCSDS_FIELD_UINT:
                eng = (double)raw * scale + f->offset;
                break;
            case CCSDS_FIELD_INT:
                eng = (double)sign_extend(raw, f->bit_width) * scale + f->offset;
                break;
            default:
                return CCSDS_ERR_UNSUPPORTED_FIELD_TYPE;
        }
        values_out[i] = eng;
    }
    return CCSDS_OK;
}
