/*
 * adcs_packets.h -- CCSDS Space Packet codec for the reference ADCS app (M23.3).
 *
 * M23.4 reconciliation: `ADCS_EncodePacket`/`ADCS_DecodePacket` (this header's own generic
 * per-codec engine, below) are now a thin wrapper over
 * `services/cfs/apps/shared/ccsds/inc/ccsds_codec.h`'s `ccsds_encode_packet`/
 * `ccsds_decode_packet` -- the same shared implementation `services/cfs/apps/io_lockstep` links
 * against -- instead of this file's own from-scratch bitfield reader/writer. Two independent C
 * ports of the same CCSDS bit-packing existed after M23.2/M23.3 (this file's own
 * `ADCS_ReadBitfieldU64`/`ADCS_WriteBitfieldU64`, and io_lockstep's `ccsds_codec.c`), each
 * checked against real Rust golden bytes but never against each other -- exactly the drift risk
 * a duplicated implementation invites. See `ccsds_codec.h`'s own doc comment for the full
 * account. Every type/function declared below (`ADCS_StarTrackerMeas_t`,
 * `ADCS_DecodeStarTrackerPacket`, etc.) keeps its exact pre-existing signature and behaviour --
 * this is an internal-implementation change only, which is why
 * `services/cfs/apps/adcs/unit-test/test_adcs_packets.c` (M23.3's own 21 tests) needed no
 * changes to keep passing.
 *
 * Ports crates/av-kernel/src/codec.rs's PacketCodec contract to C, field-for-field, for the
 * three packets docs/open-questions.md question 149 declares (PacketCodec/PacketField in
 * proto/altavista/v1/packet.proto) and the M22.4 fixture instantiates:
 *
 *   drms/demo_attitude_control_startracker.system.yaml `st_meas_codec` (APID 200, telemetry)
 *   drms/demo_attitude_control_imu.system.yaml         `imu_meas_codec` (APID 201, telemetry)
 *   drms/demo_attitude_control_controller.system.yaml/_truth.system.yaml
 *       `wheel_torque_codec` (APID 300, command)
 *
 * This module (adcs_packets.h/.c) has NO cFE/OSAL dependency -- it is plain C99 operating on
 * byte buffers, deliberately, so it can be unit-tested on the host (services/cfs/apps/adcs/
 * unit-test/) whether or not third_party/cfs has landed yet (M23.2's scope, not this one's).
 * services/cfs/apps/adcs/fsw/src/adcs_app.c is the (separate, cFE-dependent) glue that moves
 * bytes between the cFS software bus and these functions.
 *
 * ## Wire format (CCSDS 133.0-B-2, matching crate::codec's own module doc comment exactly)
 *
 * Primary header, 6 bytes, big-endian:
 *   byte 0: [version:3][type:1][sec_hdr_flag:1][apid[10:8]:3]
 *   byte 1: [apid[7:0]:8]
 *   byte 2: [sequence_flags:2][sequence_count[13:8]:6]
 *   byte 3: [sequence_count[7:0]:8]
 *   byte 4: [packet_data_length[15:8]:8]
 *   byte 5: [packet_data_length[7:0]:8]
 *
 * `version` is always 0. `type` is 1 for a command (telecommand), 0 for telemetry -- matches
 * `PacketCodec.is_command`. `sec_hdr_flag` is 1 iff `secondary_header_bytes > 0` (always 0 for
 * all three packets here, all three declare `secondary_header_bytes: 0`). `sequence_flags` is
 * always `0b11` ("unsegmented") -- one whole space packet per port message, no segmentation.
 *
 * Packet data length (CCSDS 133.0-B 4.1.2.5): `secondary_header_bytes + user_data_bytes - 1`
 * -- counts the octets of the Packet Data Field (secondary header + user data), minus one. An
 * earlier M22.3 draft computed `user_data_bytes - 1` alone, which silently drops the secondary
 * header from the count; that was a manager briefing error, corrected in review before any
 * flight software met it (crate::codec's own module doc comment tells the same story). This
 * module implements the corrected formula from the start and pins it with a test using a
 * nonzero secondary_header_bytes case (services/cfs/apps/adcs/unit-test/test_adcs_packets.c),
 * even though none of the three real packets below declare a secondary header themselves --
 * the two formulas coincide exactly when secondary_header_bytes == 0, so a packet-shaped test
 * with only these three real codecs could never distinguish the correct formula from the
 * off-by-one one.
 *
 * Every field in all three packets is FLOAT64 (IEEE-754 binary64), big-endian, byte-aligned,
 * scale=1.0/offset=0.0 (i.e. the wire value IS the engineering value, no conversion) -- so this
 * header only implements the FLOAT64 field type, plus a general bit-level reader/writer
 * ([`ADCS_ReadBitfieldU64`]/[`ADCS_WriteBitfieldU64`] in the .c file) mirroring
 * crate::codec::read_bitfield_u64/write_bitfield_u64's own MSB-first bit numbering exactly, so
 * a future non-byte-aligned or non-FLOAT64 field would decode identically to the Rust
 * implementation without a second, independently-invented bit convention.
 */
