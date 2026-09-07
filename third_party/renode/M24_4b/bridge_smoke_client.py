#!/usr/bin/env python3
"""M24.4b -- drives the real LockstepService gRPC surface (Bind, a few Steps, Reset, one more
Step, Shutdown) against the shim fronted by renode_bridge.py, to prove the bridge works
end-to-end: shim <-gRPC-> this client, shim <-lockstep-local-> bridge <-WriteChar/UART-> the real
RTEMS/cFE/IO_LOCKSTEP guest under Renode.
"""
import sys
import time

import grpc

from altavista.pb.altavista.v1 import lockstep_pb2, lockstep_pb2_grpc


def main():
    addr = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:15210"
    channel = grpc.insecure_channel(addr)
    print(f"waiting for the shim's gRPC server at {addr} to become ready...")
    grpc.channel_ready_future(channel).result(timeout=180.0)
    print("channel ready")
    stub = lockstep_pb2_grpc.LockstepServiceStub(channel)

    start_tai_ns = 1_000_000_000_000
    # Deliberately >= IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS (2000ms, io_lockstep_app.c) plus margin:
    # this smoke test sends no TO_BUS inputs at all, so ADCS never receives a measurement and
    # never publishes -- every FROM_BUS port legitimately times out every step (a real,
    # documented "nothing to report yet" outcome, not a bug), and that 2000ms wait is measured
    # against the guest's own RTEMS clock tick, which only advances during the bridge's own
    # `RunFor` -- so the bridge must be given at least that much virtual time per step to let the
    # guest actually finish processing and write its STEP_DONE reply.
    base_period_ns = 3_000_000_000  # 3 s

    print("=== Bind ===")
    bind_resp = stub.Bind(lockstep_pb2.LockstepBindRequest(
        run_id="m24-4b-bridge-smoke",
        instance="io_lockstep",
        ports=[],
        start_tai_ns=start_tai_ns,
        base_period_ns=base_period_ns,
        step_period_ns=base_period_ns,
        seed=1,
    ), timeout=60.0)
    print(f"lockstep_capable={bind_resp.lockstep_capable} version={bind_resp.version!r} "
          f"refusal_reason={bind_resp.refusal_reason!r}")
    if not bind_resp.lockstep_capable:
        print("FAIL: not lockstep_capable")
        return 1

    seq = 1
    tai = start_tai_ns
    for i in range(3):
        tai += base_period_ns
        print(f"=== Step {i} (until_tai_ns={tai}) ===")
        step_resp = stub.Step(lockstep_pb2.LockstepStepRequest(
            sequence=seq, until_tai_ns=tai, inputs=[],
        ), timeout=60.0)
        print(f"reached_tai_ns={step_resp.reached_tai_ns} outputs={len(step_resp.outputs)}")
        if step_resp.reached_tai_ns != tai:
            print("FAIL: reached_tai_ns mismatch")
            return 1
        seq += 1

    print("=== Reset (hardware power-cycle through the bridge) ===")
    reset_resp = stub.Reset(lockstep_pb2.LockstepResetRequest(
        sequence=seq, tai_ns=tai, reason="fault:m24-4b-smoke-hardware-fault",
    ), timeout=60.0)
    print(f"reset sequence echoed={reset_resp.sequence}")
    if reset_resp.sequence != seq:
        print("FAIL: reset sequence mismatch")
        return 1
    seq += 1

    tai += base_period_ns
    print(f"=== Step after Reset (until_tai_ns={tai}) ===")
    step_resp = stub.Step(lockstep_pb2.LockstepStepRequest(
        sequence=seq, until_tai_ns=tai, inputs=[],
    ), timeout=60.0)
    print(f"reached_tai_ns={step_resp.reached_tai_ns} outputs={len(step_resp.outputs)}")
    if step_resp.reached_tai_ns != tai:
        print("FAIL: post-reset reached_tai_ns mismatch")
        return 1
    seq += 1

    print("=== Shutdown ===")
    stub.Shutdown(lockstep_pb2.LockstepShutdownRequest(run_id="m24-4b-bridge-smoke"), timeout=30.0)
    print("PASS: full Bind/Step/Reset/Step/Shutdown cycle succeeded end to end")
    return 0


if __name__ == "__main__":
    sys.exit(main())
