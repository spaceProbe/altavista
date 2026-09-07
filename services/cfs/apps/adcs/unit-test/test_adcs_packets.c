/*
 * test_adcs_packets.c -- host-side unit tests for adcs_packets.c, the CCSDS codec ported from
 * crates/av-kernel/src/codec.rs's PacketCodec contract (M22.3/question 149) to the three
 * concrete packets this app's YAML fixtures declare (APID 200/201/300 --
 * drms/demo_attitude_control_startracker.system.yaml, `_imu.system.yaml`,
 * `_controller.system.yaml`/`_truth.system.yaml`).
 *
 * The hand-computed byte sequences below are derived directly from the CCSDS 133.0-B-2 bit
 * layout and the declared YAML fields, NOT by running this module's own encoder and trusting
 * it (a self-consistent-but-wrong layout would round-trip through its own encode/decode pair
 * and never get caught that way -- exactly the weakness M22.3 review found and closed, per
 * this app's own brief). IEEE-754 binary64 bit patterns for small integers/halves (1.0, 2.0,
 * 3.0, 4.0, -1.0, 0.5) are well-known constants, reproduced in the comments alongside each use
 * so the derivation is checkable by eye without running anything.
 */
#include "adcs_test_framework.h"
#include "adcs_packets.h"
#include <string.h>

/* ---------------------------------------------------------------------------------------
 * Star tracker (APID 200, telemetry): hand-derived packet.
 *
 * APID 200 = 0xC8 (fits in 8 bits, so apid[10:8] = 0b000).
 *   byte0 = type(0, telemetry)<<4 | sec_hdr_flag(0)<<3 | apid[10:8](000) = 0x00
 *   byte1 = apid[7:0] = 0xC8
 * sequence_count = 7 = 0b00000000000111 (14 bits): top6=000000, bottom8=0x07
 *   byte2 = seq_flags(0b11)<<6 | top6(000000) = 0xC0
 *   byte3 = 0x07
 * packet_data_length = secondary_header_bytes(0) + user_data_bytes(32) - 1 = 31 = 0x001F
 *   byte4 = 0x00, byte5 = 0x1F
 * user data: qx=1.0, qy=2.0, qz=3.0, qw=4.0, each FLOAT64 big-endian:
 *   1.0 = 0x3FF0000000000000, 2.0 = 0x4000000000000000,
 *   3.0 = 0x4008000000000000, 4.0 = 0x4010000000000000
 * ------------------------------------------------------------------------------------- */
static const uint8_t ST_HAND_PACKET[ADCS_STARTRACKER_PACKET_LEN] = {
    0x00, 0xC8, 0xC0, 0x07, 0x00, 0x1F,
    0x3F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* qx = 1.0 */
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* qy = 2.0 */
    0x40, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* qz = 3.0 */
    0x40, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* qw = 4.0 */
};

ADCS_TEST(star_tracker_decode_matches_the_hand_computed_ccsds_layout)
{
    ADCS_StarTrackerMeas_t meas;
    uint16_t seq = 0;
    ADCS_CodecStatus_t st = ADCS_DecodeStarTrackerPacket(ST_HAND_PACKET, sizeof(ST_HAND_PACKET), &meas, &seq);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(seq, 7);
    ADCS_CHECK_NEAR(meas.qx, 1.0, 0.0);
    ADCS_CHECK_NEAR(meas.qy, 2.0, 0.0);
    ADCS_CHECK_NEAR(meas.qz, 3.0, 0.0);
    ADCS_CHECK_NEAR(meas.qw, 4.0, 0.0);
}

ADCS_TEST(star_tracker_encode_reproduces_the_hand_computed_bytes)
{
    ADCS_StarTrackerMeas_t meas = {1.0, 2.0, 3.0, 4.0};
    uint8_t out[ADCS_STARTRACKER_PACKET_LEN];
    size_t out_len = 0;
    ADCS_CodecStatus_t st = ADCS_EncodeStarTrackerPacket(7, &meas, out, sizeof(out), &out_len);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(out_len, ADCS_STARTRACKER_PACKET_LEN);
    ADCS_CHECK_BYTES_EQ(out, ST_HAND_PACKET, ADCS_STARTRACKER_PACKET_LEN);
}

ADCS_TEST(star_tracker_type_bit_is_clear_for_telemetry)
{
    /* byte0 bit 4 (0x10) is the CCSDS type bit; 0 = telemetry (this packet), 1 = telecommand
     * (the wheel-torque command packet, checked separately below). */
    ADCS_CHECK_EQ_INT(ST_HAND_PACKET[0] & 0x10, 0x00);
}

