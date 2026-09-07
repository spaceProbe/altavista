"""``.venv/bin/python -m gmat_service [--port P] [--evidence-path PATH] [--run-id ID]``

Run from ``services/gmat-service`` (or with that directory prepended to ``PYTHONPATH``) so
``import gmat_service`` resolves -- see this package's README.
"""
from __future__ import annotations

import sys

from .server import main

if __name__ == "__main__":
    sys.exit(main())
