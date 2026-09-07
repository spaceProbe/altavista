/*
 * adcs_packets.c -- see adcs_packets.h for the full contract and wire-format derivation, and
 * for the M23.4 reconciliation note: `ADCS_EncodePacket`/`ADCS_DecodePacket` below now delegate
 * to `services/cfs/apps/shared/ccsds`'s `ccsds_encode_packet`/`ccsds_decode_packet` instead of
 * this file's own (removed) bitfield reader/writer -- the bit-packing itself lives in exactly
 * one place now, shared with `services/cfs/apps/io_lockstep`.
 *
 * No cFE/OSAL dependency (plain C99 + <string.h>/<stdint.h>, plus the equally cFE-free shared
 * `ccsds_codec.h`), so this file builds and is unit-tested
 * (services/cfs/apps/adcs/unit-test/test_adcs_packets.c) with a plain host compiler whether or
 * not third_party/cfs (M23.2) has landed.
 */
#include "adcs_packets.h"
#include "ccsds_codec.h"

const char *ADCS_CodecStatusName(ADCS_CodecStatus_t status)
{
    switch (status) {
        case ADCS_CODEC_OK:
            return "OK";
        case ADCS_CODEC_ERR_TOO_SHORT_FOR_HEADER:
            return "TOO_SHORT_FOR_HEADER";
        case ADCS_CODEC_ERR_WRONG_APID:
            return "WRONG_APID";
        case ADCS_CODEC_ERR_LENGTH_FIELD_MISMATCH:
            return "LENGTH_FIELD_MISMATCH";
        case ADCS_CODEC_ERR_TOTAL_LENGTH_MISMATCH:
            return "TOTAL_LENGTH_MISMATCH";
        case ADCS_CODEC_ERR_OUTPUT_BUFFER_TOO_SMALL:
            return "OUTPUT_BUFFER_TOO_SMALL";
        case ADCS_CODEC_ERR_SEQUENCE_COUNT_OUT_OF_RANGE:
            return "SEQUENCE_COUNT_OUT_OF_RANGE";
        case ADCS_CODEC_ERR_INTERNAL:
            return "INTERNAL";
        default:
            return "UNKNOWN_CODEC_STATUS";
    }
}

/* Maps the shared codec's `ccsds_status_t` onto this header's own, pre-existing
 * `ADCS_CodecStatus_t` -- see `ADCS_CODEC_ERR_INTERNAL`'s own doc comment (adcs_packets.h) for
 * why every `ccsds_status_t` without a named ADCS counterpart falls there rather than being
 * mis-mapped onto an unrelated packet-shaped status. */
static ADCS_CodecStatus_t ADCS_MapCcsdsStatus(ccsds_status_t st)
{
    switch (st) {
        case CCSDS_OK:
            return ADCS_CODEC_OK;
        case CCSDS_ERR_SEQUENCE_RANGE:
            return ADCS_CODEC_ERR_SEQUENCE_COUNT_OUT_OF_RANGE;
        case CCSDS_ERR_BUFFER_TOO_SMALL:
            return ADCS_CODEC_ERR_OUTPUT_BUFFER_TOO_SMALL;
        case CCSDS_ERR_PACKET_TOO_SHORT:
            return ADCS_CODEC_ERR_TOO_SHORT_FOR_HEADER;
        case CCSDS_ERR_UNKNOWN_APID:
            return ADCS_CODEC_ERR_WRONG_APID;
        case CCSDS_ERR_LENGTH_FIELD_MISMATCH:
            return ADCS_CODEC_ERR_LENGTH_FIELD_MISMATCH;
        case CCSDS_ERR_PACKET_LENGTH_MISMATCH:
            return ADCS_CODEC_ERR_TOTAL_LENGTH_MISMATCH;
        case CCSDS_ERR_APID_RANGE:
        case CCSDS_ERR_SECONDARY_HEADER_LENGTH:
        case CCSDS_ERR_INVALID_USER_DATA_BYTES:
        case CCSDS_ERR_FIELD_EXTENT:
        case CCSDS_ERR_UNSUPPORTED_FIELD_TYPE:
        case CCSDS_ERR_VALUE_OUT_OF_RANGE:
        case CCSDS_ERR_TOO_MANY_FIELDS:
        default:
            /* Every one of these is a codec-*table* defect this app's own three fixed,
             * compile-time-constant tables never trigger -- see ADCS_CODEC_ERR_INTERNAL's own
             * doc comment. */
            return ADCS_CODEC_ERR_INTERNAL;
    }
}