/* ---------------------------------------------------------------------------------------
 * Wheel torque command (APID 300, telecommand): hand-derived packet -- what this app actually
 * transmits, so pinning its exact bytes matters as much as decode.
 *
 * APID 300 = 0x12C: apid[10:8] = 0b001, apid[7:0] = 0x2C.
 *   byte0 = type(1, command)<<4 | sec_hdr_flag(0)<<3 | apid[10:8](001) = 0x11
 *   byte1 = 0x2C
 * sequence_count = 0: byte2 = 0b11000000 = 0xC0, byte3 = 0x00
 * packet_data_length = 0 + 24 - 1 = 23 = 0x0017: byte4 = 0x00, byte5 = 0x17
 * user data: tau_1=1.0 (0x3FF0000000000000), tau_2=-1.0 (0xBFF0000000000000),
 *            tau_3=0.5 (0x3FE0000000000000)
 * ------------------------------------------------------------------------------------- */
static const uint8_t WT_HAND_PACKET[ADCS_WHEEL_TORQUE_PACKET_LEN] = {
    0x11, 0x2C, 0xC0, 0x00, 0x00, 0x17,
    0x3F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* tau_1 = 1.0 */
    0xBF, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* tau_2 = -1.0 */
    0x3F, 0xE0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* tau_3 = 0.5 */
};

ADCS_TEST(wheel_torque_encode_reproduces_the_hand_computed_bytes)
{
    ADCS_WheelTorqueCmd_t cmd = {1.0, -1.0, 0.5};
    uint8_t out[ADCS_WHEEL_TORQUE_PACKET_LEN];
    size_t out_len = 0;
    ADCS_CodecStatus_t st = ADCS_EncodeWheelTorquePacket(0, &cmd, out, sizeof(out), &out_len);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(out_len, ADCS_WHEEL_TORQUE_PACKET_LEN);
    ADCS_CHECK_BYTES_EQ(out, WT_HAND_PACKET, ADCS_WHEEL_TORQUE_PACKET_LEN);
}

ADCS_TEST(wheel_torque_decode_matches_the_hand_computed_ccsds_layout)
{
    ADCS_WheelTorqueCmd_t cmd;
    uint16_t seq = 123; /* poison, must be overwritten */
    ADCS_CodecStatus_t st = ADCS_DecodeWheelTorquePacket(WT_HAND_PACKET, sizeof(WT_HAND_PACKET), &cmd, &seq);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(seq, 0);
    ADCS_CHECK_NEAR(cmd.tau_1, 1.0, 0.0);
    ADCS_CHECK_NEAR(cmd.tau_2, -1.0, 0.0);
    ADCS_CHECK_NEAR(cmd.tau_3, 0.5, 0.0);
}

ADCS_TEST(wheel_torque_type_bit_is_set_for_a_telecommand)
{
    ADCS_CHECK_EQ_INT(WT_HAND_PACKET[0] & 0x10, 0x10);
}

/* ---------------------------------------------------------------------------------------
 * The packet-data-length off-by-one this module must NOT reintroduce (question 149 / this
 * app's own brief): CCSDS 133.0-B 4.1.2.5 defines the length field as
 * `secondary_header_bytes + user_data_bytes - 1`, not `user_data_bytes - 1` alone. The three
 * real packets above all declare secondary_header_bytes=0, so they cannot distinguish the two
 * formulas -- this synthetic codec (secondary_header_bytes=2) is the case that does, mirroring
 * crates/av-kernel/src/codec.rs's own
 * `packet_data_length_counts_the_secondary_header_pinned_by_hand` test (same apid=6,
 * is_command=true, secondary header {0xAA, 0xBB}).
 *
 * apid=6: apid[10:8]=0b000, apid[7:0]=0x06.
 *   byte0 = type(1)<<4 | sec_hdr_flag(1, since secondary_header_bytes=2>0)<<3 | 000 = 0x18
 *   byte1 = 0x06
 * sequence_count=0: byte2=0xC0, byte3=0x00
 * packet_data_length = secondary_header_bytes(2) + user_data_bytes(8) - 1 = 9 = 0x0009
 *   byte4=0x00, byte5=0x09  <- NOT 0x07 (which is what `user_data_bytes - 1` alone would give)
 * secondary header: 0xAA 0xBB (carried verbatim)
 * user data: one FLOAT64 field "v" at bit_offset 0, value = 2.0 (0x4000000000000000)
 * ------------------------------------------------------------------------------------- */
