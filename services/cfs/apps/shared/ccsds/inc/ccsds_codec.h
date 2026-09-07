/* CCSDS Space Packet (CCSDS 133.0-B-2) encode/decode -- the ONE C implementation of this
 * platform's bit-packing/header layout, shared by every cFS app under services/cfs/apps/ that
 * speaks a FRAMED port (docs/open-questions.md question 149, docs/sil-plan.md M23).
 *
 * M23.4 (docs/open-questions.md's M23 reconciliation notes) moved this module here from
 * services/cfs/apps/io_lockstep/fsw/{inc,src}/ -- M23.2 (the lockstep I/O app) and M23.3 (the
 * reference ADCS app) each landed their own independent C port of the same wire format
 * (io_lockstep's own `ccsds_codec.c` and ADCS's own `adcs_packets.c`'s internal bit-packing),
 * both checked against real `av_kernel::codec`-produced golden bytes but never against EACH
 * OTHER -- exactly the drift risk a from-scratch C reimplementation invites. Rather than keep
 * two and pin them against the same golden (the brief's own fallback), this batch resolves it
 * the more direct way: one implementation, used by both `io_lockstep` (`fsw/src/
 * io_lockstep_port_table.c`) and `adcs` (`fsw/src/adcs_packets.c`, now a thin wrapper over
 * `ccsds_encode_packet`/`ccsds_decode_packet` instead of its own bitfield reader/writer).
 * `services/cfs/apps/io_lockstep/CMakeLists.txt` and `services/cfs/apps/adcs/CMakeLists.txt`
 * both build this file into a small static library (`ccsds_codec`, guarded the same
 * `if(NOT TARGET ...)` way `psp_lockstep` already is, since either app's CMakeLists may be
 * processed first) and link against it, rather than each compiling its own copy.
 *
 * This is a from-scratch C port of `crates/av-kernel/src/codec.rs`'s `encode_packet`/
 * `decode_packet` (that crate is off limits for this task to edit or link against from cFS --
 * see services/cfs/README.md's ownership note) -- same primary-header layout, same
 * "packet_data_length = secondary_header_bytes + user_data_bytes - 1" convention (CCSDS
 * 133.0-B 4.1.2.5), same MSB-first bit numbering for fields, same
 * `engineering = raw * scale + offset` convention with `scale == 0.0` meaning 1.0.
 * `services/cfs/tests/test_ccsds_golden.py` proves this port agrees with the real Rust codec
 * byte-for-byte, not merely with itself, by comparing against bytes
 * `services/cfs/tests/golden_gen` produced by calling `av_kernel::codec::encode_packet` directly
 * -- and, since this module is now the ONE implementation both apps link, that same golden test
 * covers `adcs_packets.c`'s own encode/decode path too: any future drift in this file fails
 * `test_ccsds_golden.py` for both apps at once, by construction, not by a second, separately
 * maintained cross-check.
 *
 * Deliberately narrower than the Rust module: only UINT/INT/FLOAT32/FLOAT64 numeric fields are
 * supported (every `PacketCodec` this platform declares so far -- see the drms YAML system
 * fixtures -- uses only FLOAT64 fields). `BYTES` fields are refused with
 * `CCSDS_ERR_UNSUPPORTED_FIELD_TYPE` rather than silently mishandled; adding them is
 * straightforward (a raw byte copy, see the Rust module's own comment) but out of this batch's
 * verified scope.
 */
#ifndef AV_CFS_CCSDS_CODEC_H
#define AV_CFS_CCSDS_CODEC_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define CCSDS_PRIMARY_HEADER_LEN 6u
#define CCSDS_MAX_APID 0x7FFu
#define CCSDS_MAX_SEQUENCE_COUNT 0x3FFFu
#define CCSDS_MAX_FIELDS 32u

typedef enum
{
    CCSDS_FIELD_UINT = 1,
    CCSDS_FIELD_INT = 2,
    CCSDS_FIELD_FLOAT32 = 3,
    CCSDS_FIELD_FLOAT64 = 4,
} ccsds_field_type_t;

