"""``LockstepServiceServicer``: a deterministic SIGNAL-in/SIGNAL-out integrator.

Behaviour (see this package's ``README.md`` for the full contract):

- ``Bind`` validates the caller's declared ``ports`` against exactly the two SIGNAL ports
  this process expects (``--in-port``/``--out-port``, default ``"in"``/``"out"``) --
  mismatched name/kind/direction, or a wrong port count, is a refusal
  (``lockstep_capable=False``, a ``refusal_reason`` naming exactly what disagreed), never a
  silent partial bind. Resets all internal state (the integral, the sequence-independent
  step counter, the simulated clock) so a second ``Bind`` on the same process starts clean.
- ``Step`` sums every SIGNAL payload delivered on the input port this step, holds it constant
  over ``[cursor, until_tai_ns]`` (an explicit Euler integration -- no claim to a higher-order
  scheme), adds ``value * dt_s`` to the running integral, and emits the integral as a SIGNAL
  on the output port plus as the one declared named output (``--output-name``, default
  ``"integral"``). Deterministic: a pure function of ``(the Bind parameters, the ordered
  sequence of Step inputs)`` -- no wall clock, no unseeded randomness (``seed`` is accepted
  and recorded for protocol completeness; this particular reference model has no random
  behaviour to seed -- see the README's "Honesty" section).
- ``Reset`` zeroes the integral and re-anchors the simulated clock at ``request.tai_ns``,
  mirroring ``Bind``'s own reset. M15.3 (question 118): now driven end to end through
  ``av-kernel`` by a ``FAULT_TARGET_KIND_DYNAMICS``/``kind == "power_cycle"`` fault targeting a
  container-bound instance (``crate::drm::executor::run_shared_group``'s boundary loop) --
  ``reason`` arrives as ``"fault:<fault id>"`` -- see the README's "Reset is wired end to end"
  section and ``crates/av-kernel/tests/drm_container.rs``.
- ``Shutdown`` logs and lets the caller stop the server (``lockstep_ref.__main__`` stops the
  ``grpc.Server`` once the RPC returns).

Test-only misbehaviour knobs (environment variables, never read outside a test process):

- ``LOCKSTEP_REF_REFUSE=1`` -- every ``Bind`` refuses (``lockstep_capable=False``), whatever
  the request's ports look like. Exercises the plain "the process is not lockstep capable"
  refusal, distinct from a port-set mismatch.
- ``LOCKSTEP_REF_LIE_REACHED_AT_STEP=<n>`` -- on the ``n``-th ``Step`` call after a successful
  ``Bind`` (1-indexed), respond with ``reached_tai_ns = until_tai_ns + 1`` instead of the
  correct value -- the fixture `docs/open-questions.md` question 107 and this task's brief
  both call for: "a fixture that lies about reached_tai_ns is caught."
- ``LOCKSTEP_REF_LIE_SEQUENCE_AT_STEP=<n>`` -- on the ``n``-th ``Step`` call, respond with
  ``sequence = request.sequence + 1`` instead of echoing it back correctly.

Both lie knobs fire exactly once (on the named step) and then behave correctly again, so a
test can assert the run stopped at exactly that boundary.
"""
from __future__ import annotations

import logging
import os

import grpc

from altavista.pb.altavista.v1 import lockstep_pb2, lockstep_pb2_grpc, system_pb2

log = logging.getLogger("lockstep_ref.server")


def encode_signal(value: float) -> bytes:
    """Little-endian IEEE-754 double -- ``lockstep.proto``'s own ``PortMessage`` doc comment.
    The inverse of :func:`decode_signal`; matches
    ``crates/av-dynamics/src/lib.rs``'s ``encode_signal`` byte for byte."""
    import struct
    return struct.pack("<d", value)


def decode_signal(payload: bytes) -> "float | None":
    """The inverse of :func:`encode_signal` -- ``None`` if ``payload`` is not exactly 8
    bytes, matching the Rust side's ``decode_signal`` refusing-to-guess contract."""
    import struct
    if len(payload) != 8:
        return None
    return struct.unpack("<d", payload)[0]