/* This app's own field tables never exceed 6 entries (the IMU packet: wx,wy,wz,ax,ay,az) --
 * bounded well under `CCSDS_MAX_FIELDS` (32) -- so a fixed-size stack array (no VLA, no heap)
 * covers every real call; a caller asking for more is refused as ADCS_CODEC_ERR_INTERNAL rather
 * than silently truncated or overflowing the array. */
#define ADCS_PACKETS_MAX_FIELDS 8u

/* Builds the shared-codec's `ccsds_field_t` array (every field here is FLOAT64, byte-aligned,
 * 64 bits wide, scale=1.0/offset=0.0 -- see adcs_packets.h's own wire-format derivation) and
 * the `ccsds_codec_t` descriptor from this file's own `ADCS_PacketCodecMeta_t` + bit_offsets --
 * the one place `ADCS_EncodePacket`/`ADCS_DecodePacket` translate this app's own narrower,
 * FLOAT64-only vocabulary into the shared module's more general one. */
static ADCS_CodecStatus_t ADCS_BuildCcsdsCodec(const ADCS_PacketCodecMeta_t *meta, const uint32_t *bit_offsets, size_t n_fields, ccsds_field_t fields_out[ADCS_PACKETS_MAX_FIELDS], ccsds_codec_t *codec_out)
{
    if (n_fields > ADCS_PACKETS_MAX_FIELDS) {
        return ADCS_CODEC_ERR_INTERNAL;
    }
    for (size_t i = 0; i < n_fields; i++) {
        fields_out[i].name = NULL;
        fields_out[i].bit_offset = bit_offsets[i];
        fields_out[i].bit_width = 64u;
        fields_out[i].type = CCSDS_FIELD_FLOAT64;
        fields_out[i].scale = 0.0; /* 0.0 means 1.0, ccsds_field_t's own doc comment */
        fields_out[i].offset = 0.0;
    }
    codec_out->id = NULL;
    codec_out->apid = meta->apid;
    codec_out->is_command = meta->is_command;
    codec_out->secondary_header_bytes = meta->secondary_header_bytes;
    codec_out->user_data_bytes = meta->user_data_bytes;
    codec_out->fields = fields_out;
    codec_out->field_count = n_fields;
    return ADCS_CODEC_OK;
}

/* -------------------------------------------------------------------------------------------
 * Generic per-codec encode/decode over an array of FLOAT64 (bit_offset) fields -- the shared
 * entry point every ADCS_{Encode,Decode}{StarTracker,Imu,WheelTorque}Packet wrapper below
 * drives with its own fixed field-offset table. Delegates the actual header/bit-packing work
 * to `services/cfs/apps/shared/ccsds`'s `ccsds_encode_packet`/`ccsds_decode_packet` (M23.4) --
 * this function's own job is only the `ADCS_PacketCodecMeta_t`/`bit_offsets` -> `ccsds_codec_t`
 * translation and the `ccsds_status_t` -> `ADCS_CodecStatus_t` mapping.
 * ---------------------------------------------------------------------------------------- */

