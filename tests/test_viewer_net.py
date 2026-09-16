"""Question 168 (docs/open-questions.md): "five WebSocket connection errors are logged
on every page load before the client reports connected -- even against a server that
has been up for hours." Root cause and fix are in web/js/net.js's own module docstring
(read that first -- this file only asserts the OBSERVABLE requirement: "a headless test
asserts zero console errors on load against a live server").

No new dependency: drives a REAL, isolated, headless Chrome directly over the DevTools
Protocol using the `websockets` package (already installed for spoore-io/av-grpc work),
the same way `tests/test_cdm_run.py`/`tests/test_viewer_*.py` shell out to `node` rather
than reimplementing viewer logic in Python. A fresh `--user-data-dir` per run means no
browser state (cookies, cached "IPv6 vs IPv4 for this host" preference, module cache)
survives between test runs or leaks from a developer's own Chrome profile.

Root-cause investigation summary (full detail in web/js/REPORT_M26_5.md): the ONE
reproducible mechanism found, across dozens of trials with a real live server, a fresh
browser tab against an already-warm server, and a genuine server+browser co-launch race
(with the server's own startup artificially delayed to widen the window), was a
DIFFERENT, always-reproducible bug: this page has no <link rel="icon">, so every
browser auto-requests /favicon.ico, which 404s and logs its own "Failed to load
resource" console error on every single load -- fixed in web/index.html. The
WebSocket-specific race net.js's own fix targets (a connection attempted before the
server is confirmed ready) could NOT be reproduced against a warm server in this
environment despite the same measurement effort; that limitation is disclosed, not
papered over -- see the report for every experiment run and its result.
"""
from __future__ import annotations

import asyncio
import base64
import json
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path
from types import SimpleNamespace
from typing import List, Sequence

import grpc
import pytest

from altavista.pb import authority_pb2, authority_pb2_grpc
from altavista.test_env import drain_after_terminate
from altavista.pb.altavista.v1 import command_pb2

REPO_ROOT = Path(__file__).resolve().parent.parent

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
    "chromium-browser",
]


def _find_chrome() -> str | None:
    for candidate in CHROME_CANDIDATES:
        if candidate.startswith("/"):
            if Path(candidate).exists():
                return candidate
        else:
            found = shutil.which(candidate)
            if found:
                return found
    return None


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class _LiveServer:
    def __init__(self, port: int, proc: subprocess.Popen):
        self.port = port
        self.proc = proc
        self.url = f"http://127.0.0.1:{port}/"

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def _start_altavista_server(extra_args: Sequence[str] = ()) -> _LiveServer:
    """Start a REAL `python -m altavista serve` subprocess and poll `/api/health` until
    it is confirmed ready -- the exact readiness-polling pattern question 168's own
    investigation found load-bearing (see this module's docstring): navigating BEFORE a
    server is confirmed listening is a real, separate race. `extra_args` (question
    209(c)'s own browser check below) lets a caller add `--profile execution
    --command-endpoint ... --command-admin-endpoint ... --command-entity ...` without a
    second copy of this startup/readiness-polling logic -- `live_server` below (the
    ordinary, no-command-service case) and `execution_command_live_server` (Part 5) both
    call this one function.
    """
    port = _free_port()
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port), *extra_args],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT),
    )
    server = _LiveServer(port, proc)
    deadline = time.monotonic() + 15
    ready = False
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=0.5) as r:
                if r.status == 200:
                    ready = True
                    break
        except Exception:
            time.sleep(0.05)
    if not ready:
        server.stop()
        pytest.fail("altavista server did not become ready within 15s")
    return server


@pytest.fixture()
def live_server():
    """A REAL altavista server, started and confirmed ready (polling /api/health)
    before the test ever navigates a browser to it -- deliberately correct test
    hygiene, not an accident: question 168's own investigation (see this module's
    docstring) found that navigating BEFORE a server is confirmed listening is a real,
    separate race, and conflating that with "the client logs errors against a live
    server" would make this test flaky for the wrong reason. "Live" means exactly what
    the brief asks: a server already up when the browser connects.
    """
    server = _start_altavista_server()
    try:
        yield server
    finally:
        server.stop()


