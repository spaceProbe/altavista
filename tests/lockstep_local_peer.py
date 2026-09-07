"""A reference "flight software" peer for "lockstep-local v1" (M23.1,
`docs/open-questions.md` question 153) -- the Python test fixture the task brief calls for:
"proving the shim end to end against the existing kernel container path before any cFS code
exists." This process is what `crates/av-lockstep-shim/tests/end_to_end_kernel_path.rs`
connects to the shim's Unix socket in place of a real cFS lockstep I/O app.

Speaks exactly the byte layout documented in `services/cfs/README.md`: a length-prefixed
frame `[length: u32 LE][frame_type: u8][payload]`, little-endian throughout, `length`
excluding itself. `HELLO`/`ERROR` payloads are this module's own small layouts (matching
`crates/av-lockstep-shim/src/framing.rs` byte for byte); every other payload is a plain
`lockstep_pb2` message, serialized with ordinary protobuf `SerializeToString()`/`ParseFromString()`
-- the same messages `services/lockstep-ref` already speaks over gRPC, now carried inside a
local frame instead of an HTTP/2 stream.

Behaviour mirrors `services/lockstep-ref/lockstep_ref/server.py`'s own SIGNAL-in/SIGNAL-out
integrator deliberately (one `in` SIGNAL input port, one `out` SIGNAL output port, one named
output `integral`): the acceptance test drives both this peer (through the shim) and, in
spirit, the same contract `lockstep-ref` already proves against the kernel's own gRPC path --
so the two are directly comparable, not two unrelated reference behaviours.
"""
from __future__ import annotations

import argparse
import hashlib
import socket
import struct
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LOCKSTEP_REF_DIR = REPO_ROOT / "services" / "lockstep-ref"
if str(LOCKSTEP_REF_DIR) not in sys.path:
    sys.path.insert(0, str(LOCKSTEP_REF_DIR))
from altavista.pb.altavista.v1 import lockstep_pb2, system_pb2  # noqa: E402

PROTOCOL_VERSION = 1
HELLO_MAGIC = b"AVL1"

FRAME_HELLO = 0x01
FRAME_BIND = 0x02
FRAME_BIND_ACK = 0x03
FRAME_STEP = 0x04
FRAME_STEP_DONE = 0x05
FRAME_RESET = 0x06
FRAME_RESET_ACK = 0x07
FRAME_SHUTDOWN = 0x08
FRAME_SHUTDOWN_ACK = 0x09
FRAME_ERROR = 0xFF

ERROR_CODE_VERSION_MISMATCH = 1
ERROR_CODE_BAD_HELLO_MAGIC = 7


def encode_signal(value: float) -> bytes:
    """Little-endian IEEE-754 double -- matches `av_dynamics::encode_signal` and
    `lockstep_ref.server.encode_signal` byte for byte (`lockstep.proto`'s own doc comment)."""
    return struct.pack("<d", value)


def decode_signal(payload: bytes) -> "float | None":
    if len(payload) != 8:
        return None
    return struct.unpack("<d", payload)[0]


def recv_exact(sock: socket.socket, n: int) -> bytes:
    """Reads exactly `n` bytes or raises `ConnectionError` -- never returns a short read
    silently (the same "a dropped frame is a typed error, never silently retried" rule
    `crates/av-lockstep-shim/src/framing.rs::read_frame` follows on the Rust side)."""
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError(f"peer closed the connection after {len(buf)} of {n} expected bytes")
        buf += chunk
    return bytes(buf)


def read_frame(sock: socket.socket) -> "tuple[int, bytes]":
    length = struct.unpack("<I", recv_exact(sock, 4))[0]
    if length == 0:
        raise ValueError("frame declares length 0 -- every frame carries at least a frame_type byte")
    rest = recv_exact(sock, length)
    return rest[0], rest[1:]


def write_frame(sock: socket.socket, frame_type: int, payload: bytes) -> None:
    length = 1 + len(payload)
    sock.sendall(struct.pack("<I", length) + bytes([frame_type]) + payload)


def encode_hello(version: int) -> bytes:
    return HELLO_MAGIC + struct.pack("<H", version)


def decode_hello(payload: bytes) -> "tuple[bytes, int]":
    if len(payload) != 6:
        raise ValueError(f"HELLO payload must be exactly 6 bytes, got {len(payload)}")
    return payload[0:4], struct.unpack("<H", payload[4:6])[0]


def encode_error(code: int, expected: int, actual: int, message: str) -> bytes:
    msg_bytes = message.encode("utf-8")
    return bytes([code]) + struct.pack("<q", expected) + struct.pack("<q", actual) + struct.pack("<H", len(msg_bytes)) + msg_bytes