static const uint8_t SEC_HDR_HAND_PACKET[16] = {
    0x18, 0x06, 0xC0, 0x00, 0x00, 0x09,
    0xAA, 0xBB,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* v = 2.0 */
};

ADCS_TEST(packet_data_length_counts_the_secondary_header_pinned_by_hand)
{
    ADCS_PacketCodecMeta_t meta = {6, true, 2, 8};
    uint32_t bit_offsets[1] = {0};
    double values[1] = {2.0};
    uint8_t secondary_header[2] = {0xAA, 0xBB};
    uint8_t out[16];
    size_t out_len = 0;

    ADCS_CodecStatus_t st = ADCS_EncodePacket(&meta, 0, secondary_header, bit_offsets, values, 1, out, sizeof(out), &out_len);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(out_len, 16);
    ADCS_CHECK_EQ_INT(out[4], 0x00);
    ADCS_CHECK_EQ_INT(out[5], 0x09); /* (2 + 8) - 1 = 9, not (8) - 1 = 7 */
    ADCS_CHECK_BYTES_EQ(out, SEC_HDR_HAND_PACKET, 16);

    double decoded[1];
    uint16_t seq = 0;
    ADCS_CodecStatus_t dst = ADCS_DecodePacket(&meta, out, out_len, bit_offsets, decoded, 1, &seq);
    ADCS_CHECK_EQ_INT(dst, ADCS_CODEC_OK);
    ADCS_CHECK_NEAR(decoded[0], 2.0, 0.0);
}

ADCS_TEST(a_packet_with_the_off_by_one_forgotten_is_a_typed_length_mismatch)
{
    /* Same packet as above, but byte5 mutated to 0x07 -- the "forgot to add
     * secondary_header_bytes before subtracting 1" mistake (a decoder reading the length field
     * as `user_data_bytes - 1` directly, ignoring the secondary header, would accept this). */
    uint8_t bad[16];
    memcpy(bad, SEC_HDR_HAND_PACKET, sizeof(bad));
    bad[5] = 0x07;

    ADCS_PacketCodecMeta_t meta = {6, true, 2, 8};
    uint32_t bit_offsets[1] = {0};
    double decoded[1];
    ADCS_CodecStatus_t st = ADCS_DecodePacket(&meta, bad, sizeof(bad), bit_offsets, decoded, 1, NULL);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_ERR_LENGTH_FIELD_MISMATCH);
}

/* ---------------------------------------------------------------------------------------
 * Typed faults -- question 149's "never a silent drop" rule: every malformed shape below must
 * be a distinct, visible status, never a zero-filled reading or a panic/crash.
 * ------------------------------------------------------------------------------------- */

ADCS_TEST(an_unknown_apid_is_a_typed_error_not_a_silent_drop)
{
    /* IMU decoder fed the star-tracker packet (APID 200, not 201). */
    ADCS_ImuMeas_t meas;
    memset(&meas, 0xCD, sizeof(meas)); /* poison so a silent zero-fill would be visible too */
    ADCS_CodecStatus_t st = ADCS_DecodeImuPacket(ST_HAND_PACKET, sizeof(ST_HAND_PACKET), &meas, NULL);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_ERR_WRONG_APID);
    /* Untouched on failure -- still poisoned, not zero-filled. */
    uint8_t expect_poison[sizeof(meas)];
    memset(expect_poison, 0xCD, sizeof(expect_poison));
    ADCS_CHECK_BYTES_EQ(&meas, expect_poison, sizeof(meas));
}

ADCS_TEST(a_packet_shorter_than_the_primary_header_is_a_typed_error)
{
    uint8_t tiny[2] = {0x00, 0xC8};
    ADCS_StarTrackerMeas_t meas;
    ADCS_CodecStatus_t st = ADCS_DecodeStarTrackerPacket(tiny, sizeof(tiny), &meas, NULL);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_ERR_TOO_SHORT_FOR_HEADER);
}

ADCS_TEST(a_packet_with_the_wrong_total_length_is_a_typed_error)
{
    /* Header's own length field is self-consistent (still says 31 -> 32 user data bytes), but
     * the buffer handed in is one byte short of the declared 38 -- must be refused even though
     * the internal length field alone looked fine. */
    uint8_t truncated[ADCS_STARTRACKER_PACKET_LEN - 1];
    memcpy(truncated, ST_HAND_PACKET, sizeof(truncated));
    ADCS_StarTrackerMeas_t meas;
    ADCS_CodecStatus_t st = ADCS_DecodeStarTrackerPacket(truncated, sizeof(truncated), &meas, NULL);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_ERR_TOTAL_LENGTH_MISMATCH);
}