typedef struct
{
    const char *name; /* borrowed, for error messages only */
    uint32_t bit_offset;
    uint32_t bit_width;
    ccsds_field_type_t type;
    double scale;  /* 0.0 means "1.0", matching PacketField.scale's own doc comment */
    double offset;
} ccsds_field_t;

typedef struct
{
    const char *id; /* borrowed, for error messages only */
    uint32_t apid;
    bool is_command;
    uint32_t secondary_header_bytes;
    uint32_t user_data_bytes;
    const ccsds_field_t *fields;
    size_t field_count;
} ccsds_codec_t;

typedef enum
{
    CCSDS_OK = 0,
    CCSDS_ERR_APID_RANGE,
    CCSDS_ERR_SEQUENCE_RANGE,
    CCSDS_ERR_SECONDARY_HEADER_LENGTH,
    CCSDS_ERR_INVALID_USER_DATA_BYTES,
    CCSDS_ERR_FIELD_EXTENT,
    CCSDS_ERR_UNSUPPORTED_FIELD_TYPE,
    CCSDS_ERR_VALUE_OUT_OF_RANGE,
    CCSDS_ERR_BUFFER_TOO_SMALL,
    CCSDS_ERR_PACKET_TOO_SHORT,
    CCSDS_ERR_UNKNOWN_APID,
    CCSDS_ERR_LENGTH_FIELD_MISMATCH,
    CCSDS_ERR_PACKET_LENGTH_MISMATCH,
    CCSDS_ERR_TOO_MANY_FIELDS,
} ccsds_status_t;

const char *ccsds_status_str(ccsds_status_t s);

/* Encodes one packet: primary header + secondary_header (verbatim, secondary_header_bytes long)
 * + user data built from `values[i]` (engineering units, one entry per `codec->fields[i]`, same
 * order -- there is no name lookup in this C port, unlike the Rust map-keyed API, to keep this
 * module free of any dynamic allocation or dictionary). Writes
 * `CCSDS_PRIMARY_HEADER_LEN + secondary_header_bytes + user_data_bytes` bytes to `out` (which
 * must be at least `out_cap` of that size) and sets `*out_len` on success. */
ccsds_status_t ccsds_encode_packet(const ccsds_codec_t *codec, uint16_t sequence_count, const uint8_t *secondary_header, const double *values, uint8_t *out, size_t out_cap, size_t *out_len);

/* Decodes `data` (exactly one packet, `data_len` bytes) using `codec` -- caller has already
 * matched `codec->apid` against the primary header's own APID field (this port has no
 * dictionary/map type; `ccsds_decode_apid` below reads just the APID so a caller can look up
 * the right `ccsds_codec_t` first, mirroring `av_kernel::codec::ApidMap` one level up, in the
 * calling app rather than in this module). Fills `values_out[i]` for `codec->fields[i]`. */
ccsds_status_t ccsds_decode_packet(const ccsds_codec_t *codec, const uint8_t *data, size_t data_len, double *values_out);

/* Reads just the primary header's APID field (bytes 0-1) -- used to select which `ccsds_codec_t`
 * to decode with, before the length/field checks in `ccsds_decode_packet` run. */
ccsds_status_t ccsds_decode_apid(const uint8_t *data, size_t data_len, uint32_t *apid_out);

/* M23.4: reads just the primary header's 14-bit sequence count field (the low 6 bits of byte 2
 * plus byte 3, CCSDS 133.0-B-2 4.1.3) -- added when `services/cfs/apps/adcs/fsw/src/
 * adcs_packets.c` became a wrapper over this module, so its own `ADCS_DecodePacket` (which has
 * always returned the decoded sequence count to its caller, unlike `ccsds_decode_packet` above)
 * reads it through this module rather than re-deriving the header's own bit layout itself --
 * closing that last sliver of the same duplication `ccsds_codec.h`'s own top comment describes
 * for the field-decoding path. Mirrors `ccsds_decode_apid`'s own contract exactly (same
 * `CCSDS_ERR_PACKET_TOO_SHORT` guard, no other validation). */
ccsds_status_t ccsds_decode_sequence_count(const uint8_t *data, size_t data_len, uint16_t *sequence_count_out);

#endif /* AV_CFS_CCSDS_CODEC_H */