#ifndef ADCS_PACKETS_H
#define ADCS_PACKETS_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* CCSDS 133.0-B-2 section 4.1.3: the primary header is always exactly 6 octets. */
#define ADCS_CCSDS_PRIMARY_HEADER_LEN 6u

/* The 11-bit APID field's maximum value (2^11 - 1). */
#define ADCS_CCSDS_MAX_APID 0x7FFu
/* The 14-bit sequence count field's maximum value (2^14 - 1). */
#define ADCS_CCSDS_MAX_SEQUENCE_COUNT 0x3FFFu

/* -------------------------------------------------------------------------------------------
 * Declared APIDs and user-data sizes -- field for field against the YAML `packet_codecs:`
 * blocks named in this file's own header comment. Do not change without updating the YAML (and
 * vice versa): the same APID has to mean the same layout on both the Rust kernel side and this
 * cFS app for a closed loop to actually decode what the other side encoded.
 * ---------------------------------------------------------------------------------------- */

#define ADCS_STARTRACKER_APID 200u
#define ADCS_STARTRACKER_IS_COMMAND false
#define ADCS_STARTRACKER_SECONDARY_HEADER_BYTES 0u
#define ADCS_STARTRACKER_USER_DATA_BYTES 32u /* qx,qy,qz,qw: 4 * FLOAT64 */
#define ADCS_STARTRACKER_PACKET_LEN (ADCS_CCSDS_PRIMARY_HEADER_LEN + ADCS_STARTRACKER_SECONDARY_HEADER_BYTES + ADCS_STARTRACKER_USER_DATA_BYTES)

#define ADCS_IMU_APID 201u
#define ADCS_IMU_IS_COMMAND false
#define ADCS_IMU_SECONDARY_HEADER_BYTES 0u
#define ADCS_IMU_USER_DATA_BYTES 48u /* wx,wy,wz,ax,ay,az: 6 * FLOAT64 */
#define ADCS_IMU_PACKET_LEN (ADCS_CCSDS_PRIMARY_HEADER_LEN + ADCS_IMU_SECONDARY_HEADER_BYTES + ADCS_IMU_USER_DATA_BYTES)

#define ADCS_WHEEL_TORQUE_APID 300u
#define ADCS_WHEEL_TORQUE_IS_COMMAND true
#define ADCS_WHEEL_TORQUE_SECONDARY_HEADER_BYTES 0u
#define ADCS_WHEEL_TORQUE_USER_DATA_BYTES 24u /* tau_1,tau_2,tau_3: 3 * FLOAT64 */
#define ADCS_WHEEL_TORQUE_PACKET_LEN (ADCS_CCSDS_PRIMARY_HEADER_LEN + ADCS_WHEEL_TORQUE_SECONDARY_HEADER_BYTES + ADCS_WHEEL_TORQUE_USER_DATA_BYTES)

/* Bit offsets within user data, matching the YAML `bit_offset:` values exactly (all FLOAT64,
 * bit_width 64, byte-aligned). */
#define ADCS_ST_BITOFFSET_QX 0u
#define ADCS_ST_BITOFFSET_QY 64u
#define ADCS_ST_BITOFFSET_QZ 128u
#define ADCS_ST_BITOFFSET_QW 192u

#define ADCS_IMU_BITOFFSET_WX 0u
#define ADCS_IMU_BITOFFSET_WY 64u
#define ADCS_IMU_BITOFFSET_WZ 128u
#define ADCS_IMU_BITOFFSET_AX 192u
#define ADCS_IMU_BITOFFSET_AY 256u
#define ADCS_IMU_BITOFFSET_AZ 320u

#define ADCS_WT_BITOFFSET_TAU1 0u
#define ADCS_WT_BITOFFSET_TAU2 64u
#define ADCS_WT_BITOFFSET_TAU3 128u

/* -------------------------------------------------------------------------------------------
 * Decoded packet payloads.
 * ---------------------------------------------------------------------------------------- */

/** Star tracker attitude measurement, scalar-last quaternion [qx, qy, qz, qw]. */
typedef struct {
    double qx;
    double qy;
    double qz;
    double qw;
} ADCS_StarTrackerMeas_t;

