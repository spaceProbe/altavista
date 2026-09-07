#!/usr/bin/env python3
"""Diagnostic-only (M24_4g): parse a captured uart1_raw_crosscheck.log the same way
renode_bridge.py's own read_frame() does, to know authoritatively (not by eyeballing od -c
output) how many lockstep-local v1 frames the guest actually transmitted."""
import struct
import sys

FRAME_NAMES = {
    0x01: "HELLO", 0x02: "BIND", 0x03: "BIND_ACK", 0x04: "STEP", 0x05: "STEP_DONE",
    0x06: "RESET", 0x07: "RESET_ACK", 0x08: "SHUTDOWN", 0x09: "SHUTDOWN_ACK", 0xFF: "ERROR",
}


def main(path):
    with open(path, "rb") as f:
        buf = f.read()
    print(f"{path}: {len(buf)} bytes total")
    off = 0
    n = 0
    while off < len(buf):
        if off + 4 > len(buf):
            print(f"  trailing {len(buf) - off} byte(s), not a full length prefix: {buf[off:]!r}")
            break
        (length,) = struct.unpack_from("<I", buf, off)
        if off + 4 + length > len(buf):
            print(f"  frame at offset {off} claims length={length} but only {len(buf) - off - 4} bytes remain: {buf[off:]!r}")
            break
        payload = buf[off + 4: off + 4 + length]
        frame_type = payload[0] if payload else None
        n += 1
        name = FRAME_NAMES.get(frame_type, f"0x{frame_type:02x}" if frame_type is not None else "?")
        print(f"  frame {n}: offset={off} length={length} frame_type={name} raw_payload={payload!r}")
        off += 4 + length
    print(f"total frames parsed: {n}, bytes consumed: {off}/{len(buf)}")


if __name__ == "__main__":
    main(sys.argv[1])
