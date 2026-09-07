"""Boots the ``DynamicsService`` gRPC server.

Threading -- the GMAT-singleton constraint
--------------------------------------------
GMAT holds ONE configuration per process and is not thread-safe
(``docs/adr/002-dynamics-contract.md``, amendment 2026-09-02: "all handles are !Send;
parallelism is by process"). The whole gRPC thread pool below is sized to exactly **one**
worker (``futures.ThreadPoolExecutor(max_workers=1)``), so every RPC -- and therefore
every call this service ever makes into ``gmatpy`` -- runs on that single thread, for the
life of the process. This is the simpler of the two options M2.2 allows ("a grpc.server
with a 1-worker thread pool, or a request queue drained by one thread"): concurrent
requests simply queue behind each other rather than needing a second serialization
mechanism layered on top of gRPC's own dispatch. Acceptable because ``DynamicsService`` at
depth 1 is a design-time/low-rate path (ADR-002: the real-time hot path is depths 2/3, not
this Python-API host).

``warm_up()`` (loading GMAT and building this server's one force-model/propagator
configuration) is explicitly submitted to that same one-worker executor *before*
``server.start()``, so literally the first ``gmatpy`` call this process ever makes -- not
just steady-state traffic -- happens on the pinned thread, and the first real request does
not pay GMAT's model-build cost (ADR-002 amendment: "Model build (construct, initialize,
bind) | 29.6 ms").

Security note (ADR-004)
--------------------------
This binds plaintext gRPC on localhost only (``add_insecure_port("127.0.0.1:<port>")``).
That is accepted for this milestone; mTLS with seccert-issued certificates per ADR-004 is
not wired up here yet -- see the README.
"""
from __future__ import annotations

import argparse
import logging
import sys
import uuid
from concurrent import futures

import grpc

from altavista.pb import dynamics_service_pb2_grpc

from . import config
from .admin import serve_admin
from .evidence import EvidenceLog
from .service import DynamicsServiceServicer

log = logging.getLogger("gmat_service.server")


def serve(port: int = config.DEFAULT_PORT, evidence_path=config.DEFAULT_EVIDENCE_PATH,
         run_id: "str | None" = None, block: bool = True,
         admin_port: int = config.DEFAULT_ADMIN_PORT) -> grpc.Server:
    """Build, warm up and start the server. Returns the running ``grpc.Server`` (already
    started); with ``block=True`` (the default, and what the CLI uses) this only returns
    after the server stops.

    Also starts the localhost-only ``/admin/api/evidence`` HTTP admin server (ADR-004
    question 63, :mod:`gmat_service.admin`) on ``admin_port``, on a background daemon
    thread -- it stops on its own when this process exits (daemon thread), there is no
    separate shutdown call needed the way there is for the returned ``grpc.Server``.
    """
    executor = futures.ThreadPoolExecutor(max_workers=1)  # see module docstring
    server = grpc.server(executor)
    evidence = EvidenceLog(evidence_path)
    servicer = DynamicsServiceServicer(evidence=evidence, run_id=run_id or uuid.uuid4().hex)
    dynamics_service_pb2_grpc.add_DynamicsServiceServicer_to_server(servicer, server)

    address = f"127.0.0.1:{port}"
    bound_port = server.add_insecure_port(address)
    if bound_port == 0:
        raise RuntimeError(f"failed to bind {address} (port already in use?)")

    # Run on the SAME single worker thread every RPC will run on (see module docstring).
    executor.submit(servicer.warm_up).result()

    # Never a non-loopback address (ADR-004) -- same rule as the gRPC port above, and this
    # endpoint is not fronted by nginx/mTLS at all (secdeploy evidence is expected to run on
    # the same host).
    serve_admin(host="127.0.0.1", port=admin_port, evidence=evidence, run_id=servicer.run_id)

    server.start()
    log.info("gmat-service listening on 127.0.0.1:%d (run_id=%s, evidence=%s)",
             bound_port, servicer.run_id, evidence.path)
    if block:
        server.wait_for_termination()
    return server


def main(argv=None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    p = argparse.ArgumentParser(
        prog="gmat_service",
        description="altavista.v1.DynamicsService over altavista (ADR-002 depth 1: GMAT Python API in-process)")
    p.add_argument("--port", type=int, default=config.DEFAULT_PORT, help="localhost port to bind (plaintext, ADR-004 not wired up yet)")
    p.add_argument("--admin-port", type=int, default=config.DEFAULT_ADMIN_PORT, help="localhost port for /admin/api/evidence (ADR-004 question 63)")
    p.add_argument("--evidence-path", default=str(config.DEFAULT_EVIDENCE_PATH), help="evidence JSONL path")
    p.add_argument("--run-id", default=None, help="override the generated run_id (mainly for tests)")
    args = p.parse_args(argv)
    serve(port=args.port, evidence_path=args.evidence_path, run_id=args.run_id, admin_port=args.admin_port)
    return 0


if __name__ == "__main__":
    sys.exit(main())
