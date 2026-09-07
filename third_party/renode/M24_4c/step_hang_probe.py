#!/usr/bin/env python3
"""M24.4c diagnostic (not the fix itself): root-causing why the very first real DRM `STEP`
(carrying real star-tracker + IMU CCSDS packets, not the M24.4b smoke test's empty-input case)
never produces a reply, even after raising the bridge's own per-STEP `RunFor` floor from 100ms
to 3s and then 10s with no change in outcome (see third_party/renode/M24_4c_REPORT.md).

Reuses `renode_bridge.py`'s own `RenodeBridge`/`MonitorClient` machinery directly (imported, not
copied) so this probe drives the exact same boot/handshake/BIND path already proven live, then
takes over the STEP itself by hand: constructs a real `LockstepStepRequest` carrying two real,
correctly-encoded CCSDS packets (`ccsds_encode_packet`'s own byte layout, re-derived here from
`services/cfs/apps/shared/ccsds/src/ccsds_codec.c` and `services/cfs/apps/io_lockstep/fsw/src/
io_lockstep_port_table.c`, not assumed), injects it, and instead of a single fixed RunFor+read,
polls uart1's own TX-side register (`channel_sts`, `TEMPTY` bit) in small increments so the
question "is data ever sitting in the TX FIFO at all" is answered directly from hardware state,
not inferred from a client-side socket timeout.
"""
import struct
import sys
import time

sys.path.insert(0, "/Users/probe/code/AltaVista/third_party/renode/M24_4b")
import renode_bridge as rb  # noqa: E402
from altavista.pb.altavista.v1 import lockstep_pb2  # noqa: E402

UART1_CHANNEL_STS = 0xFF01002C
UART1_CONTROL = 0xFF010000
TEMPTY = 1 << 3
REMPTY = 1 << 1


def ccsds_packet(apid, is_command, user_data_bytes, field_values):
    """field_values: list of (byte_offset, float) pairs -- everything in this DRM's own port
    table is a FLOAT64 field at a multiple-of-8 bit_offset, so this only implements that case
    (matches ccsds_encode_packet's own FLOAT64 branch: memcpy the double's native bit pattern
    into a u64, then write_bitfield_u64 writes it MSB-first -- on a little-endian host that is
    exactly struct.pack('>d', value), re-derived in this docstring, not assumed)."""
    type_bit = 1 if is_command else 0
    sec_hdr_flag = 0
    out = bytearray(6 + user_data_bytes)
    out[0] = (0 << 5) | (type_bit << 4) | (sec_hdr_flag << 3) | ((apid >> 8) & 0x7)
    out[1] = apid & 0xFF
    out[2] = (0x3 << 6) | 0  # SEQUENCE_FLAGS_UNSEGMENTED, sequence_count=0
    out[3] = 0
    packet_data_length = user_data_bytes - 1
    out[4] = (packet_data_length >> 8) & 0xFF
    out[5] = packet_data_length & 0xFF
    for byte_offset, value in field_values:
        out[6 + byte_offset:6 + byte_offset + 8] = struct.pack(">d", value)
    return bytes(out)


def build_frame(frame_type, payload):
    length = 1 + len(payload)
    return struct.pack("<I", length) + bytes([frame_type]) + payload


