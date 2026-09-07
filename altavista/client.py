"""Publish scenarios to a running viewer server (stdlib only, no extra deps)."""
from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Optional, Union

from .model import ScenarioData

DEFAULT_URL = os.environ.get("ALTAVISTA_URL", "http://127.0.0.1:8765")


class ViewerError(RuntimeError):
    pass


def _post(url: str, payload: dict, timeout: float = 30.0) -> dict:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode() or "{}")
    except urllib.error.URLError as e:
        raise ViewerError(f"cannot reach viewer server at {url}: {e.reason}. "
                          f"Start it with: python -m altavista serve") from e


def publish(scenario: Union[ScenarioData, dict], url: Optional[str] = None) -> dict:
    """Send a scenario to the viewer; every connected browser updates immediately."""
    base = (url or DEFAULT_URL).rstrip("/")
    payload = scenario.to_dict() if isinstance(scenario, ScenarioData) else scenario
    return _post(base + "/api/scenario", payload)


def set_clock(t: float, playing: Optional[bool] = None, speed: Optional[float] = None,
              scenario: Optional[str] = None, url: Optional[str] = None) -> dict:
    """Move every browser's playback clock to A1MJD ``t`` (optionally set play state/speed).

    ``scenario`` names the scenario the clock applies to (default: the latest published).
    """
    base = (url or DEFAULT_URL).rstrip("/")
    payload = {"t": t}
    if playing is not None:
        payload["playing"] = playing
    if speed is not None:
        payload["speed"] = speed
    if scenario is not None:
        payload["scenario"] = scenario
    return _post(base + "/api/clock", payload)


def remove(name: str, url: Optional[str] = None) -> dict:
    base = (url or DEFAULT_URL).rstrip("/")
    req = urllib.request.Request(f"{base}/api/scenario/{name}", method="DELETE")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode() or "{}")
    except urllib.error.URLError as e:
        raise ViewerError(f"cannot reach viewer server at {base}: {e.reason}") from e


def is_running(url: Optional[str] = None) -> bool:
    base = (url or DEFAULT_URL).rstrip("/")
    try:
        with urllib.request.urlopen(base + "/api/health", timeout=2) as resp:
            return resp.status == 200
    except Exception:
        return False
