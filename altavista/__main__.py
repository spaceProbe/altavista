"""Command line: ``python -m altavista serve [--host H] [--port P]``."""
from __future__ import annotations

import argparse
import sys


def main(argv=None) -> int:
    p = argparse.ArgumentParser(prog="altavista", description="GMAT Three.js viewer")
    sub = p.add_subparsers(dest="cmd")

    s = sub.add_parser("serve", help="run the viewer server")
    s.add_argument("--host", default="0.0.0.0", help="bind address (default 0.0.0.0 = all interfaces)")
    s.add_argument("--port", type=int, default=8765)
    s.add_argument("--textures", default=None, help="folder of planet textures (default: GMAT install)")
    s.add_argument("--log-level", default="info")
    s.add_argument("--profile", default="design",
                    help="profiles/*.yaml id to read the globe's imagery source from (default: design)")
    # Round 3 (question 217(b)): where profiles/*.yaml actually lives -- matches --textures's own
    # shape (an optional path override, default None so altavista.profile.resolve_profiles_dir's
    # own ordered search picks the packaged copy or the in-repo copy on its own). Read once here,
    # as an ordinary CLI flag -- never the process environment (question 199) -- and passed
    # straight through to altavista.server.serve/create_app.
    s.add_argument("--profiles-dir", default=None,
                    help="folder of profiles/*.yaml (default: the packaged copy shipped in the "
                         "wheel, else the in-repo profiles/ next to this worktree)")
    # R3.5a (docs/aiplane-plan.md milestone A5's server half; question 199): configuration for
    # the /api/command/* routes (altavista/command_client.py) -- a real, already-running
    # av-command service's gRPC and admin-HTTP addresses, plus the entity ids the proposals
    # route sweeps by default. Read once here, as ordinary CLI flags -- never the process
    # environment -- and passed straight through to altavista.server.serve/create_app.
    s.add_argument("--command-endpoint", default=None,
                    help="host:port of a running av-command gRPC service (enables /api/command/*)")
    s.add_argument("--command-admin-endpoint", default=None,
                    help="host:port of that av-command service's admin HTTP server "
                         "(enables GET /api/command/counters)")
    s.add_argument("--command-entity", dest="command_entities", action="append", default=None,
                    help="entity id GET /api/command/proposals sweeps when no ?entity_id= is "
                         "given (repeatable)")

    r = sub.add_parser("run", help="run a GMAT script and publish it to the viewer")
    r.add_argument("script")
    r.add_argument("--name", default=None)
    r.add_argument("--frame", default="EarthMJ2000Eq")
    r.add_argument("--url", default=None)
    r.add_argument("--bodies", default=None, help="comma separated bodies to show (default: auto)")

    args = p.parse_args(argv)
    if args.cmd == "serve":
        from .server import serve
        serve(host=args.host, port=args.port, texture_dir=args.textures, log_level=args.log_level,
              profile=args.profile, command_endpoint=args.command_endpoint,
              command_admin_endpoint=args.command_admin_endpoint,
              command_entities=args.command_entities or [],
              profiles_dir=args.profiles_dir)
        return 0
    if args.cmd == "run":
        from .scenario import Scenario
        bodies = args.bodies.split(",") if args.bodies else None
        sc = Scenario.from_script(args.script, name=args.name, frame=args.frame, bodies=bodies)
        res = sc.publish(url=args.url)
        print(f"published {sc.name!r}: {len(sc.spacecraft_list)} spacecraft -> {res}")
        return 0
    p.print_help()
    return 1


if __name__ == "__main__":
    sys.exit(main())