class LockstepRefServicer(lockstep_pb2_grpc.LockstepServiceServicer):
    def __init__(self, in_port: str = "in", out_port: str = "out", output_name: str = "integral", stop_event=None) -> None:
        self.in_port = in_port
        self.out_port = out_port
        self.output_name = output_name
        # Set (by `Shutdown`, below) when the caller has asked this process to stop --
        # `lockstep_ref.__main__`'s own serve loop watches it, rather than this servicer
        # reaching into `grpc.Server`/`ServicerContext` internals to stop itself mid-RPC.
        self.stop_event = stop_event
        self._reset_state()
        self._bound = False
        self._run_id = ""
        self._instance = ""

    def _reset_state(self) -> None:
        self.integral = 0.0
        self.cursor_tai_ns = 0
        self.step_count = 0

    def _expected_ports(self) -> "dict[str, tuple[int, int]]":
        return {
            self.in_port: (system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_IN),
            self.out_port: (system_pb2.PORT_KIND_SIGNAL, system_pb2.PORT_DIRECTION_OUT),
        }

    def _port_mismatch_reason(self, ports) -> "str | None":
        """``None`` if ``ports`` is exactly the expected set (name -> (kind, direction)),
        case-sensitive, order-independent; otherwise a human-readable reason naming exactly
        what disagreed -- `lockstep.proto`'s own doc comment: "the bound process must accept
        exactly this set (names, kinds, directions) or refuse.\""""
        expected = self._expected_ports()
        got = {p.name: (p.kind, p.direction) for p in ports}
        if len(ports) != len(got):
            return f"duplicate port name(s) in request (got {len(ports)} Port entries but {len(got)} distinct names)"
        if got.keys() != expected.keys():
            missing = sorted(expected.keys() - got.keys())
            extra = sorted(got.keys() - expected.keys())
            return f"port name set mismatch: expected {sorted(expected.keys())}, missing {missing}, unexpected {extra}"
        for name, (exp_kind, exp_dir) in expected.items():
            got_kind, got_dir = got[name]
            if got_kind != exp_kind or got_dir != exp_dir:
                return (
                    f"port {name!r}: expected kind={exp_kind} direction={exp_dir}, "
                    f"got kind={got_kind} direction={got_dir}"
                )
        return None

    def Bind(self, request: lockstep_pb2.LockstepBindRequest, context) -> lockstep_pb2.LockstepBindResponse:
        self._reset_state()
        self._bound = False

        if os.environ.get("LOCKSTEP_REF_REFUSE") == "1":
            log.info("Bind refused: LOCKSTEP_REF_REFUSE=1")
            return lockstep_pb2.LockstepBindResponse(
                lockstep_capable=False,
                refusal_reason="refused for testing (LOCKSTEP_REF_REFUSE=1)",
            )

        reason = self._port_mismatch_reason(request.ports)
        if reason is not None:
            log.info("Bind refused: %s", reason)
            return lockstep_pb2.LockstepBindResponse(lockstep_capable=False, refusal_reason=reason)

        self._bound = True
        self._run_id = request.run_id
        self._instance = request.instance
        self.cursor_tai_ns = request.start_tai_ns

        # SHA-256 over the bound identity this process presents -- `lockstep.proto`'s own
        # `LockstepBindResponse.binding_hash` doc comment: "SHA-256 over the bound process's own
        # identity (image digest or binary hash plus its configuration)". M15.3 (question 118):
        # when this process is run from a Docker image (rather than spawned as a bare
        # subprocess, the M13.2-era-only path), the caller passes the pulled image's own digest
        # through `IMAGE_DIGEST` (an environment variable, never an RPC field -- the same
        # "read only from the environment, never trust the wire" rule this file's own
        # `LOCKSTEP_REF_*` misbehaviour knobs already follow) so it is folded into the hash
        # here, exactly where the proto's own doc comment says the process's identity belongs.
        # Absent (the plain-subprocess path, still exactly what `tests/drm_container.rs` uses),
        # this reference binary has no image digest -- "its own module name plus everything
        # Bind declared that determines its behaviour" (run_id, instance, start/base/step
        # periods, seed, sorted parameter map) is the whole identity, unchanged from before
        # M15.3.
        import hashlib
        h = hashlib.sha256()
        h.update(b"lockstep-ref\n")
        h.update(os.environ.get("IMAGE_DIGEST", "").encode())
        h.update(b"\n")
        h.update(request.run_id.encode())
        h.update(b"\n")
        h.update(request.instance.encode())
        h.update(b"\n")
        h.update(str(request.start_tai_ns).encode())
        h.update(b"\n")
        h.update(str(request.base_period_ns).encode())
        h.update(b"\n")
        h.update(str(request.step_period_ns).encode())
        h.update(b"\n")
        h.update(str(request.seed).encode())
        h.update(b"\n")
        for k in sorted(request.parameters):
            h.update(f"{k}={request.parameters[k]}\n".encode())
        binding_hash = h.hexdigest()

        log.info("Bind ok: run_id=%s instance=%s start_tai_ns=%d", request.run_id, request.instance, request.start_tai_ns)
        return lockstep_pb2.LockstepBindResponse(
            lockstep_capable=True,
            binding_hash=binding_hash,
            version="lockstep-ref/0.1",
        )

    def Step(self, request: lockstep_pb2.LockstepStepRequest, context) -> lockstep_pb2.LockstepStepResponse:
        if not self._bound:
            context.abort(grpc.StatusCode.FAILED_PRECONDITION, "Step called before a successful Bind")

        self.step_count += 1

        total = 0.0
        for msg in request.inputs:
            if msg.port != self.in_port:
                continue
            value = decode_signal(msg.payload)
            if value is None:
                context.abort(grpc.StatusCode.INVALID_ARGUMENT, f"port {msg.port!r}: payload is not exactly 8 bytes (a SIGNAL port carries a little-endian f64)")
            total += value

        dt_s = (request.until_tai_ns - self.cursor_tai_ns) * 1e-9
        self.integral += total * dt_s
        self.cursor_tai_ns = request.until_tai_ns

        reached_tai_ns = request.until_tai_ns
        lie_reached_at = os.environ.get("LOCKSTEP_REF_LIE_REACHED_AT_STEP")
        if lie_reached_at is not None and self.step_count == int(lie_reached_at):
            reached_tai_ns = request.until_tai_ns + 1
            log.warning("Step %d: LOCKSTEP_REF_LIE_REACHED_AT_STEP fired -- reporting reached_tai_ns=%d (requested %d)", self.step_count, reached_tai_ns, request.until_tai_ns)

        sequence = request.sequence
        lie_sequence_at = os.environ.get("LOCKSTEP_REF_LIE_SEQUENCE_AT_STEP")
        if lie_sequence_at is not None and self.step_count == int(lie_sequence_at):
            sequence = request.sequence + 1
            log.warning("Step %d: LOCKSTEP_REF_LIE_SEQUENCE_AT_STEP fired -- echoing sequence=%d (request carried %d)", self.step_count, sequence, request.sequence)

        return lockstep_pb2.LockstepStepResponse(
            sequence=sequence,
            reached_tai_ns=reached_tai_ns,
            outputs=[lockstep_pb2.PortMessage(port=self.out_port, tai_ns=reached_tai_ns, payload=encode_signal(self.integral))],
            named_outputs={self.output_name: self.integral},
        )

    def Reset(self, request: lockstep_pb2.LockstepResetRequest, context) -> lockstep_pb2.LockstepResetResponse:
        log.info("Reset: reason=%r tai_ns=%d", request.reason, request.tai_ns)
        self.integral = 0.0
        self.cursor_tai_ns = request.tai_ns
        return lockstep_pb2.LockstepResetResponse(sequence=request.sequence)

    def Shutdown(self, request: lockstep_pb2.LockstepShutdownRequest, context) -> lockstep_pb2.LockstepShutdownResponse:
        log.info("Shutdown: run_id=%r", request.run_id)
        if self.stop_event is not None:
            self.stop_event.set()
        return lockstep_pb2.LockstepShutdownResponse()
