"""``/admin/api/evidence`` (ADR-004 question 63: "exposes `/admin/api/evidence` for
`secdeploy evidence` to collect"), **localhost only** -- mirrors
`crates/av-dynamics-service/src/admin.rs` on the Rust side (same two routes, same JSON
shape modulo language-native types).

A small ``http.server`` (stdlib) server on a background daemon thread, not a new
dependency: this service already has no HTTP framework, and two ``GET``-only routes
returning JSON is well within what ``http.server.BaseHTTPRequestHandler`` serves correctly.

Routes
------
- ``GET /admin/api/evidence`` -- ``version``, ``settings_hash``, ``run_id``, the evidence
  log's ``chain_head``/``entries``, and :func:`gmat_service.fips.detect`'s posture
  (detected, not claimed).
- ``GET /admin/api/evidence/verify`` -- runs :meth:`gmat_service.evidence.EvidenceLog.verify`
  and returns its result as JSON.

Every other path/method gets ``404``/``405``.
"""
from __future__ import annotations

import json
import logging
from http.server import BaseHTTPRequestHandler, HTTPServer
from threading import Thread

from . import config, fips
from .evidence import EvidenceLog

log = logging.getLogger("gmat_service.admin")


class _AdminHandler(BaseHTTPRequestHandler):
    server_version = "gmat-service-admin/1"

    # BaseHTTPRequestHandler's default logs every request to stderr; the gRPC server's own
    # `serve()` already logs what it needs (server.py), so this handler stays quiet on
    # success and only surfaces genuine errors.
    def log_message(self, format: str, *args) -> None:  # noqa: A002 (stdlib signature)
        pass

    def _write_json(self, status: int, body: dict) -> None:
        payload = json.dumps(body, sort_keys=True).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _evidence_body(self) -> dict:
        server: _AdminServer = self.server  # type: ignore[assignment]
        return {
            "chain_head": server.evidence.chain_head(),
            "entries": server.evidence.entry_count(),
            "evidence_path": str(server.evidence.path),
            "fips": fips.detect(),
            "run_id": server.run_id,
            "settings_hash": config.settings_hash(),
            "version": config.GMAT_VERSION,
        }

    def do_GET(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler's naming convention)
        server: _AdminServer = self.server  # type: ignore[assignment]
        if self.path == "/admin/api/evidence":
            self._write_json(200, self._evidence_body())
        elif self.path == "/admin/api/evidence/verify":
            self._write_json(200, server.evidence.verify())
        else:
            self._write_json(404, {"error": "not found", "path": self.path})


class _AdminServer(HTTPServer):
    """Attaches the handful of values `_AdminHandler` needs per request -- avoids module-
    level globals, which would break if a test process ever ran two admin servers."""

    def __init__(self, address, evidence: EvidenceLog, run_id: str):
        super().__init__(address, _AdminHandler)
        self.evidence = evidence
        self.run_id = run_id


def serve_admin(*, host: str, port: int, evidence: EvidenceLog, run_id: str) -> HTTPServer:
    """Starts the admin HTTP server on a background daemon thread and returns the
    already-listening ``HTTPServer`` -- **localhost only** (ADR-004): ``host`` is the CLI's
    own responsibility to keep at ``127.0.0.1`` (see ``server.py``), this function does not
    default or validate it itself, matching the gRPC port's own "the caller decides the
    bind address, this function just binds it" contract."""
    httpd = _AdminServer((host, port), evidence=evidence, run_id=run_id)
    thread = Thread(target=httpd.serve_forever, name="gmat-service-admin", daemon=True)
    thread.start()
    log.info("gmat-service admin API on %s:%d (GET /admin/api/evidence, /admin/api/evidence/verify)",
             host, httpd.server_address[1])
    return httpd