/** IMU rate + specific-force measurement. */
typedef struct {
    double wx;
    double wy;
    double wz;
    double ax;
    double ay;
    double az;
} ADCS_ImuMeas_t;

/** Reaction wheel torque command, wheel k aligned with body axis k (k = 1,2,3 = x,y,z). */
typedef struct {
    double tau_1;
    double tau_2;
    double tau_3;
} ADCS_WheelTorqueCmd_t;

/* -------------------------------------------------------------------------------------------
 * Typed status -- question 149's "never a silent drop" rule: every way a packet can fail to
 * match its declared codec is a distinct, visible status, never a dropped or zero-filled
 * reading. On any non-OK status the caller MUST NOT use the output struct -- these functions
 * leave it untouched (not zero-filled) on failure so a caller that forgets to check the status
 * is not handed a plausible-looking-but-fake all-zero reading.
 * ---------------------------------------------------------------------------------------- */
typedef enum {
    ADCS_CODEC_OK = 0,
    /** Buffer shorter than the 6-byte primary header -- nothing to even read an APID from. */
    ADCS_CODEC_ERR_TOO_SHORT_FOR_HEADER,
    /** The primary header's APID does not match the codec this function decodes. */
    ADCS_CODEC_ERR_WRONG_APID,
    /** The primary header's packet data length field disagrees with the declared codec's own
     *  secondary_header_bytes + user_data_bytes (converted back via `length_field + 1`). */
    ADCS_CODEC_ERR_LENGTH_FIELD_MISMATCH,
    /** The buffer's total length disagrees with primary header + secondary header + user data,
     *  checked independently of the length field above (catches a caller handing over the
     *  wrong number of bytes even when the header's own internal field was self-consistent). */
    ADCS_CODEC_ERR_TOTAL_LENGTH_MISMATCH,
    /** `ADCS_EncodePacket`'s output buffer is smaller than the packet it must produce. */
    ADCS_CODEC_ERR_OUTPUT_BUFFER_TOO_SMALL,
    /** `sequence_count` does not fit the CCSDS 14-bit sequence count field (0..=16383). */
    ADCS_CODEC_ERR_SEQUENCE_COUNT_OUT_OF_RANGE,
    /** M23.4: the shared `services/cfs/apps/shared/ccsds` codec
     *  (`ADCS_EncodePacket`/`ADCS_DecodePacket`'s own implementation since that reconciliation --
     *  see this header's own top comment) reported a `ccsds_status_t` none of the specific
     *  statuses above maps to (`CCSDS_ERR_APID_RANGE`/`_INVALID_USER_DATA_BYTES`/`_FIELD_EXTENT`/
     *  `_UNSUPPORTED_FIELD_TYPE`/`_VALUE_OUT_OF_RANGE`/`_TOO_MANY_FIELDS`). Every one of those is
     *  a codec-*table* defect (a malformed `ADCS_PacketCodecMeta_t`/field-offset array), not a
     *  malformed *packet* -- this app's own three fixed, compile-time-constant codec tables
     *  (star tracker/IMU/wheel-torque) never trigger it, so reaching this status is a programming
     *  error in this app's own code, not an expected runtime condition. Still a distinct, typed
     *  status rather than silently coerced into one of the packet-shaped statuses above (which
     *  would misdescribe the actual failure to a caller/log reader) or a panic. */
    ADCS_CODEC_ERR_INTERNAL,
} ADCS_CodecStatus_t;

/** Human-readable name for a status, for event/log messages -- never used to gate control
 *  flow (match on the enum itself for that), only for a message a human reads. */
const char *ADCS_CodecStatusName(ADCS_CodecStatus_t status);

/* -------------------------------------------------------------------------------------------
 * Generic codec engine -- mirrors crate::codec::PacketCodec/encode_packet/decode_packet's own
 * "one implementation of the header layout and field bit arithmetic" shape, parameterized by
 * secondary_header_bytes rather than hardcoding it to 0. The three real packets this app uses
 * all declare secondary_header_bytes=0, so ADCS_{Encode,Decode}{StarTracker,Imu,WheelTorque}
 * Packet below never exercise a nonzero value -- this generic entry point exists specifically
 * so services/cfs/apps/adcs/unit-test/test_adcs_packets.c can pin the CCSDS 133.0-B 4.1.2.5
 * packet-data-length formula (`secondary_header_bytes + user_data_bytes - 1`, not
 * `user_data_bytes - 1` alone -- see this header's own top comment) against a synthetic codec
 * with a nonzero secondary header, the one case that distinguishes the two formulas. Every
 * field is FLOAT64 (this module's only implemented field type -- see the top comment). */