ADCS_CodecStatus_t ADCS_EncodePacket(const ADCS_PacketCodecMeta_t *meta, uint16_t sequence_count, const uint8_t *secondary_header, const uint32_t *bit_offsets, const double *values, size_t n_fields, uint8_t *out_buf, size_t out_buf_len, size_t *out_len)
{
    ccsds_field_t fields[ADCS_PACKETS_MAX_FIELDS];
    ccsds_codec_t codec;
    ADCS_CodecStatus_t build_status = ADCS_BuildCcsdsCodec(meta, bit_offsets, n_fields, fields, &codec);
    if (build_status != ADCS_CODEC_OK) {
        return build_status;
    }
    ccsds_status_t st = ccsds_encode_packet(&codec, sequence_count, secondary_header, values, out_buf, out_buf_len, out_len);
    return ADCS_MapCcsdsStatus(st);
}

ADCS_CodecStatus_t ADCS_DecodePacket(const ADCS_PacketCodecMeta_t *meta, const uint8_t *buf, size_t len, const uint32_t *bit_offsets, double *values_out, size_t n_fields, uint16_t *sequence_count_out)
{
    ccsds_field_t fields[ADCS_PACKETS_MAX_FIELDS];
    ccsds_codec_t codec;
    ADCS_CodecStatus_t build_status = ADCS_BuildCcsdsCodec(meta, bit_offsets, n_fields, fields, &codec);
    if (build_status != ADCS_CODEC_OK) {
        return build_status;
    }
    ccsds_status_t st = ccsds_decode_packet(&codec, buf, len, values_out);
    if (st != CCSDS_OK) {
        return ADCS_MapCcsdsStatus(st);
    }
    if (sequence_count_out != NULL) {
        uint16_t seq;
        /* `ccsds_decode_packet` above already proved `len >= CCSDS_PRIMARY_HEADER_LEN` (any
         * shorter buffer fails it first with CCSDS_ERR_PACKET_TOO_SHORT, returned above), so
         * this cannot re-fail on that same ground. */
        ccsds_status_t seq_status = ccsds_decode_sequence_count(buf, len, &seq);
        if (seq_status != CCSDS_OK) {
            return ADCS_MapCcsdsStatus(seq_status);
        }
        *sequence_count_out = seq;
    }
    return ADCS_CODEC_OK;
}

/* -------------------------------------------------------------------------------------------
 * Star tracker (APID 200, telemetry, 32-byte user data: qx,qy,qz,qw).
 * ---------------------------------------------------------------------------------------- */

static const uint32_t ADCS_ST_BIT_OFFSETS[4] = {ADCS_ST_BITOFFSET_QX, ADCS_ST_BITOFFSET_QY, ADCS_ST_BITOFFSET_QZ, ADCS_ST_BITOFFSET_QW};
static const ADCS_PacketCodecMeta_t ADCS_ST_META = {ADCS_STARTRACKER_APID, ADCS_STARTRACKER_IS_COMMAND, ADCS_STARTRACKER_SECONDARY_HEADER_BYTES, ADCS_STARTRACKER_USER_DATA_BYTES};

ADCS_CodecStatus_t ADCS_DecodeStarTrackerPacket(const uint8_t *buf, size_t len, ADCS_StarTrackerMeas_t *out, uint16_t *sequence_count_out)
{
    double v[4];
    ADCS_CodecStatus_t st = ADCS_DecodePacket(&ADCS_ST_META, buf, len, ADCS_ST_BIT_OFFSETS, v, 4, sequence_count_out);
    if (st != ADCS_CODEC_OK) {
        return st;
    }
    out->qx = v[0];
    out->qy = v[1];
    out->qz = v[2];
    out->qw = v[3];
    return ADCS_CODEC_OK;
}

ADCS_CodecStatus_t ADCS_EncodeStarTrackerPacket(uint16_t sequence_count, const ADCS_StarTrackerMeas_t *meas, uint8_t *out_buf, size_t out_buf_len, size_t *out_len)
{
    double v[4] = {meas->qx, meas->qy, meas->qz, meas->qw};
    return ADCS_EncodePacket(&ADCS_ST_META, sequence_count, NULL, ADCS_ST_BIT_OFFSETS, v, 4, out_buf, out_buf_len, out_len);
}