async def _collect_console_errors(
    url: str, chrome_path: str, wait_s: float = 5.0, eval_js: str | None = None
) -> tuple[list[str], object]:
    """Navigate a fresh, isolated headless Chrome to `url` and return every console
    error seen within `wait_s` seconds of navigation starting -- THREE distinct CDP
    signals, all real, browser-native failure channels:
      - `Log.entryAdded` with level 'error' -- where a browser-native failure like a
        failed WebSocket connection or a 404 resource load actually appears, verified
        directly while investigating question 168;
      - `Runtime.consoleAPICalled` with type 'error', i.e. an explicit
        `console.error(...)` call;
      - `Runtime.exceptionThrown` -- an UNCAUGHT exception during page JS execution
        (e.g. a synchronous `TypeError` thrown inside a WebSocket `onmessage` handler).
        Added for question 209(c)'s own browser check, which found this collector
        previously missed this class of failure entirely: an uncaught exception is
        delivered ONLY via this event, never via `Log.entryAdded`/`Runtime.
        consoleAPICalled` -- confirmed live, not assumed (this repo's own R4.2
        scratchpad log records a run where the pre-209(c) `scene.js`'s real,
        reproducible `TypeError` on `sc.frame.name` produced a real page crash but
        `errors == []` from this function, before this case was added).
    A fresh `--user-data-dir` per call is the whole point: no cached DNS/
    connection-pool state from an earlier run or a developer's own profile can make
    this test's result depend on browser history.

    `eval_js` (question 209(c)'s own browser check, `test_viewer_net.py`'s Part 5
    below) -- when given, a `Runtime.evaluate` (`returnByValue: True`) is sent AFTER
    `wait_s` has elapsed (so any async work the page kicked off on load has had time to
    settle) and its value is returned as this function's second element; `None` when
    `eval_js` is omitted (every pre-existing call site, unaffected). Reuses this exact
    CDP session/`reader()` loop rather than opening a second one -- there is only ever
    ONE consumer of `ws`'s messages, so the eval's own response (matched by request id)
    is picked out inside the SAME `reader()` that already demultiplexes console/log
    events, never a second websocket connection or a second Chrome subprocess.
    """
    with tempfile.TemporaryDirectory() as profile_dir:
        cdp_port = _free_port()
        chrome = subprocess.Popen([
            chrome_path, f"--user-data-dir={profile_dir}", "--headless=new",
            f"--remote-debugging-port={cdp_port}", "--no-first-run",
            "--no-default-browser-check", "--disable-extensions", "about:blank",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            info = None
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/version", timeout=0.5) as r:
                        info = json.loads(r.read())
                        break
                except Exception:
                    await asyncio.sleep(0.05)
            if info is None:
                raise RuntimeError("headless Chrome did not expose a DevTools endpoint in time")

            import websockets

            async with websockets.connect(info["webSocketDebuggerUrl"], max_size=None) as bws:
                await bws.send(json.dumps({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}))
                target_id = None
                async for raw in bws:
                    msg = json.loads(raw)
                    if msg.get("id") == 1:
                        target_id = msg["result"]["targetId"]
                        break
                plist = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/list", timeout=1).read())
                page = next(p for p in plist if p.get("id") == target_id)

                async with websockets.connect(page["webSocketDebuggerUrl"], max_size=None) as ws:
                    mid = [1]

                    async def send(method, params=None):
                        mid[0] += 1
                        await ws.send(json.dumps({"id": mid[0], "method": method, "params": params or {}}))

                    await send("Runtime.enable")
                    await send("Log.enable")
                    await send("Page.enable")

                    errors: list[str] = []
                    eval_state: dict = {"want_id": None, "done": False, "value": None}

                    async def reader():
                        async for raw in ws:
                            msg = json.loads(raw)
                            method = msg.get("method")
                            if method == "Log.entryAdded":
                                entry = msg["params"]["entry"]
                                if entry.get("level") == "error":
                                    errors.append(f"[Log:{entry.get('source')}] {entry.get('text')} ({entry.get('url')})")
                            elif method == "Runtime.consoleAPICalled":
                                if msg["params"].get("type") == "error":
                                    errors.append(f"[console.error] {msg['params'].get('args')}")
                            elif method == "Runtime.exceptionThrown":
                                # Question 209(c)'s own browser check found this gap: an
                                # UNCAUGHT exception thrown synchronously inside a page
                                # event handler (e.g. `net.js`'s `onmessage` calling
                                # `loadScenario()`, which used to throw on
                                # `sc.frame.name` for a `frame`-less scenario) is a real,
                                # distinct CDP event -- `Runtime.exceptionThrown` --
                                # never delivered as `Log.entryAdded`/`Runtime.
                                # consoleAPICalled` at all. Before this fix, this
                                # collector silently missed it: this exact scenario
                                # (`test_empty_scenario_publish_does_not_crash_the_
                                # viewer_and_the_command_console_refreshes` run against
                                # the pre-209(c) `scene.js`) reported ZERO console
                                # errors despite a real uncaught `TypeError` on every
                                # load -- captured live, see this repo's own R4.2
                                # scratchpad log for that run's output. Handling it here
                                # closes the gap for every test in this file, not just
                                # the one that found it.
                                details = msg.get("params", {}).get("exceptionDetails", {})
                                text = details.get("text", "exception")
                                exc = details.get("exception") or {}
                                description = exc.get("description") or exc.get("value") or ""
                                errors.append(f"[Runtime.exceptionThrown] {text}: {description}")
                            elif msg.get("id") is not None and msg["id"] == eval_state["want_id"]:
                                result = msg.get("result", {}).get("result", {})
                                eval_state["value"] = result.get("value")
                                eval_state["done"] = True

                    reader_task = asyncio.create_task(reader())
                    await send("Page.navigate", {"url": url})
                    await asyncio.sleep(wait_s)

                    eval_value = None
                    if eval_js:
                        eval_id = mid[0] + 1
                        eval_state["want_id"] = eval_id
                        await send("Runtime.evaluate", {"expression": eval_js, "returnByValue": True})
                        deadline = time.monotonic() + 5.0
                        while time.monotonic() < deadline and not eval_state["done"]:
                            await asyncio.sleep(0.05)
                        eval_value = eval_state["value"]

                    reader_task.cancel()
                    try:
                        await reader_task
                    except asyncio.CancelledError:
                        pass
                    return errors, eval_value
        finally:
            chrome.terminate()
            try:
                chrome.wait(timeout=5)
            except subprocess.TimeoutExpired:
                chrome.kill()


def test_zero_console_errors_on_load_against_a_live_server(live_server):
    """Question 168's required test, verbatim: "a headless test asserts zero console
    errors on load against a live server." Fails against the pre-M26.5 page (missing
    favicon link, and -- in whatever environment can reproduce the race net.js's own
    fix targets -- an eagerly-connected WebSocket): any console error logged between
    navigation and this function returning is a failure, not just ones that happen to
    match a specific expected substring (question 164's rule: a diagnostic that matches
    the wrong banner substring can miss a real failure, or in this case, could hide one
    by filtering too narrowly).

    Proven by breaking the code and watching this exact test fail (recorded in
    web/js/REPORT_M26_5.md, reproducible by anyone): reverting web/index.html's
    `<link rel="icon">` line reintroduces a real, 100%-reproducible "Failed to load
    resource: 404 (favicon.ico)" console error on every load, and this test fails
    against that reverted file; restoring the line makes it pass again.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, _ = asyncio.run(_collect_console_errors(live_server.url, chrome_path, wait_s=5.0))
    assert errors == [], (
        f"expected zero console errors loading {live_server.url}, got {len(errors)}:\n" +
        "\n".join(errors)
    )


def test_console_error_collector_actually_detects_a_real_failure():
    """Teeth proof for the DETECTION MECHANISM `test_zero_console_errors_on_load_...`
    depends on (question 164's rule: pin a pass condition against a captured real
    artifact before trusting it -- a collector that silently misses real errors would
    make the test above pass for the wrong reason). Points the exact same
    `_collect_console_errors` helper at a genuinely closed port -- no altavista server
    involved, no app code touched -- and asserts it reports a real, recognizable
    WebSocket connection failure. This is the same failure shape (a browser-native,
    unsuppressable network-level console error) question 168's own investigation
    captured live while root-causing the reported bug (web/js/REPORT_M26_5.md).
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    with tempfile.TemporaryDirectory() as d:
        page = Path(d) / "fail.html"
        page.write_text(
            "<!doctype html><title>t</title><script>"
            "new WebSocket('ws://127.0.0.1:1/ws');"
            "</script>"
        )
        errors, _ = asyncio.run(_collect_console_errors(page.as_uri(), chrome_path, wait_s=2.0))
    assert any("WebSocket" in e for e in errors), (
        f"expected the collector to report a WebSocket connection failure; got {errors!r}"
    )


# ============================================================================== Part 5
# Question 209(c): `viewer.setScenario` must not throw on a scenario with no bodies or
# frames -- an empty `{"name", "spacecraft": []}` publish used to crash
# `_buildFrameGraph` in web/js/scene.js and abort the rest of `loadScenario`
# (web/js/app.js), which is how the command console's own refresh (Part 5's own real
# proof target) was silently skipped during the lead's browser drive: one more failure
# that left no trace (question 148). This section proves the fix end to end, in a REAL
# headless browser, against a REAL running server in the execution profile with a REAL
# `av-command` service configured -- reusing every piece of `test_zero_console_errors_
# on_load_against_a_live_server`'s own machinery above (`_find_chrome`,
# `_collect_console_errors`, `_start_altavista_server`) rather than a second copy, and
# the `command_service` fixture SHAPE from tests/test_viewer_command_panel.py
# (duplicated, not imported -- this repo's existing viewer test files each stay
# self-contained, that file's own module docstring, restated here).
import os  # noqa: E402  (grouped with this section, which is the only place it's used)

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
READY_TIMEOUT_S = 90.0
TEST_ISSUER = "https://sso.test.example/"
TEST_AUDIENCE = "av-command"
EMPTY_SCENARIO_ENTITY = "sat-empty-scenario-proof"


def _cargo_env() -> dict:
    """A COPY of the process environment with rustup prefixed onto `PATH`, passed as
    the child process's own `env=` -- never `os.environ[...] = ...` (question 199: no
    test mutates the process environment)."""
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


class LocalTestIssuer:
    """Duplicated verbatim (shape) from tests/test_command_console_routes.py's own
    class of the same name -- see that module's own docstring for the full "why
    openssl, not a new Python dependency" rationale."""

    def __init__(self, tmp_path: Path) -> None:
        self.private_key_path = tmp_path / "issuer_private.pem"
        self.public_key_path = tmp_path / "issuer_public.pem"
        subprocess.run(["openssl", "genrsa", "-out", str(self.private_key_path), "2048"], check=True, capture_output=True)
        subprocess.run(
            ["openssl", "rsa", "-in", str(self.private_key_path), "-pubout", "-out", str(self.public_key_path)], check=True, capture_output=True
        )

    def mint(self, claims: dict) -> str:
        header_b64 = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode("utf-8"))
        payload_b64 = _b64url(json.dumps(claims, separators=(",", ":")).encode("utf-8"))
        signing_input = f"{header_b64}.{payload_b64}"
        proc = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(self.private_key_path)], input=signing_input.encode("ascii"), check=True, capture_output=True
        )
        signature_b64 = _b64url(proc.stdout)
        return f"{signing_input}.{signature_b64}"