def main():
    bridge = rb.RenodeBridge(
        renode_bin="/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode",
        platform="/Users/probe/code/AltaVista/third_party/renode/platforms/cpus/zynqmp.repl",
        elf="/Users/probe/code/AltaVista/third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe",
        monitor_port=15310,
        uart_port=15311,
        uart0_log="/Users/probe/code/AltaVista/third_party/renode/M24_4c/step_hang_probe_uart0.log",
    )
    bridge.start("/Users/probe/code/AltaVista/third_party/renode/M24_4c/step_hang_probe.renode_log.txt")
    step = bridge.wait_for_rxen()
    print(f"RXEN observed at step {step}")

    hello_payload = b"AVL1" + struct.pack("<H", 1)
    hello_raw = build_frame(0x01, hello_payload)
    bridge.deliver_to_guest(hello_raw, 0.2)
    _ft, _pl, guest_hello = bridge.guest_read_frame()
    print(f"HELLO reply: {guest_hello.hex()}")

    start_tai_ns = 1_000_000_000_000
    bind_req = lockstep_pb2.LockstepBindRequest(
        run_id="m24-4c-step-hang-probe", instance="controller", ports=[],
        start_tai_ns=start_tai_ns, base_period_ns=100_000_000, step_period_ns=100_000_000, seed=42,
    )
    bind_raw = build_frame(0x02, bind_req.SerializeToString())
    bridge.deliver_to_guest(bind_raw, 0.5)
    _ft, _pl, guest_bind_ack = bridge.guest_read_frame()
    print(f"BIND_ACK reply ({len(guest_bind_ack)} bytes): {guest_bind_ack.hex()}")

    # Real star-tracker (apid=200, 32 bytes: qx,qy,qz,qw) + IMU (apid=201, 48 bytes:
    # wx,wy,wz,ax,ay,az) packets -- a near-identity attitude, zero rates, values chosen only to
    # be realistic doubles, not zero/degenerate ones a decoder might special-case.
    star_bytes = ccsds_packet(200, False, 32, [(0, 0.01), (8, 0.02), (16, 0.03), (24, 0.999)])
    imu_bytes = ccsds_packet(201, False, 48, [(0, 0.001), (8, 0.002), (16, 0.003), (24, 0.0), (32, 0.0), (40, -9.81)])
    print(f"star packet ({len(star_bytes)} bytes): {star_bytes.hex()}")
    print(f"imu packet ({len(imu_bytes)} bytes): {imu_bytes.hex()}")

    until_tai_ns = start_tai_ns + 100_000_000
    step_req = lockstep_pb2.LockstepStepRequest(
        sequence=1, until_tai_ns=until_tai_ns,
        inputs=[
            lockstep_pb2.PortMessage(port="startracker_in", tai_ns=start_tai_ns, payload=star_bytes),
            lockstep_pb2.PortMessage(port="imu_in", tai_ns=start_tai_ns, payload=imu_bytes),
        ],
    )
    step_raw = build_frame(0x04, step_req.SerializeToString())
    print(f"STEP frame ({len(step_raw)} bytes)")
    bridge.mon.write_chars(step_raw)

    # Poll uart1's own TX-side register in small increments instead of a single fixed RunFor +
    # blocking socket read -- directly answers "does the guest ever put reply bytes in uart1's
    # TX FIFO at all" from hardware state, not a client-side timeout.
    total_virtual_s = 0.0
    for i in range(60):  # up to 6 virtual seconds, well past the 2000ms FROM_BUS timeout
        bridge.mon.run_for(0.1)
        total_virtual_s += 0.1
        ctrl = bridge.mon.read_reg_last_hex(UART1_CONTROL)
        sts = bridge.mon.read_reg_last_hex(UART1_CHANNEL_STS)
        temp_empty = bool(sts & TEMPTY)
        print(f"  t={total_virtual_s:.1f}s virtual: uart1 control=0x{ctrl:x} channel_sts=0x{sts:x} TEMPTY={temp_empty}")
        if not temp_empty:
            print("  -> TX FIFO has data queued (TEMPTY=0) -- the guest DID write something; draining now")
            break
    else:
        print("uart1's TX FIFO never showed TEMPTY=0 in 6 virtual seconds -- the guest never even started writing a reply")

    # Whether or not TEMPTY ever cleared, try the ordinary socket read with a short timeout --
    # if bytes are sitting in the outbound terminal's own buffer already, this returns instantly.
    try:
        _ft, _pl, guest_step_done = bridge.guest_read_frame(timeout=3.0)
        print(f"STEP_DONE reply received ({len(guest_step_done)} bytes): {guest_step_done.hex()}")
    except TimeoutError:
        print("guest_read_frame timed out -- no STEP_DONE ever arrived at the outbound terminal socket")

    bridge.stop()


if __name__ == "__main__":
    main()