ADCS_TEST(an_output_buffer_too_small_to_encode_into_is_a_typed_error)
{
    ADCS_StarTrackerMeas_t meas = {1.0, 2.0, 3.0, 4.0};
    uint8_t out[ADCS_STARTRACKER_PACKET_LEN - 1];
    size_t out_len = 0;
    ADCS_CodecStatus_t st = ADCS_EncodeStarTrackerPacket(0, &meas, out, sizeof(out), &out_len);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_ERR_OUTPUT_BUFFER_TOO_SMALL);
}

ADCS_TEST(a_sequence_count_past_14_bits_is_a_typed_error)
{
    ADCS_StarTrackerMeas_t meas = {1.0, 2.0, 3.0, 4.0};
    uint8_t out[ADCS_STARTRACKER_PACKET_LEN];
    size_t out_len = 0;
    ADCS_CodecStatus_t st = ADCS_EncodeStarTrackerPacket(0x4000 /* 16384, one past the 14-bit max 16383 */, &meas, out, sizeof(out), &out_len);
    ADCS_CHECK_EQ_INT(st, ADCS_CODEC_ERR_SEQUENCE_COUNT_OUT_OF_RANGE);
}

/* ---------------------------------------------------------------------------------------
 * Round trip for the IMU packet (all six fields, distinct values per field so a field-order or
 * bit-offset bug would show up as a mismatched value, not just a mismatched sum) -- a
 * supplementary check, not a substitute for the hand-computed tests above.
 * ------------------------------------------------------------------------------------- */

ADCS_TEST(imu_packet_round_trips_all_six_fields)
{
    ADCS_ImuMeas_t meas = {0.011, -0.022, 0.033, 1.1, -2.2, 3.3};
    uint8_t buf[ADCS_IMU_PACKET_LEN];
    size_t out_len = 0;
    ADCS_CodecStatus_t est = ADCS_EncodeImuPacket(42, &meas, buf, sizeof(buf), &out_len);
    ADCS_CHECK_EQ_INT(est, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(out_len, ADCS_IMU_PACKET_LEN);

    ADCS_ImuMeas_t decoded;
    uint16_t seq = 0;
    ADCS_CodecStatus_t dst = ADCS_DecodeImuPacket(buf, sizeof(buf), &decoded, &seq);
    ADCS_CHECK_EQ_INT(dst, ADCS_CODEC_OK);
    ADCS_CHECK_EQ_INT(seq, 42);
    ADCS_CHECK_NEAR(decoded.wx, meas.wx, 0.0);
    ADCS_CHECK_NEAR(decoded.wy, meas.wy, 0.0);
    ADCS_CHECK_NEAR(decoded.wz, meas.wz, 0.0);
    ADCS_CHECK_NEAR(decoded.ax, meas.ax, 0.0);
    ADCS_CHECK_NEAR(decoded.ay, meas.ay, 0.0);
    ADCS_CHECK_NEAR(decoded.az, meas.az, 0.0);
}

int main(void)
{
    printf("test_adcs_packets:\n");
    ADCS_RUN_TEST(star_tracker_decode_matches_the_hand_computed_ccsds_layout);
    ADCS_RUN_TEST(star_tracker_encode_reproduces_the_hand_computed_bytes);
    ADCS_RUN_TEST(star_tracker_type_bit_is_clear_for_telemetry);
    ADCS_RUN_TEST(wheel_torque_encode_reproduces_the_hand_computed_bytes);
    ADCS_RUN_TEST(wheel_torque_decode_matches_the_hand_computed_ccsds_layout);
    ADCS_RUN_TEST(wheel_torque_type_bit_is_set_for_a_telecommand);
    ADCS_RUN_TEST(packet_data_length_counts_the_secondary_header_pinned_by_hand);
    ADCS_RUN_TEST(a_packet_with_the_off_by_one_forgotten_is_a_typed_length_mismatch);
    ADCS_RUN_TEST(an_unknown_apid_is_a_typed_error_not_a_silent_drop);
    ADCS_RUN_TEST(a_packet_shorter_than_the_primary_header_is_a_typed_error);
    ADCS_RUN_TEST(a_packet_with_the_wrong_total_length_is_a_typed_error);
    ADCS_RUN_TEST(an_output_buffer_too_small_to_encode_into_is_a_typed_error);
    ADCS_RUN_TEST(a_sequence_count_past_14_bits_is_a_typed_error);
    ADCS_RUN_TEST(imu_packet_round_trips_all_six_fields);
    ADCS_TEST_SUMMARY_AND_EXIT();
}