typedef struct {
    uint16_t apid;
    bool is_command;
    uint32_t secondary_header_bytes;
    uint32_t user_data_bytes;
} ADCS_PacketCodecMeta_t;

/** Encode `n_fields` FLOAT64 values (`values[i]` at `bit_offsets[i]`, big-endian, scale=1/
 *  offset=0) into one CCSDS space packet per `meta`. `secondary_header` must be exactly
 *  `meta->secondary_header_bytes` long (may be NULL iff that is 0) and is copied verbatim
 *  between the primary header and the user data. */
ADCS_CodecStatus_t ADCS_EncodePacket(const ADCS_PacketCodecMeta_t *meta, uint16_t sequence_count, const uint8_t *secondary_header, const uint32_t *bit_offsets, const double *values, size_t n_fields, uint8_t *out_buf, size_t out_buf_len, size_t *out_len);

/** Decode `n_fields` FLOAT64 values out of one CCSDS space packet per `meta`. `meta->apid`
 *  must match the packet's own header APID (else [`ADCS_CODEC_ERR_WRONG_APID`]). On any
 *  non-OK status `values_out`/`sequence_count_out` are left untouched. */
ADCS_CodecStatus_t ADCS_DecodePacket(const ADCS_PacketCodecMeta_t *meta, const uint8_t *buf, size_t len, const uint32_t *bit_offsets, double *values_out, size_t n_fields, uint16_t *sequence_count_out);

/* -------------------------------------------------------------------------------------------
 * Decode: star tracker / IMU (telemetry, inbound to this app).
 * ---------------------------------------------------------------------------------------- */

/** Decode one star tracker measurement packet. `buf`/`len` is the full CCSDS space packet
 *  (primary header + 32-byte user data, 38 bytes total for this codec). On success, `*out`
 *  is filled and `*sequence_count_out` (if non-NULL) carries the header's 14-bit sequence
 *  count. On any non-OK status, `*out` is left untouched. */
ADCS_CodecStatus_t ADCS_DecodeStarTrackerPacket(const uint8_t *buf, size_t len, ADCS_StarTrackerMeas_t *out, uint16_t *sequence_count_out);

/** Decode one IMU measurement packet (54 bytes total for this codec). Same contract as
 *  [`ADCS_DecodeStarTrackerPacket`]. */
ADCS_CodecStatus_t ADCS_DecodeImuPacket(const uint8_t *buf, size_t len, ADCS_ImuMeas_t *out, uint16_t *sequence_count_out);

/** Decode one wheel-torque command packet (30 bytes total for this codec) -- exposed mainly
 *  for this module's own encode/decode symmetry tests; the ADCS app itself only ever encodes
 *  this packet (the plant/actuator side decodes it, which is out of this app's scope). */
ADCS_CodecStatus_t ADCS_DecodeWheelTorquePacket(const uint8_t *buf, size_t len, ADCS_WheelTorqueCmd_t *out, uint16_t *sequence_count_out);

/* -------------------------------------------------------------------------------------------
 * Encode: wheel-torque command (outbound from this app).
 * ---------------------------------------------------------------------------------------- */

/** Encode one wheel-torque command packet into `out_buf` (must be at least
 *  ADCS_WHEEL_TORQUE_PACKET_LEN bytes). `*out_len` receives the number of bytes written on
 *  success. `sequence_count` must fit the CCSDS 14-bit field (0..=16383). */
ADCS_CodecStatus_t ADCS_EncodeWheelTorquePacket(uint16_t sequence_count, const ADCS_WheelTorqueCmd_t *cmd, uint8_t *out_buf, size_t out_buf_len, size_t *out_len);

/* Encoders for the two telemetry packets are provided too (not used by the flight app itself,
 * which never originates star tracker/IMU packets -- but needed so this module's own unit
 * tests can build a valid on-wire packet to feed the decoder without hand-writing every test
 * fixture byte by byte, mirroring crate::codec's own test helpers). */
ADCS_CodecStatus_t ADCS_EncodeStarTrackerPacket(uint16_t sequence_count, const ADCS_StarTrackerMeas_t *meas, uint8_t *out_buf, size_t out_buf_len, size_t *out_len);
ADCS_CodecStatus_t ADCS_EncodeImuPacket(uint16_t sequence_count, const ADCS_ImuMeas_t *meas, uint8_t *out_buf, size_t out_buf_len, size_t *out_len);

#ifdef __cplusplus
}
#endif

#endif /* ADCS_PACKETS_H */