/* -------------------------------------------------------------------------------------------
 * IMU (APID 201, telemetry, 48-byte user data: wx,wy,wz,ax,ay,az).
 * ---------------------------------------------------------------------------------------- */

static const uint32_t ADCS_IMU_BIT_OFFSETS[6] = {ADCS_IMU_BITOFFSET_WX, ADCS_IMU_BITOFFSET_WY, ADCS_IMU_BITOFFSET_WZ, ADCS_IMU_BITOFFSET_AX, ADCS_IMU_BITOFFSET_AY, ADCS_IMU_BITOFFSET_AZ};
static const ADCS_PacketCodecMeta_t ADCS_IMU_META = {ADCS_IMU_APID, ADCS_IMU_IS_COMMAND, ADCS_IMU_SECONDARY_HEADER_BYTES, ADCS_IMU_USER_DATA_BYTES};

ADCS_CodecStatus_t ADCS_DecodeImuPacket(const uint8_t *buf, size_t len, ADCS_ImuMeas_t *out, uint16_t *sequence_count_out)
{
    double v[6];
    ADCS_CodecStatus_t st = ADCS_DecodePacket(&ADCS_IMU_META, buf, len, ADCS_IMU_BIT_OFFSETS, v, 6, sequence_count_out);
    if (st != ADCS_CODEC_OK) {
        return st;
    }
    out->wx = v[0];
    out->wy = v[1];
    out->wz = v[2];
    out->ax = v[3];
    out->ay = v[4];
    out->az = v[5];
    return ADCS_CODEC_OK;
}

ADCS_CodecStatus_t ADCS_EncodeImuPacket(uint16_t sequence_count, const ADCS_ImuMeas_t *meas, uint8_t *out_buf, size_t out_buf_len, size_t *out_len)
{
    double v[6] = {meas->wx, meas->wy, meas->wz, meas->ax, meas->ay, meas->az};
    return ADCS_EncodePacket(&ADCS_IMU_META, sequence_count, NULL, ADCS_IMU_BIT_OFFSETS, v, 6, out_buf, out_buf_len, out_len);
}

/* -------------------------------------------------------------------------------------------
 * Wheel torque command (APID 300, command, 24-byte user data: tau_1,tau_2,tau_3).
 * ---------------------------------------------------------------------------------------- */

static const uint32_t ADCS_WT_BIT_OFFSETS[3] = {ADCS_WT_BITOFFSET_TAU1, ADCS_WT_BITOFFSET_TAU2, ADCS_WT_BITOFFSET_TAU3};
static const ADCS_PacketCodecMeta_t ADCS_WT_META = {ADCS_WHEEL_TORQUE_APID, ADCS_WHEEL_TORQUE_IS_COMMAND, ADCS_WHEEL_TORQUE_SECONDARY_HEADER_BYTES, ADCS_WHEEL_TORQUE_USER_DATA_BYTES};

ADCS_CodecStatus_t ADCS_DecodeWheelTorquePacket(const uint8_t *buf, size_t len, ADCS_WheelTorqueCmd_t *out, uint16_t *sequence_count_out)
{
    double v[3];
    ADCS_CodecStatus_t st = ADCS_DecodePacket(&ADCS_WT_META, buf, len, ADCS_WT_BIT_OFFSETS, v, 3, sequence_count_out);
    if (st != ADCS_CODEC_OK) {
        return st;
    }
    out->tau_1 = v[0];
    out->tau_2 = v[1];
    out->tau_3 = v[2];
    return ADCS_CODEC_OK;
}

ADCS_CodecStatus_t ADCS_EncodeWheelTorquePacket(uint16_t sequence_count, const ADCS_WheelTorqueCmd_t *cmd, uint8_t *out_buf, size_t out_buf_len, size_t *out_len)
{
    double v[3] = {cmd->tau_1, cmd->tau_2, cmd->tau_3};
    return ADCS_EncodePacket(&ADCS_WT_META, sequence_count, NULL, ADCS_WT_BIT_OFFSETS, v, 3, out_buf, out_buf_len, out_len);
}