def _valid_claims(sub: str, groups: List[str]) -> dict:
    now = int(time.time())
    return {"iss": TEST_ISSUER, "aud": TEST_AUDIENCE, "sub": sub, "iat": now, "exp": now + 3600, "groups": groups, "amr": [], "acr": "", "jti": "test-jti"}


@pytest.fixture(scope="module")
def command_bin():
    proc = subprocess.run(["cargo", "build", "-p", "av-command", "--bin", "av-command"], cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-command failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "av-command"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def issuer(tmp_path_factory):
    return LocalTestIssuer(tmp_path_factory.mktemp("av_net_empty_scenario_issuer"))


@pytest.fixture(scope="module")
def command_service(command_bin, issuer, tmp_path_factory):
    """A REAL, running `av-command` service (same shape as tests/test_viewer_command_
    panel.py's own `command_service` fixture, duplicated per this repo's
    self-contained-test-file convention) -- this Part's own browser check points a
    REAL `python -m altavista serve --profile execution` subprocess at it via
    `--command-endpoint`/`--command-admin-endpoint`."""
    tmp = tmp_path_factory.mktemp("av_net_empty_scenario_service")
    ledger_dir = tmp / "ledger"
    policy_dir = REPO_ROOT / "profiles" / "policies" / "authority"
    profile_path = tmp / "profile.yaml"
    profile_path.write_text(
        "authority:\n"
        "  roles:\n"
        "    operators: [\"mode\"]\n"
        "  mfa_amr_methods: []\n"
        "  mfa_acr: \"\"\n"
        "  service_roles: {}\n"
        "  delegations_path: \"\"\n"
    )
    grpc_port = _free_port()
    admin_port = _free_port()
    proc = subprocess.Popen(
        [
            str(command_bin),
            "--bind", f"127.0.0.1:{grpc_port}",
            "--admin-bind", f"127.0.0.1:{admin_port}",
            "--ledger-dir", str(ledger_dir),
            "--policy-dir", str(policy_dir),
            "--profile-path", str(profile_path),
            "--oidc-issuer", TEST_ISSUER,
            "--oidc-audience", TEST_AUDIENCE,
            "--oidc-public-key-path", str(issuer.public_key_path),
            "--run-id", "test_viewer_net_empty_scenario",
        ],
        cwd=str(REPO_ROOT), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    grpc_endpoint = f"127.0.0.1:{grpc_port}"
    admin_endpoint = f"127.0.0.1:{admin_port}"
    channel = grpc.insecure_channel(grpc_endpoint)
    try:
        try:
            grpc.channel_ready_future(channel).result(timeout=READY_TIMEOUT_S)
        except Exception as e:
            channel.close()
            returncode = proc.poll()
            # `proc.stdout.read()` here is a readall on a subprocess that is still running
            # (a readiness wait times out precisely when the child is alive), so it used to
            # block FOREVER -- measured, 34 minutes, in P5 round 3's acceptance gate. See
            # `altavista.test_env.drain_after_terminate`'s own doc for the measurement.
            output = drain_after_terminate(proc)
            pytest.fail(f"av-command subprocess did not become ready within {READY_TIMEOUT_S}s (returncode={returncode}): {e}\n--- subprocess output ---\n{output}")
        channel.close()
        yield SimpleNamespace(grpc_endpoint=grpc_endpoint, admin_endpoint=admin_endpoint, proc=proc)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def _propose(command_service, command_id: str, entity_id: str, command_class: str, rationale: str):
    """A real `Propose` gRPC call, checked automatically (question 209(a)) -- seeds one
    real, real-rationale command this Part's browser check can look for in the
    panel's own real DOM after a real scenario load."""
    channel = grpc.insecure_channel(command_service.grpc_endpoint)
    try:
        stub = authority_pb2_grpc.CommandAuthorityServiceStub(channel)
        command = command_pb2.Command(id=command_id, entity_id=entity_id, command_class=command_class)
        proposal = command_pb2.CommandProposal(command=command, rationale=rationale, evidence_ids=[])
        return stub.Propose(authority_pb2.ProposeRequest(proposal=proposal, principal="model-x"))
    finally:
        channel.close()


@pytest.fixture()
def execution_command_live_server(command_service):
    """A REAL `python -m altavista serve` subprocess in the EXECUTION profile, with a
    REAL `av-command` endpoint configured -- `_start_altavista_server`'s own
    `extra_args`, never a second copy of the startup/readiness-polling logic."""
    server = _start_altavista_server([
        "--profile", "execution",
        "--command-endpoint", command_service.grpc_endpoint,
        "--command-admin-endpoint", command_service.admin_endpoint,
        "--command-entity", EMPTY_SCENARIO_ENTITY,
    ])
    try:
        yield server
    finally:
        server.stop()


def test_empty_scenario_publish_does_not_crash_the_viewer_and_the_command_console_refreshes(
    execution_command_live_server, command_service,
):
    """Question 209(c), proven in a REAL headless browser against a REAL running
    server:

    1. A REAL command is proposed (auto-Checked, question 209(a)) against the REAL
       `av-command` service this server was started with -- `cmd-empty-scenario-proof`,
       with a distinctive real rationale text.
    2. `{"name": "<something>", "spacecraft": []}` -- NO `frame`, NO `bodies` -- is
       published through the REAL `POST /api/scenario` route.
    3. A REAL headless Chrome loads the page and is given `wait_s` to run
       `net.js`'s WebSocket handshake, receive the `scenario` message, and run
       `web/js/app.js`'s `loadScenario()` to completion.
    4. Zero console errors -- the same question 168 gate every other test in this file
       enforces, unchanged (`_collect_console_errors`'s own docstring above). Before
       this question's fix, `_buildFrameGraph` threw a `TypeError` on `sc.frame.name`
       (`sc.frame` is absent for this scenario shape), which -- being inside
       `loadScenario`'s own synchronous call chain, driven from `net.onMessage`, with
       no surrounding `try`/`catch` -- surfaces as an UNCAUGHT exception, delivered by
       Chrome as `Runtime.exceptionThrown` (a real CDP event this exact investigation
       found `_collect_console_errors` did not yet watch for -- see that function's own
       docstring for the fix and the captured stack trace this test's own "before" run
       produced), so this assertion alone is now already a real regression guard for
       the crash itself.
    5. **The direct observation that `loadScenario` reached the command-console
       refresh** (the actual defect: a crash that aborted `loadScenario` silently
       skipped this, with nothing else in the page saying so): `document.
       getElementById('panel-command-console').textContent` is read directly out of
       the REAL page's REAL DOM (`web/js/app.js`'s `els.commandPanel`, a static,
       always-present element -- see web/index.html -- that `render()` from
       web/js/panels/command_panel.js writes into on every `renderCommandPanelNow()`
       call, regardless of whether that pane is part of the CURRENT tiling layout).
       This text can only contain the real command's real rationale if `viewer.
       setScenario(sc)` returned normally, `loadScenario` reached its own
       `isExecutionProfile(sc)` branch, and the real `GET /api/command/proposals`
       fetch completed and re-rendered the panel -- there is no other code path that
       writes this text into this element. This is chosen over a server-side access
       log or a request-counting middleware because it proves the actual, specific
       claim (`loadScenario` reached the refresh) rather than merely "some GET request
       for that path arrived eventually", which a middleware could satisfy even if the
       arriving request came from something else entirely.
    """
    proposed_rationale = "empty-scenario-proof: the command console must still refresh"
    _propose(command_service, "cmd-empty-scenario-proof", EMPTY_SCENARIO_ENTITY, "mode", proposed_rationale)

    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")

    scenario_body = json.dumps({"name": "empty-console-scenario", "spacecraft": []}).encode("utf-8")
    req = urllib.request.Request(
        f"{execution_command_live_server.url}api/scenario", data=scenario_body,
        headers={"content-type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=5) as resp:
        assert resp.status == 200, resp.read()

    errors, panel_text = asyncio.run(_collect_console_errors(
        execution_command_live_server.url, chrome_path, wait_s=5.0,
        eval_js="document.getElementById('panel-command-console') "
                "? document.getElementById('panel-command-console').textContent : null",
    ))
    assert errors == [], (
        f"expected zero console errors loading an empty-scenario publish, got {len(errors)}:\n" +
        "\n".join(errors)
    )
    assert panel_text is not None, "expected #panel-command-console to exist in the real page"
    assert proposed_rationale in panel_text, (
        "expected the command console's real DOM text to show the real proposed command's "
        f"rationale after the empty-scenario load (proof that loadScenario reached the "
        f"command-console refresh) -- got: {panel_text!r}"
    )