class LockstepLocalPeer:
    def __init__(self, sock: socket.socket, in_port: str = "in", out_port: str = "out", output_name: str = "integral") -> None:
        self.sock = sock
        self.in_port = in_port
        self.out_port = out_port
        self.output_name = output_name
        self.integral = 0.0
        self.cursor_tai_ns = 0
        self._bound = False

    def handshake(self) -> bool:
        """Reads the shim's `HELLO` (the shim always speaks first --
        `services/cfs/README.md`'s "Who listens, who connects, who speaks first") and
        answers with this peer's own. Returns `False` (after sending a typed `ERROR`) on a
        magic/version mismatch, mirroring the Rust shim's own handshake contract exactly."""
        frame_type, payload = read_frame(self.sock)
        if frame_type != FRAME_HELLO:
            write_frame(self.sock, FRAME_ERROR, encode_error(0, 0, 0, f"expected HELLO, got frame_type 0x{frame_type:02X}"))
            return False
        magic, version = decode_hello(payload)
        if magic != HELLO_MAGIC:
            write_frame(self.sock, FRAME_ERROR, encode_error(ERROR_CODE_BAD_HELLO_MAGIC, 0, 0, f"bad HELLO magic: expected {HELLO_MAGIC!r}, got {magic!r}"))
            return False
        if version != PROTOCOL_VERSION:
            write_frame(self.sock, FRAME_ERROR, encode_error(ERROR_CODE_VERSION_MISMATCH, PROTOCOL_VERSION, version, f"version mismatch: this peer speaks v{PROTOCOL_VERSION}, shim sent v{version}"))
            return False
        write_frame(self.sock, FRAME_HELLO, encode_hello(PROTOCOL_VERSION))
        return True

    def _expected_ports(self) -> "dict[str, tuple[int, int]]":
        return {
            self.in_port: (system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_IN),
            self.out_port: (system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_OUT),
        }

    def _port_mismatch_reason(self, ports) -> "str | None":
        expected = self._expected_ports()
        got = {p.name: (p.kind, p.direction) for p in ports}
        if got.keys() != expected.keys():
            return f"port name set mismatch: expected {sorted(expected.keys())}, got {sorted(got.keys())}"
        for name, (exp_kind, exp_dir) in expected.items():
            if got[name] != (exp_kind, exp_dir):
                return f"port {name!r}: expected kind={exp_kind} direction={exp_dir}, got kind={got[name][0]} direction={got[name][1]}"
        return None

    def handle_bind(self, payload: bytes) -> None:
        request = lockstep_pb2.LockstepBindRequest()
        request.ParseFromString(payload)
        reason = self._port_mismatch_reason(request.ports)
        if reason is not None:
            response = lockstep_pb2.LockstepBindResponse(lockstep_capable=False, refusal_reason=reason)
            write_frame(self.sock, FRAME_BIND_ACK, response.SerializeToString())
            return
        self._bound = True
        self.integral = 0.0
        self.cursor_tai_ns = request.start_tai_ns
        h = hashlib.sha256()
        h.update(b"lockstep-local-peer\n")
        h.update(request.run_id.encode())
        h.update(b"\n")
        h.update(request.instance.encode())
        response = lockstep_pb2.LockstepBindResponse(lockstep_capable=True, binding_hash=h.hexdigest(), version="lockstep-local-peer/0.1")
        write_frame(self.sock, FRAME_BIND_ACK, response.SerializeToString())

    def handle_step(self, payload: bytes) -> None:
        request = lockstep_pb2.LockstepStepRequest()
        request.ParseFromString(payload)
        total = 0.0
        for msg in request.inputs:
            if msg.port != self.in_port:
                continue
            value = decode_signal(msg.payload)
            if value is None:
                write_frame(self.sock, FRAME_ERROR, encode_error(6, 0, 0, f"port {msg.port!r}: payload is not exactly 8 bytes"))
                return
            total += value
        dt_s = (request.until_tai_ns - self.cursor_tai_ns) * 1e-9
        self.integral += total * dt_s
        self.cursor_tai_ns = request.until_tai_ns
        response = lockstep_pb2.LockstepStepResponse(
            sequence=request.sequence,
            reached_tai_ns=request.until_tai_ns,
            outputs=[lockstep_pb2.PortMessage(port=self.out_port, tai_ns=request.until_tai_ns, payload=encode_signal(self.integral))],
            named_outputs={self.output_name: self.integral},
        )
        write_frame(self.sock, FRAME_STEP_DONE, response.SerializeToString())

    def handle_reset(self, payload: bytes) -> None:
        request = lockstep_pb2.LockstepResetRequest()
        request.ParseFromString(payload)
        self.integral = 0.0
        self.cursor_tai_ns = request.tai_ns
        response = lockstep_pb2.LockstepResetResponse(sequence=request.sequence)
        write_frame(self.sock, FRAME_RESET_ACK, response.SerializeToString())

    def handle_shutdown(self, payload: bytes) -> bool:
        request = lockstep_pb2.LockstepShutdownRequest()
        request.ParseFromString(payload)
        response = lockstep_pb2.LockstepShutdownResponse()
        write_frame(self.sock, FRAME_SHUTDOWN_ACK, response.SerializeToString())
        return False  # tells run() to stop the loop

    def run(self) -> None:
        if not self.handshake():
            return
        running = True
        while running:
            frame_type, payload = read_frame(self.sock)
            if frame_type == FRAME_BIND:
                self.handle_bind(payload)
            elif frame_type == FRAME_STEP:
                self.handle_step(payload)
            elif frame_type == FRAME_RESET:
                self.handle_reset(payload)
            elif frame_type == FRAME_SHUTDOWN:
                running = self.handle_shutdown(payload)
            elif frame_type == FRAME_ERROR:
                # The shim reported a protocol error on us -- nothing more to do.
                running = False
            else:
                write_frame(self.sock, FRAME_ERROR, encode_error(5, 0, frame_type, f"unexpected frame_type 0x{frame_type:02X}"))
                running = False


def main() -> None:
    parser = argparse.ArgumentParser(description="lockstep-local v1 reference peer (M23.1)")
    parser.add_argument("--socket-path", required=True)
    parser.add_argument("--in-port", default="in")
    parser.add_argument("--out-port", default="out")
    parser.add_argument("--output-name", default="integral")
    args = parser.parse_args()

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(args.socket_path)
    try:
        peer = LockstepLocalPeer(sock, in_port=args.in_port, out_port=args.out_port, output_name=args.output_name)
        peer.run()
    finally:
        sock.close()


if __name__ == "__main__":
    main()
