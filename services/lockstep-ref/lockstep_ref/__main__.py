"""Boots the ``LockstepRefServicer`` gRPC server. Test fixture only -- see this package's
``README.md``.

Threading: this reference model's own state (the running integral, the step counter) is
trivial and single-threaded by construction, unlike ``gmat_service`` (which must pin GMAT's
process-wide singleton to one worker thread) -- this process does not touch GMAT or any other
non-thread-safe native library, so a small worker pool is fine. `lockstep.proto`'s own
"one outstanding Step" rule is a contract on the *caller* (`av-kernel`'s `ContainerModel`
never sends a second `Step` before the first's response arrives), not something this server
needs to separately enforce with a single-worker pool the way `gmat_service` does.

Plaintext gRPC on localhost only, by default, matching every other Python-hosted gRPC
fixture/service in this repository (``gmat_service``): ``--tls`` opts into mTLS through
``grpc.ssl_server_credentials`` with the given server cert/key and (optionally) a CA file for
client-certificate verification -- **note this is `grpcio`'s own TLS, which bundles
BoringSSL** (see ``services/gmat-service/README.md``'s "TLS front door" section for why that
mirrors this repository's ADR-004 crypto rule) -- so ``--tls`` is provided only so a caller
who explicitly wants to exercise `av_lockstep::BlockingLockstepClient::connect_mtls` end to
end has something to point it at in a throwaway test; the required tests in this batch all
use plaintext loopback, exactly as this task's brief allows ("plaintext loopback allowed only
for tests").
"""
from __future__ import annotations

import argparse
import logging
import sys
import threading
from concurrent import futures

import grpc

from altavista.pb.altavista.v1 import lockstep_pb2_grpc

from .server import LockstepRefServicer

log = logging.getLogger("lockstep_ref")

DEFAULT_PORT = 50070


def serve(port: int = DEFAULT_PORT, host: str = "127.0.0.1", in_port: str = "in", out_port: str = "out", output_name: str = "integral",
          tls_cert: "str | None" = None, tls_key: "str | None" = None, tls_ca: "str | None" = None,
          block: bool = True) -> grpc.Server:
    """Build and start the server. Returns the running ``grpc.Server`` (already started);
    with ``block=True`` (the default, and what the CLI uses) this only returns once a
    ``Shutdown`` RPC has been received (or the process is killed).

    ``host`` defaults to ``127.0.0.1`` (loopback), matching every other Python-hosted gRPC
    fixture in this repository -- every *local-subprocess* test binds this way. M15.3
    (question 118) added the parameter so a Docker-run instance of this same image can pass
    ``--host 0.0.0.0``: the *container's* own interface must accept connections from outside its
    network namespace for `docker run -p 127.0.0.1::<port>` to actually reach it, even though
    the caller (`av-kernel`'s Docker-lifecycle path) still only ever connects over its own
    loopback -- ADR-003's "plaintext loopback only inside one node under test" rule is about the
    kernel's own connection, not this process's bind address inside its container's namespace."""
    stop_event = threading.Event()
    executor = futures.ThreadPoolExecutor(max_workers=4)
    server = grpc.server(executor)
    servicer = LockstepRefServicer(in_port=in_port, out_port=out_port, output_name=output_name, stop_event=stop_event)
    lockstep_pb2_grpc.add_LockstepServiceServicer_to_server(servicer, server)

    address = f"{host}:{port}"
    if tls_cert and tls_key:
        with open(tls_cert, "rb") as f:
            cert_chain = f.read()
        with open(tls_key, "rb") as f:
            private_key = f.read()
        root_certs = None
        require_client_auth = False
        if tls_ca:
            with open(tls_ca, "rb") as f:
                root_certs = f.read()
            require_client_auth = True
        creds = grpc.ssl_server_credentials([(private_key, cert_chain)], root_certificates=root_certs, require_client_auth=require_client_auth)
        bound_port = server.add_secure_port(address, creds)
    else:
        bound_port = server.add_insecure_port(address)
    if bound_port == 0:
        raise RuntimeError(f"failed to bind {address} (port already in use?)")

    server.start()
    log.info("lockstep-ref listening on %s:%d (in_port=%s out_port=%s output_name=%s)", host, bound_port, in_port, out_port, output_name)

    if block:
        # Wait for either a Shutdown RPC (stop_event) or the process being killed.
        while not stop_event.wait(timeout=0.1):
            pass
        server.stop(grace=1.0).wait()
        log.info("lockstep-ref stopped (Shutdown received)")
    return server


def main(argv=None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s", stream=sys.stderr)
    p = argparse.ArgumentParser(prog="lockstep_ref", description="Reference altavista.v1.LockstepService implementation (test fixture, docs/open-questions.md question 107)")
    p.add_argument("--port", type=int, default=DEFAULT_PORT)
    p.add_argument("--host", default="127.0.0.1", help="bind address (0.0.0.0 when run inside a Docker container so a published port can reach it; see serve()'s own docstring)")
    p.add_argument("--in-port", default="in", help="declared SIGNAL input port name this process expects at Bind")
    p.add_argument("--out-port", default="out", help="declared SIGNAL output port name this process emits on")
    p.add_argument("--output-name", default="integral", help="named_outputs key this process reports")
    p.add_argument("--tls-cert", default=None)
    p.add_argument("--tls-key", default=None)
    p.add_argument("--tls-ca", default=None, help="if set alongside --tls-cert/--tls-key, requires and verifies a client certificate against this CA")
    args = p.parse_args(argv)
    serve(port=args.port, host=args.host, in_port=args.in_port, out_port=args.out_port, output_name=args.output_name,
          tls_cert=args.tls_cert, tls_key=args.tls_key, tls_ca=args.tls_ca)
    return 0


if __name__ == "__main__":
    sys.exit(main())
