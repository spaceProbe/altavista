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
              profile=args.profile)
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
