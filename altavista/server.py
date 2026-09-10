"""Viewer server: serves the Three.js frontend, stores scenarios, and pushes updates to
every connected browser over a WebSocket.

The server does **not** load GMAT. Python sessions that build scenarios ``publish()``
JSON to it over HTTP, so the GMAT singleton stays in the user's own process and any
number of browsers (on any machine that can reach the server) stay in sync.

Endpoints
---------
GET  /                      viewer
GET  /api/scenarios         list of scenario names
GET  /api/scenario/{name}   scenario JSON
POST /api/scenario          publish/replace a scenario (JSON body, ``name`` field)
DELETE /api/scenario/{name}
POST /api/cdm/trajectory    publish an altavista.v1.Trajectory (binary protobuf or JSON
                            transcoding); converted to the viewer JSON shape and pushed
                            like POST /api/scenario (M1.3, altavista.cdm)
POST /api/cdm/run           publish an altavista.v1.RunProducts (binary protobuf or JSON
                            transcoding) from crates/av-run; converted and pushed like
                            POST /api/cdm/trajectory (M17.2, question 121)
POST /api/cdm/sweep         publish an altavista.v1.SweepResults (binary protobuf or JSON
                            transcoding) from crates/av-sweep; converted and pushed like
                            POST /api/cdm/run (F3, docs/feasibility-plan.md, question 191)
POST /api/cdm/sweep/sample  open one already-run sample of a published sweep by identity
                            ({"sweepId", "pointIndex", "drawIndex"}, never a path) -- reads
                            <productsUri>/run_products.pb on this host (the productsUri
                            this server itself recorded, never one the caller supplies) and
                            publishes/broadcasts it like POST /api/cdm/run (F3c, this
                            task's own defect fix for F3's "opens any sample ... through
                            its products_uri")
POST /api/clock             broadcast a playback clock to all browsers
GET  /textures/{file}       planet textures from the GMAT install
WS   /ws                    push channel (scenario, list, clock messages)
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
from pathlib import Path
from typing import Dict, List, Optional, Set

from fastapi import FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse, JSONResponse, Response
from fastapi.staticfiles import StaticFiles
from google.protobuf import json_format
from google.protobuf.message import DecodeError

from . import cdm as cdm_adapter
from . import profile as profile_loader
from .model import Frame, ScenarioData
from .pb import core_pb2, trajectory_pb2
# run_pb2 is not (yet) re-exported by altavista/pb/__init__.py's own explicit import list (that
# file is not owned by this task) -- imported directly from its real module path, the same way
# services/lockstep-ref/lockstep_ref/server.py already imports lockstep_pb2.
from .pb.altavista.v1 import run_pb2

log = logging.getLogger("altavista.server")

WEB_DIR = Path(__file__).resolve().parent.parent / "web"
DEFAULT_PORT = 8765


class RevalidatingStaticFiles(StaticFiles):
    """Serves ``web/`` with ``Cache-Control: no-cache`` so every load revalidates
    against the server instead of trusting the browser's heuristic freshness
    (question 139, M21.1): after M20 landed, a browser held onto a cached
    ``cdm_run.js`` while loading a fresh ``app.js``, and the viewer died at module
    import with a stale export. ``no-cache`` means "revalidate before use", not
    "don't store" -- an unchanged file still comes back as a 304 via the ETag
    Starlette's ``StaticFiles`` already computes and already answers
    ``If-None-Match`` with; this override only adds the missing header onto
    whichever response (200 or 304) Starlette produces, rather than reimplementing
    conditional-request handling.
    """

    def file_response(self, *args, **kwargs) -> Response:
        response = super().file_response(*args, **kwargs)
        response.headers["Cache-Control"] = "no-cache"
        return response


class Hub:
    """In-memory scenario store + WebSocket fan-out."""

    def __init__(self, imagery: Optional[dict] = None) -> None:
        self.scenarios: Dict[str, dict] = {}
        self.order: List[str] = []
        self.clients: Set[WebSocket] = set()
        self.clock: Optional[dict] = None
        self._lock = asyncio.Lock()
        # M19.5 (question 132): the active profile's globe imagery source
        # ({"urlTemplate", "attribution", "maxLevel"}, altavista.profile.load_imagery_
        # config) -- put() stamps it onto every scenario that doesn't already carry one,
        # so every publish path (POST /api/scenario, /api/cdm/trajectory, /api/cdm/run)
        # gets it uniformly from this single point, without each handler having to know
        # about profiles at all.
        self.imagery = imagery

    # -- scenario store -------------------------------------------------
    def put(self, scenario: dict) -> str:
        name = str(scenario.get("name") or "scenario")
        if self.imagery is not None and "imagery" not in scenario:
            scenario["imagery"] = self.imagery
        if name not in self.scenarios:
            self.order.append(name)
        self.scenarios[name] = scenario
        return name

    def remove(self, name: str) -> bool:
        if name not in self.scenarios:
            return False
        del self.scenarios[name]
        self.order.remove(name)
        return True

    def names(self) -> List[str]:
        return list(self.order)

    # -- fan-out ----------------------------------------------------------
    async def broadcast(self, message: dict, exclude: Optional[WebSocket] = None) -> None:
        data = json.dumps(message)
        dead = []
        for ws in list(self.clients):
            if ws is exclude:
                continue
            try:
                await ws.send_text(data)
            except Exception:
                dead.append(ws)
        for ws in dead:
            self.clients.discard(ws)

    async def send_state(self, ws: WebSocket) -> None:
        await ws.send_text(json.dumps({"type": "list", "names": self.names()}))
        if self.order:
            latest = self.order[-1]
            await ws.send_text(json.dumps({"type": "scenario", "scenario": self.scenarios[latest]}))
        if self.clock and self.order and self.clock.get("scenario") == self.order[-1]:
            await ws.send_text(json.dumps({"type": "clock", **self.clock}))


def create_app(texture_dir: Optional[os.PathLike] = None, web_dir: Optional[os.PathLike] = None,
                profile: str = profile_loader.DEFAULT_PROFILE_ID) -> FastAPI:
    """``profile`` (M19.5, question 132): which ``profiles/*.yaml`` file's ``imagery:``
    section every published scenario gets stamped with (``Hub.put``) -- defaults to the
    "design" profile (altavista has no running "current profile" console yet; see
    ``altavista/profile.py``'s module docstring). Raises ``altavista.profile.ProfileError``
    (never silently falls back) if the named profile has no usable ``imagery:`` section.
    """
    app = FastAPI(title="altavista")
    hub = Hub(imagery=profile_loader.load_imagery_config(profile))
    app.state.hub = hub
    web = Path(web_dir) if web_dir else WEB_DIR
    textures = Path(texture_dir) if texture_dir else _default_texture_dir()
    # Shared with the app.mount(...) below (question 139, M21.1): the "/" route is a
    # separate FileResponse and does not inherit StaticFiles behaviour, so index.html
    # is served through the same RevalidatingStaticFiles.file_response Starlette uses
    # for every other web/ asset, rather than a second, hand-rolled cache/ETag path.
    web_static = RevalidatingStaticFiles(directory=str(web))

    @app.get("/")
    async def index(request: Request):
        index_path = web / "index.html"
        return web_static.file_response(index_path, os.stat(index_path), request.scope)

    @app.get("/api/scenarios")
    async def list_scenarios():
        return {"names": hub.names()}

    @app.get("/api/scenario/{name}")
    async def get_scenario(name: str):
        if name not in hub.scenarios:
            raise HTTPException(404, f"no scenario named {name!r}")
        return JSONResponse(hub.scenarios[name])

    @app.post("/api/scenario")
    async def publish(request: Request):
        scenario = await request.json()
        if not isinstance(scenario, dict) or "spacecraft" not in scenario:
            raise HTTPException(400, "expected a scenario object with a 'spacecraft' list")
        name = hub.put(scenario)
        if hub.clock and hub.clock.get("scenario") != name:
            hub.clock = None  # a stale clock from another scenario must not be replayed
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "scenario", "scenario": scenario})
        log.info("published scenario %r (%d spacecraft, %d bodies) to %d client(s)",
                 name, len(scenario.get("spacecraft", [])), len(scenario.get("bodies", [])), len(hub.clients))
        return {"ok": True, "name": name, "clients": len(hub.clients)}

    @app.post("/api/cdm/trajectory")
    async def publish_cdm_trajectory(request: Request):
        """Accept an ``altavista.v1.Trajectory`` (M1.3, ``altavista.cdm``), either binary
        protobuf (``Content-Type: application/x-protobuf``) or JSON transcoding (any other
        content type, via ``google.protobuf.json_format``), convert it to the viewer's JSON
        shape (m -> km, TAI ns -> A1MJD) and publish/broadcast it exactly like
        ``POST /api/scenario`` does.

        **Frames alongside (M17.1, question 122).** ``altavista.v1.Trajectory`` itself has
        no ``frames`` field (``proto/altavista/v1/trajectory.proto``, read-only to this
        task -- "no proto change authorized"), and the binary-protobuf body is exactly the
        bytes of one ``Trajectory`` message, with no room to smuggle a second message in
        without a new declared wrapper type. So there is genuinely no existing message-level
        envelope this endpoint could reuse for the binary path; it stays a bare
        ``Trajectory``, unchanged, and carries no frames -- the caller wanting real,
        GMAT-validated frames for a bare trajectory should use ``POST /api/cdm/run`` (a real
        ``RunProducts.frames``) instead.

        The **JSON-transcoding path**, which already parses the body as a JSON document
        rather than raw protobuf bytes, additively accepts a second shape: a JSON object
        with a top-level ``"trajectory"`` key -- ``{"trajectory": <Trajectory JSON>,
        "frames": [<FrameDefinition JSON>, ...]}``. This is a plain JSON convention (no new
        ``.proto`` message), safe to distinguish from a bare ``Trajectory`` body because
        none of ``Trajectory``'s own camelCase JSON field names is ``"trajectory"`` (``id``,
        ``entityId``, ``stateSpaceId``, ``frameId``, ``interpolation``, ``samples``,
        ``segments``, ``eventIds``, ``label``, ``provenance``, ``configHash``) -- a bare
        ``Trajectory`` JSON body (every existing caller) is therefore never misread as an
        envelope, and an envelope body is never misread as a (nonsensical) ``Trajectory``
        with a stray ``trajectory`` field. ``frames`` (default ``[]`` when the envelope
        omits it) is threaded through ``altavista.cdm.frames_to_viewer_json`` exactly like
        ``POST /api/cdm/run`` above.
        """
        content_type = (request.headers.get("content-type") or "").split(";", 1)[0].strip().lower()
        body = await request.body()
        cdm_traj = trajectory_pb2.Trajectory()
        frame_defs: List[core_pb2.FrameDefinition] = []
        try:
            if content_type == "application/x-protobuf":
                cdm_traj.ParseFromString(body)
            else:
                doc = json.loads(body.decode("utf-8"))
                if isinstance(doc, dict) and "trajectory" in doc:
                    cdm_traj = json_format.ParseDict(doc["trajectory"], cdm_traj)
                    for fd_json in doc.get("frames") or []:
                        frame_defs.append(json_format.ParseDict(fd_json, core_pb2.FrameDefinition()))
                else:
                    cdm_traj = json_format.ParseDict(doc, cdm_traj)
        except (DecodeError, json_format.ParseError, UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
            raise HTTPException(
                400, f"malformed altavista.v1.Trajectory body (Content-Type "
                     f"{content_type or '<none>'!r}): {exc}")

        try:
            viewer_traj = cdm_adapter.cdm_trajectory_to_viewer_json(cdm_traj)
        except cdm_adapter.CdmAdapterError as exc:
            raise HTTPException(400, f"cannot convert Trajectory {cdm_traj.id!r} to viewer JSON: {exc}")
        if viewer_traj is None:
            # M20.1 (question 133): this endpoint publishes exactly one spacecraft, so a
            # state space with no position class (cdm_adapter.has_position_class) leaves
            # nothing to publish -- a typed 400, not a silently-empty spacecraft entry.
            raise HTTPException(
                400, f"Trajectory {cdm_traj.id!r} declares state space {cdm_traj.state_space_id!r}, "
                     f"which has no position class -- nothing to render")

        # M17.1: when frames were supplied alongside (the JSON envelope above), thread them
        # through the same FrameRegistry-backed conversion POST /api/cdm/run uses, and give
        # the entities frame its real origin/axes from the matching declared definition.
        # With no frames supplied (every pre-M17.1 caller, and the binary-protobuf path,
        # which cannot carry any), this is byte-for-byte the previous behaviour: the posted
        # frame_id carried through as a display name only, origin/axes at altavista's own
        # Earth/MJ2000Eq default (documented in altavista/CDM.md) -- purely additive.
        viewer_frames: List[dict] = []
        entities_frame = Frame(name=cdm_traj.frame_id or Frame().name)
        if frame_defs:
            try:
                viewer_frames = cdm_adapter.frames_to_viewer_json(frame_defs)
            except cdm_adapter.CdmAdapterError as exc:
                raise HTTPException(400, f"cannot build frame list from the supplied frames: {exc}")
            frame_def = next((fd for fd in frame_defs if fd.id == cdm_traj.frame_id), None)
            entities_frame = (cdm_adapter.viewer_frame_for(frame_def) if frame_def is not None else None) \
                or entities_frame

        scenario = ScenarioData(
            name=f"cdm:{cdm_traj.id or viewer_traj.name}",
            frame=entities_frame,
            spacecraft=[viewer_traj],
            frames=viewer_frames,
        ).to_dict()
        name = hub.put(scenario)
        if hub.clock and hub.clock.get("scenario") != name:
            hub.clock = None
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "scenario", "scenario": scenario})
        log.info("published CDM trajectory %r (entity %r, %d samples) to %d client(s)",
                 cdm_traj.id, cdm_traj.entity_id, len(cdm_traj.samples), len(hub.clients))
        return {"ok": True, "name": name, "clients": len(hub.clients)}

    @app.post("/api/cdm/run")
    async def publish_cdm_run(request: Request):
        """Accept an ``altavista.v1.RunProducts`` (M17.2, question 121 -- the CDM message the
        lead added for a whole run, replacing the ad hoc ``AVRUN1`` framing ``crates/av-run``
        used to hand-roll and this endpoint used to decode with the now-deleted
        ``altavista.cdm.parse_run_wire``): every ``Trajectory`` (keyed by ``SystemInstance.id``)
        and ``Event`` one ``av_kernel::drm::execute`` run produced, plus the run's own overall
        ``Provenance``, evaluated ``scores``, the dropped-in-flight-message count, and frame
        definitions.

        Accepts binary protobuf (``Content-Type: application/x-protobuf``) or JSON transcoding
        (any other content type, via ``google.protobuf.json_format``) -- *exactly* the same
        content-type dispatch ``POST /api/cdm/trajectory`` above already uses, per this task's
        own instruction ("/api/cdm/run must accept the message exactly the same way").

        Every ``Trajectory`` is converted with the *same*, unmodified
        ``altavista.cdm.cdm_trajectory_to_viewer_json`` ``POST /api/cdm/trajectory`` already
        uses (M16.3's own design steer, unchanged: reuse that path rather than writing a
        second converter) -- iterated off ``run_products.trajectories`` sorted by key, since a
        proto ``map`` field has no wire order of its own. Every ``Event`` goes through
        ``altavista.cdm.cdm_event_to_viewer_event`` (already sorted ``(tai_ns, id)`` by the
        executor, so no re-sort is needed here). The run's ``Provenance.config_hash`` (the
        DRM's own canonical hash, verified and refused-if-tampered by
        ``crates/av-kernel/src/drm/hash.rs`` before the run ever executed) is carried in
        ``ScenarioData.meta["configHash"]``, additive on the existing ``meta`` dict, so the
        viewer's info line (``web/js/cdm_run.js``'s ``formatScenarioInfo``, called from
        ``web/js/app.js``) can display it -- "reproducible from its config hash" (question 5)
        means a viewer showing a run must be able to show *which* configuration produced it.

        ``scores`` (new on the wire as of M17.2, question 121; threaded into the viewer
        payload as of M26.4b, question 165): ``RunProducts.scores`` (a proto map, keyed by
        score name, of ``ScoreResult{value, unit, passed}``) is converted with
        ``_score_result_to_dict`` below into ``ScenarioData.scores`` -- a plain dict, same
        key, each entry ``{"value", "unit", "passed"}`` -- and published additively on the
        scenario. ``meta["scoresSource"] = "RunProducts.scores"`` records where they came
        from, matching the ``bodiesSource`` convention below. Deliberately NOT
        ``google.protobuf.json_format.MessageToDict`` on each ``ScoreResult`` directly:
        that helper omits an unset ``optional bool passed`` field entirely rather than
        emitting an explicit JSON ``null``, which would silently misrepresent "no pass
        criterion exists" (a measure of effectiveness, ADR-005 sec 6) as "field not sent" --
        the Run Products panel must be able to tell the two apart on the wire, not only
        after a client-side coercion. The Run Products panel (``web/js/panels/
        run_products_panel.js``) is this data's first consumer.

        ``frames`` (M17.1, question 122): threaded into ``ScenarioData.frames`` through
        ``altavista.cdm.frames_to_viewer_json`` (which runs every entry through the same
        ``altavista.frames.FrameRegistry`` a Python scenario's own ``Scenario._build_frames``
        uses), so the viewer builds its frame graph -- and offers every frame the bundle
        actually declares, ICRF included whenever the producer declared one -- from the
        bundle alone, exactly as question 122 decided ("a consumer needs nothing outside
        the bundle to place a trajectory"). ``ScenarioData.frame`` itself (the *entities*
        frame -- ``web/js/scene.js``'s ``_originFrameId``) also gets its real ``origin``/
        ``axes`` from the matching declared ``FrameDefinition`` now (``cdm_adapter.
        viewer_frame_for``), not the previous hardcoded Earth/MJ2000Eq fallback every frame
        this endpoint could not resolve used to silently take.

        ``measurements`` (M25.3e, question 174, "Mirrors question 165"): ``RunProducts.
        measurements`` (question 173's ``repeated Measurement``, sorted by epoch and id
        by the executor -- no re-sort needed here, same posture as ``events`` above) is
        converted with ``_measurement_to_dict`` below into ``ScenarioData.measurements``
        -- a plain list, each entry ``{"id", "epoch", "sensorId", "frameId", "z", "r"}``
        -- published additively. ``meta["measurementsSource"] = "RunProducts.
        measurements"`` records where they came from, matching the ``scoresSource``/
        ``bodiesSource`` convention. Nothing is ever synthesized: a run with no
        measurements publishes ``"measurements": []``, and a measurement with an empty
        ``r`` (a sensor that declares no covariance) publishes an empty list, never a
        fabricated one.

        ``bodies`` (M18.2, question 125): the CDM ingest path had no bodies handling at
        all before this task, so the viewer's globe (which needs an ``Earth`` body
        entry -- ``web/js/scene.js``'s ``enableGlobe``) was unavailable for every
        ``av-run`` run. ``altavista.cdm.bodies_from_frames`` derives the scene's body
        list from the origin bodies ``RunProducts.frames`` actually names -- body
        identity comes from the wire, never invented, and a body no frame names is
        never added. ``ScenarioData.meta["bodiesSource"]`` records that these bodies
        came from the bundle's own frames, so a reader of the published scenario can
        tell them apart from a Python scenario file's ``Scenario.default_bodies()``
        convenience list.
        """
        content_type = (request.headers.get("content-type") or "").split(";", 1)[0].strip().lower()
        body = await request.body()
        run_products = run_pb2.RunProducts()
        try:
            if content_type == "application/x-protobuf":
                run_products.ParseFromString(body)
            else:
                run_products = json_format.Parse(body.decode("utf-8"), run_products)
        except (DecodeError, json_format.ParseError, UnicodeDecodeError, ValueError) as exc:
            raise HTTPException(
                400, f"malformed altavista.v1.RunProducts body (Content-Type "
                     f"{content_type or '<none>'!r}): {exc}")

        # F3c (this task's own defect fix, docs/feasibility-plan.md's F3 milestone): the
        # actual "RunProducts -> viewer scenario" conversion is factored out into
        # _run_products_to_scenario_data below -- decode + call + publish is all that is
        # left here -- so POST /api/cdm/sweep/sample further down (which also needs to turn
        # a decoded RunProducts, this time read off local disk rather than the request
        # body, into a scenario) uses the exact same conversion rather than a second,
        # potentially-divergent copy of it. See that function's own docstring for the full,
        # unchanged conversion contract (M17.2, M17.1, M26.4b, M25.3e, M18.2) this route
        # always implemented inline before this refactor.
        scenario_data = _run_products_to_scenario_data(run_products)
        scenario = scenario_data.to_dict()
        name = hub.put(scenario)
        if hub.clock and hub.clock.get("scenario") != name:
            hub.clock = None
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "scenario", "scenario": scenario})
        log.info("published RunProducts %r (%d trajectories, %d events, %d measurements, %d bodies, config_hash %s) to %d client(s)",
                 scenario_data.meta["runId"], len(run_products.trajectories), len(run_products.events),
                 len(scenario_data.measurements), len(scenario_data.bodies), run_products.provenance.config_hash,
                 len(hub.clients))
        return {"ok": True, "name": name, "clients": len(hub.clients)}

    @app.post("/api/cdm/sweep")
    async def publish_cdm_sweep(request: Request):
        """Accept an ``altavista.v1.SweepResults`` (F3, ``docs/feasibility-plan.md``,
        question 191 -- the feasibility-study products ``crates/av-sweep`` writes): every
        ``SweepSample`` (one grid point, one Monte Carlo draw) and ``ScoreAggregate`` (one
        score, one grid point, aggregated across its draws) one ``av-sweep`` study produced.

        Accepts binary protobuf (``Content-Type: application/x-protobuf``) or JSON
        transcoding (any other content type, via ``google.protobuf.json_format``) --
        *exactly* the same content-type dispatch ``POST /api/cdm/run`` above already uses
        (this task's own instruction: mirror that route's pattern).

        **The wire contract below is fixed by this task's own manager and is the single
        source of truth** (the viewer's feasibility panel, built concurrently by another
        worker, is written against exactly this shape) -- this handler does not improvise
        on it:

        ::

            scenario name:  "sweep:{sweep_id}"
            meta.sweepSource = "SweepResults"
            meta.sweepId, meta.sweepHash, meta.drmHash    (always present, even when empty)
            scenario["sweep"] = <the dict _sweep_results_to_dict below returns>

        ``sweep_id``/``sweep_hash``/``drm_hash`` are carried through verbatim (``""`` if the
        producer left them empty) -- unlike ``POST /api/cdm/run``'s ``run_id`` fallback
        chain (``run_products.run_id or run_products.provenance.run_id or "run"``), this
        contract names no fallback for an empty ``sweep_id``, so none is invented here: a
        study published with an empty ``sweep_id`` becomes a scenario literally named
        ``"sweep:"``, an honest (if unusual) reflection of what the producer actually sent,
        not a silently substituted default that would misrepresent that the producer left it
        blank.

        The whole ``scenario["sweep"]`` projection is built by the single, module-level,
        directly-testable :func:`_sweep_results_to_dict` function below (this task's own
        instruction: "so a test -- and worker B's join test later -- can call it directly
        rather than through HTTP") -- see that function's own docstring for the per-field
        contract, the ``scores``/``passFraction`` ``HasField`` handling (mirroring
        ``_score_result_to_dict`` above for exactly the same "an unset optional field must
        stay an explicit ``null``, never silently omitted or coerced" reason, question 165),
        the ``seeds`` uint64-as-decimal-string encoding (proto3 canonical JSON convention --
        a bare JSON number cannot hold the full uint64 range without precision loss), and
        every explicit sort (a proto ``map`` has no wire order of its own).

        **No trajectory, no events, no bodies.** A ``SweepResults`` carries none of the
        three (``proto/altavista/v1/run.proto``'s message has no ``Trajectory``/``Event``
        fields at all -- it is samples and aggregates, nothing else). Investigated against
        ``altavista.model.ScenarioData``: :meth:`ScenarioData.span` computes ``min(ts),
        max(ts)`` over every spacecraft's first/last sample epoch and returns ``None`` when
        that list is empty (``spacecraft=[]``) rather than raising or dividing by zero, and
        :meth:`ScenarioData.to_dict` already handles a ``None`` span by publishing
        ``t0``/``t1`` as JSON ``null`` -- so ``spacecraft=[]``, ``events=[]`` (the dataclass
        default) is a genuinely representable, self-consistent scenario, not a workaround
        that silently breaks something else. Chosen over any alternative that would invent a
        trajectory, a body, or a time span this message does not carry -- "nothing is ever
        synthesized" applies here exactly as it does to ``bodiesSource``/``scoresSource``
        above. ``frame``/``bodies`` are therefore left at :class:`ScenarioData`'s own
        defaults (the Earth/MJ2000Eq ``Frame()``, an empty body list) rather than derived
        from anything -- there is nothing in a ``SweepResults`` to derive them from.
        """
        content_type = (request.headers.get("content-type") or "").split(";", 1)[0].strip().lower()
        body = await request.body()
        sweep_results = run_pb2.SweepResults()
        try:
            if content_type == "application/x-protobuf":
                sweep_results.ParseFromString(body)
            else:
                sweep_results = json_format.Parse(body.decode("utf-8"), sweep_results)
        except (DecodeError, json_format.ParseError, UnicodeDecodeError, ValueError) as exc:
            raise HTTPException(
                400, f"malformed altavista.v1.SweepResults body (Content-Type "
                     f"{content_type or '<none>'!r}): {exc}")

        scenario_data = ScenarioData(
            name=f"sweep:{sweep_results.sweep_id}",
            # See this handler's own docstring, "No trajectory, no events, no bodies": a
            # SweepResults carries none of the three, so the minimal honest representation
            # is the dataclass's own empty defaults, never a fabricated one.
            spacecraft=[],
            events=[],
            meta={
                "sweepSource": "SweepResults",
                "sweepId": sweep_results.sweep_id,
                "sweepHash": sweep_results.sweep_hash,
                "drmHash": sweep_results.drm_hash,
            },
        )
        scenario = scenario_data.to_dict()
        scenario["sweep"] = _sweep_results_to_dict(sweep_results)

        name = hub.put(scenario)
        if hub.clock and hub.clock.get("scenario") != name:
            hub.clock = None
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "scenario", "scenario": scenario})
        log.info("published SweepResults %r (%d samples, %d aggregates, sweep_hash %s) to %d client(s)",
                 sweep_results.sweep_id, len(sweep_results.samples), len(sweep_results.aggregates),
                 sweep_results.sweep_hash, len(hub.clients))
        return {"ok": True, "name": name, "clients": len(hub.clients)}

    @app.post("/api/cdm/sweep/sample")
    async def open_cdm_sweep_sample(request: Request):
        """Open one already-run sample of a published feasibility study in the viewer --
        the server-side fix for F3's "opens any sample's run in the existing viewer through
        its products_uri" requirement (docs/feasibility-plan.md's F3 milestone, F3c: this
        task's own defect fix).

        **Why this route exists at all.** The panel worker's first attempt
        (``web/js/app.js``'s ``openFeasibilitySample()``) tried to do this from the
        *browser*: ``fetch(drawRow.productsUri)`` followed by POSTing the bytes to
        ``POST /api/cdm/run``. That cannot work, for two independent reasons, both verified
        against the source before this route was written: (1) ``SweepSample.products_uri``
        is the sample's *directory*, not a file -- ``crates/av-sweep/src/bin/av-sweep/
        study.rs``'s ``finalize()`` sets it to ``std::fs::canonicalize(&r.sample_dir)``; the
        actual ``RunProducts`` bytes are at ``<products_uri>/run_products.pb``. (2) it is an
        absolute *local filesystem path* on whichever machine ran the study -- a browser's
        own ``fetch()`` of that string resolves it against the page's own origin and asks
        *this* HTTP server for a path shaped like ``/private/var/folders/.../sample_p0_d0``,
        which serves nothing. Reading the sample therefore has to happen server-side, where
        the path is meaningful.

        **The caller sends an identity, never a path.** The request body is exactly
        ``{"sweepId": str, "pointIndex": int, "drawIndex": int}`` -- three plain
        identifiers, nothing resembling a filesystem path. This handler resolves that
        identity against the server's OWN state: it looks up the already-published sweep
        scenario for ``sweepId`` (the one ``POST /api/cdm/sweep`` put in the hub, named
        ``f"sweep:{sweepId}"``), finds that point/draw in ITS OWN recorded
        ``sweep["points"]``, and reads the ``productsUri`` THE SERVER ITSELF RECORDED when
        it published that study -- never a string the HTTP caller supplied (a caller that
        additionally sends a ``productsUri`` field in the body gets it silently ignored;
        see ``tests/test_feasibility_join.py``'s own
        ``test_caller_supplied_products_uri_is_never_trusted`` for the proof). A route that
        instead read a caller-supplied path directly would be an arbitrary-local-file-read
        primitive -- any path on the server's filesystem, readable by anyone who can reach
        this endpoint. This route cannot be that, because the only paths it will ever open
        are ones that were already inside a scenario THIS SAME SERVER published, at its own
        earlier ``POST /api/cdm/sweep`` call -- the same trust boundary
        ``GET /textures/{file}`` already draws for local file reads on this server, just
        keyed by a study's own identity instead of a filename. No path-sanitisation is
        layered on top of that on purpose: there is no untrusted path here to sanitise.
        Adding one would only decorate a boundary that is not "a string pattern this
        handler happens to accept," it is "identity resolved against this server's own
        prior state."

        **This route reads local files, so it is only meaningful for a viewer server
        co-located with (on the same machine or shared filesystem as) the study that
        produced the sweep it is opening a sample of.** A ``SweepResults`` bundle published
        here after being produced elsewhere -- or a study whose sample directories were
        since deleted -- legitimately fails the "does not exist on this host" refusal
        below. That is not a bug to route around; it is an honest report of "this server
        cannot see that file," the same posture ``GET /textures/{file}`` already takes
        (404, never a fabricated texture) for a texture directory this process cannot see.

        Refusals (each its own typed ``HTTPException``, each with its own test in
        ``tests/test_feasibility_join.py``): the request body is malformed (400); no sweep
        named ``sweepId`` has been published to this server (404); that sweep has no such
        ``pointIndex`` (404); that point has no such ``drawIndex`` (404); the sample at that
        point/draw failed (``error`` non-empty, so it has no run to open -- 409); the
        directory or ``run_products.pb`` inside it does not exist on this host (404); the
        file that does exist does not decode as an ``altavista.v1.RunProducts`` (400).

        On success: converts ``<productsUri>/run_products.pb`` with the exact same
        ``_run_products_to_scenario_data`` ``POST /api/cdm/run`` uses (one implementation
        of "a RunProducts becomes a viewer scenario", this task's own instruction),
        publishes and broadcasts it exactly like that route, and returns the published
        scenario's name so the caller can ``{"type": "select", "name": ...}`` it over the
        websocket -- the same selection path the scenario dropdown already uses.
        """
        try:
            body = await request.json()
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            raise HTTPException(400, f"malformed JSON body: {exc}")
        if not isinstance(body, dict):
            raise HTTPException(400, "expected a JSON object with sweepId/pointIndex/drawIndex")
        sweep_id = body.get("sweepId")
        point_index = body.get("pointIndex")
        draw_index = body.get("drawIndex")
        if not isinstance(sweep_id, str) or not sweep_id:
            raise HTTPException(400, "'sweepId' must be a non-empty string")
        # bool is a subclass of int in Python -- excluded explicitly so {"pointIndex": true}
        # is refused as malformed rather than silently treated as point 1.
        if not isinstance(point_index, int) or isinstance(point_index, bool):
            raise HTTPException(400, "'pointIndex' must be an integer")
        if not isinstance(draw_index, int) or isinstance(draw_index, bool):
            raise HTTPException(400, "'drawIndex' must be an integer")

        scenario_name = f"sweep:{sweep_id}"
        sweep_scenario = hub.scenarios.get(scenario_name)
        if sweep_scenario is None or (sweep_scenario.get("meta") or {}).get("sweepSource") != "SweepResults":
            raise HTTPException(404, f"no published sweep named {sweep_id!r} (looked for scenario "
                                      f"{scenario_name!r} in the hub -- publish it with POST /api/cdm/sweep first)")
        sweep = sweep_scenario.get("sweep") or {}

        point = next((p for p in sweep.get("points", []) if p.get("pointIndex") == point_index), None)
        if point is None:
            raise HTTPException(404, f"sweep {sweep_id!r} has no point {point_index}")
        sample = next((s for s in point.get("samples", []) if s.get("drawIndex") == draw_index), None)
        if sample is None:
            raise HTTPException(404, f"sweep {sweep_id!r} point {point_index} has no draw {draw_index}")

        # A failed sample never ran to completion, so there is nothing at a products_uri to
        # open (crates/av-sweep/src/aggregate.rs's own "a failed sample's scores map is
        # always empty" rule, and study.rs's finalize() never sets products_uri for one --
        # see feasibility_panel.js's own isSampleOpenable, which this refusal mirrors on the
        # server side). Checked BEFORE looking at productsUri (never after) so the error
        # text always names the real cause -- "this sample failed" -- rather than a
        # confusing "no productsUri" for a case that has a perfectly good explanation.
        if sample.get("error"):
            raise HTTPException(409, f"sweep {sweep_id!r} point {point_index} draw {draw_index} failed and has "
                                      f"no run to open: {sample['error']}")
        products_uri = sample.get("productsUri") or ""
        if not products_uri:
            raise HTTPException(409, f"sweep {sweep_id!r} point {point_index} draw {draw_index} recorded no "
                                      f"productsUri (every succeeded sample has one; this looks like a "
                                      f"malformed publish)")

        # SweepSample.products_uri is the sample's DIRECTORY (verified against
        # crates/av-sweep/src/bin/av-sweep/study.rs's finalize(), which canonicalize()s
        # r.sample_dir -- never a file), so the real RunProducts bytes are one path segment
        # further in. `products_uri` here came from THIS SERVER'S OWN hub state, never the
        # request body (see this route's own docstring, "identity, never a path").
        run_products_path = Path(products_uri) / "run_products.pb"
        if not run_products_path.is_file():
            raise HTTPException(
                404, f"{run_products_path} does not exist on this host -- this route reads local files, so "
                     f"it only works against a viewer server co-located with the study that produced "
                     f"{sweep_id!r} (see this route's own docstring); a sweep published here after being "
                     f"produced elsewhere legitimately fails this way")

        run_products = run_pb2.RunProducts()
        try:
            run_products.ParseFromString(run_products_path.read_bytes())
        except DecodeError as exc:
            raise HTTPException(400, f"{run_products_path} does not decode as an altavista.v1.RunProducts: {exc}")

        scenario_data = _run_products_to_scenario_data(run_products)
        scenario = scenario_data.to_dict()
        name = hub.put(scenario)
        if hub.clock and hub.clock.get("scenario") != name:
            hub.clock = None
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "scenario", "scenario": scenario})
        log.info("opened feasibility sample sweep=%r point=%d draw=%d -> run %r (%d client(s))",
                 sweep_id, point_index, draw_index, name, len(hub.clients))
        return {"ok": True, "name": name, "clients": len(hub.clients)}

    @app.delete("/api/scenario/{name}")
    async def delete_scenario(name: str):
        if not hub.remove(name):
            raise HTTPException(404, f"no scenario named {name!r}")
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "removed", "name": name})
        return {"ok": True}

    @app.post("/api/clock")
    async def set_clock(request: Request):
        clock = await request.json()
        hub.clock = _clock_fields(clock)
        if "scenario" not in hub.clock and hub.order:
            hub.clock["scenario"] = hub.order[-1]
        await hub.broadcast({"type": "clock", "source": "api", **hub.clock})
        return {"ok": True}

    @app.get("/api/health")
    async def health():
        return {"ok": True, "clients": len(hub.clients), "scenarios": hub.names(),
                "textures": textures.exists()}

    @app.get("/textures/{file}")
    async def texture(file: str):
        path = textures / Path(file).name
        if not path.exists():
            raise HTTPException(404, f"texture {file!r} not found in {textures}")
        return FileResponse(path)

    @app.websocket("/ws")
    async def ws_endpoint(ws: WebSocket):
        await ws.accept()
        hub.clients.add(ws)
        log.info("browser connected (%d total)", len(hub.clients))
        try:
            await hub.send_state(ws)
            while True:
                raw = await ws.receive_text()
                try:
                    msg = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                if msg.get("type") == "clock":
                    hub.clock = _clock_fields(msg)
                    await hub.broadcast({"type": "clock", "source": "peer", **hub.clock}, exclude=ws)
                elif msg.get("type") == "select":
                    name = msg.get("name")
                    if name in hub.scenarios:
                        await ws.send_text(json.dumps({"type": "scenario", "scenario": hub.scenarios[name]}))
        except WebSocketDisconnect:
            pass
        finally:
            hub.clients.discard(ws)
            log.info("browser disconnected (%d total)", len(hub.clients))

    app.mount("/", web_static, name="static")
    return app


def _score_result_to_dict(sr: run_pb2.ScoreResult) -> dict:
    """``altavista.v1.ScoreResult`` -> the wire dict question 165 approved: ``{"value",
    "unit", "passed"}``. ``passed`` is the explicit Python ``None`` (JSON ``null``) when
    the proto field is unset (``sr.HasField("passed")`` is ``False``, ADR-005 sec 6's
    measure of effectiveness -- no pass criterion exists), and the real ``bool`` (``True``
    or ``False``) when it is set (an Objective). Deliberately hand-built rather than
    ``google.protobuf.json_format.MessageToDict(sr)``: that helper's default settings omit
    an unset ``optional`` field from the dict entirely, which would put no ``"passed"`` key
    at all in the JSON for a measure of effectiveness -- indistinguishable, to a consumer
    checking for the key's presence, from a server that dropped the field -- rather than
    the explicit ``null`` question 165 calls for. ``unit`` is the enum's canonical proto3
    JSON name (``core_pb2.Unit.Name(...)``, e.g. ``"UNIT_RADIAN"``), the same convention
    ``ScenarioData.state_spaces``' protobuf-JSON-transcoded ``StateComponent.unit`` entries
    already use.
    """
    return {
        "value": sr.value,
        "unit": core_pb2.Unit.Name(sr.unit),
        "passed": sr.passed if sr.HasField("passed") else None,
    }


def _run_products_to_scenario_data(run_products: run_pb2.RunProducts) -> ScenarioData:
    """Convert a decoded ``altavista.v1.RunProducts`` into the viewer's
    :class:`~altavista.model.ScenarioData` -- the single, shared implementation of "a
    RunProducts becomes a viewer scenario" (F3c, this task's own instruction: extracted out
    of ``POST /api/cdm/run`` so ``POST /api/cdm/sweep/sample`` -- opening one sample of a
    published feasibility study, F3's requirement -- uses the exact same conversion rather
    than a second, potentially-divergent copy of it. ``tests/test_cdm_run.py``, unedited,
    is this refactor's own proof: every one of its assertions about ``POST /api/cdm/run``'s
    published scenario must still pass unchanged, since this function's body is byte-for-
    byte what that route used to run inline.

    Every effect is unchanged from before this extraction (see git history / the docstring
    this replaced on ``POST /api/cdm/run`` for the full per-milestone rationale, still
    accurate): ``instance_ids`` sorted for determinism (a proto map has no wire order of its
    own); trajectories with no position class (M20.1, question 133) filtered out via
    ``cdm_adapter.cdm_trajectory_to_viewer_json`` returning ``None``; events converted
    unsorted (the executor already sorts them); scores (M26.4b, question 165) and
    measurements (M25.3e, question 174) hand-built via ``_score_result_to_dict``/
    ``_measurement_to_dict`` so an unset optional stays an explicit JSON ``null``, never
    silently omitted; the entities frame (M17.1, question 122) derived from the first
    trajectory (in sorted-key order) that declares a ``frame_id``, given its real
    origin/axes from the matching declared ``FrameDefinition`` when the bundle names one;
    bodies (M18.2, question 125) derived strictly from the origin bodies
    ``RunProducts.frames`` actually names, via ``cdm_adapter.bodies_from_frames``, run
    *before* frame-registry validation (a narrower check, so an unknown-body frame is
    refused with this precise error before frame registration ever runs); every declared
    frame (M17.1) run through the real ``altavista.frames.FrameRegistry`` via
    ``cdm_adapter.frames_to_viewer_json``.

    Raises ``fastapi.HTTPException`` (400) with the exact typed messages
    ``POST /api/cdm/run`` always raised inline, now raised from this one place so every
    caller gets identical refusals for identical malformed input -- ``CdmAdapterError``/
    ``UnknownBodyError`` never leak past this function uncaught. Does NOT decode wire bytes,
    does NOT call ``hub.put``/broadcast, and does NOT call ``.to_dict()`` -- purely the
    ``RunProducts -> ScenarioData`` projection; each caller owns content-type dispatch (or,
    for ``POST /api/cdm/sweep/sample``, reading the bytes off local disk), publish/
    broadcast, and its own route-specific logging, exactly as it always did.
    """
    # RunProducts.trajectories is a proto map (no wire order of its own) -- iterate sorted
    # by key (SystemInstance.id) for determinism, matching this task's own "sort explicitly"
    # rule (crates/av-kernel's BTreeMap convention, mirrored here since this module has no
    # BTreeMap of its own).
    instance_ids = sorted(run_products.trajectories)
    try:
        # M20.1 (question 133, decided by the lead): "the viewer renders only instances
        # whose state space has [a position class]" -- cdm_trajectory_to_viewer_json
        # returns None for an instance with no position trajectory to render (e.g. a
        # native controller's own non-physical state space), filtered out here rather
        # than published as an empty/zero-filled spacecraft entry. Events are untouched:
        # viewer_events below is built straight from run_products.events, entity-tagged
        # independently of instance_ids/viewer_trajs, so a filtered-out instance's own
        # lifecycle/port_command events still reach the timeline.
        viewer_trajs = [v for k in instance_ids if (v := cdm_adapter.cdm_trajectory_to_viewer_json(run_products.trajectories[k])) is not None]
    except cdm_adapter.CdmAdapterError as exc:
        raise HTTPException(400, f"cannot convert RunProducts trajectory to viewer JSON: {exc}")
    viewer_events = [cdm_adapter.cdm_event_to_viewer_event(e) for e in run_products.events]
    # M26.4b (question 165): RunProducts.scores is a proto map (no wire order of its
    # own) -- sorted by name for the same determinism reason instance_ids is sorted
    # above. See _score_result_to_dict's own docstring for why this hand-builds the
    # three keys instead of delegating to json_format.MessageToDict.
    viewer_scores = {name: _score_result_to_dict(run_products.scores[name])
                     for name in sorted(run_products.scores)}
    # M25.3e (question 174): RunProducts.measurements is a repeated field the
    # executor already sorts by (epoch, id) (question 173) -- no re-sort here, same
    # posture as `viewer_events` above.
    viewer_measurements = [_measurement_to_dict(m) for m in run_products.measurements]

    # The entities frame (ScenarioData.frame, web/js/scene.js's _originFrameId): the
    # first trajectory (in sorted-key order, for determinism) declaring a frame_id wins
    # (every fixture this endpoint is exercised against today declares exactly one
    # instance, hence one frame). Its real origin/axes come from the matching declared
    # FrameDefinition (cdm_adapter.viewer_frame_for) when the bundle actually declares
    # one for that id and it is a body-axes kind altavista.model.Frame can represent;
    # otherwise this falls back to carrying frame_id through as a display name only
    # (altavista's own Earth/MJ2000Eq Frame default), the same documented caveat
    # POST /api/cdm/trajectory below still has for a bundle with no matching frame.
    # Computed from the raw RunProducts.frames (not the FrameRegistry-validated
    # scenario_data.frames set further below), so it is available before -- and
    # independent of -- frame-registry validation.
    frame_id = next((run_products.trajectories[k].frame_id for k in instance_ids if run_products.trajectories[k].frame_id), "")
    frame_def = next((fd for fd in run_products.frames if fd.id == frame_id), None) if frame_id else None
    entities_frame = (cdm_adapter.viewer_frame_for(frame_def) if frame_def is not None else None) \
        or Frame(name=frame_id or Frame().name)
    run_id = run_products.run_id or run_products.provenance.run_id or "run"
    scenario_data = ScenarioData(
        name=f"run:{run_id}",
        frame=entities_frame,
        spacecraft=viewer_trajs,
        events=viewer_events,
        scores=viewer_scores,
        measurements=viewer_measurements,
        meta={"configHash": run_products.provenance.config_hash, "runId": run_id,
              # M26.4b (question 165): additive, present even when RunProducts declared
              # no scores at all (viewer_scores == {}) -- same honest-provenance posture
              # as bodiesSource below ("a reader of meta must be able to tell where the
              # (possibly empty) value came from").
              "scoresSource": "RunProducts.scores",
              # M25.3e (question 174): same posture -- present even when
              # viewer_measurements == [] (a run that declared no measurements).
              "measurementsSource": "RunProducts.measurements"},
    )

    # M18.2 (question 125): the globe needs an `Earth` body entry, and the CDM
    # ingest path had no bodies handling at all -- derive the scene's body list
    # from the origin bodies RunProducts.frames actually names (never invented; see
    # altavista.cdm.bodies_from_frames's own docstring for the full contract), sampled
    # over this run's own epoch span with the existing altavista.bodies.BodySampler.
    # Raises (400, never silently drops a body or a 500) if a frame names a body
    # this process's GMAT solar system has no data for. Deliberately run *before*
    # the FrameRegistry validation below: bodies_from_frames only ever needs GMAT's
    # solar system (SolarSystem.GetBody), a much narrower check than a full
    # CoordinateSystem construction, so an unknown-body frame is refused with this
    # precise, typed error before frame registration ever has a chance to run.
    try:
        scenario_data.bodies = cdm_adapter.bodies_from_frames(
            run_products.frames, frame=entities_frame, span=scenario_data.span())
    except cdm_adapter.UnknownBodyError as exc:
        raise HTTPException(400, f"cannot derive bodies from RunProducts.frames: {exc}")
    # Additive, honest provenance note (this task's own requirement): a reader of
    # meta must be able to tell this scenario's bodies came from the run bundle's
    # own frames, not from a Python scenario file's Scenario.default_bodies()
    # convenience list (which this path never calls) -- present even when the
    # derived list is empty (RunProducts.frames named no body), since that is
    # still an honest fact about where the (empty) list came from.
    scenario_data.meta["bodiesSource"] = "RunProducts.frames"

    # M17.1 (question 122): every FrameDefinition RunProducts.frames declares, run
    # through the real altavista.frames.FrameRegistry (GMAT validation + question 76's
    # parent_frame_id fill) -- the same wire shape ScenarioData.to_dict()["frames"]
    # already carries for a Python scenario. Raises (400, never silently drops a frame
    # or substitutes a validated one) if GMAT rejects a declared definition.
    try:
        scenario_data.frames = cdm_adapter.frames_to_viewer_json(run_products.frames)
    except cdm_adapter.CdmAdapterError as exc:
        raise HTTPException(400, f"cannot build frame list from RunProducts.frames: {exc}")

    return scenario_data


def _sweep_results_to_dict(sr: run_pb2.SweepResults) -> dict:
    """``altavista.v1.SweepResults`` -> the ``scenario["sweep"]`` wire dict this task's
    manager fixed (see ``POST /api/cdm/sweep``'s own docstring above for the full contract
    text). Module-level and independently callable (not nested in the route handler) so a
    test -- or another consumer -- can exercise this projection directly, without an HTTP
    round trip.

    ``points``: grouped by ``SweepSample.point_index`` (a distinct point exists here iff at
    least one sample names it -- ``SweepResults`` carries no separate "the grid has N points"
    field to iterate against, only the samples themselves), sorted ascending by point index;
    each point's own ``samples`` sorted ascending by ``draw_index``. ``axisValues`` for a
    point is taken from its first sample (in draw-index order) -- every draw at one grid
    point shares the same axis values by construction (``crates/av-sweep``'s own
    ``expand_grid``/``sample_config`` apply one point's axis values identically to every
    draw at that point), so this is not a lossy choice among disagreeing candidates; a key
    absent from that sample's own ``axis_values`` map is simply absent from the result --
    never zero-filled (a proto3 ``double`` map has no way to distinguish "0.0" from
    "unset" for a key that IS present, but a key that is simply not in the map is
    unambiguous, and this function never invents one).

    ``scores``: reuses ``_score_result_to_dict`` verbatim (this task's own instruction: "do
    not write a second one") -- the exact same reasoning that function's own docstring gives
    for ``ScoreResult.passed`` applies unchanged to ``ScoreAggregate.pass_fraction`` below:
    ``google.protobuf.json_format.MessageToDict``'s default settings omit an unset
    ``optional`` field entirely rather than emitting an explicit JSON ``null``, so
    ``pass_fraction`` is read through ``HasField`` here, not through a bare ``a.pass_fraction
    or None`` (which would wrongly turn a real, measured ``0.0`` pass fraction into
    ``null``).

    ``seeds``: ``SweepSample.seeds`` is ``map<string, uint64>``; Python's protobuf runtime
    exposes each value as a plain ``int``, so it is stringified here (``str(v)``) to match
    proto3 canonical JSON's own uint64-as-decimal-string convention (a bare JSON number
    cannot represent the full uint64 range without precision loss in every mainstream JSON
    parser, JavaScript's included) -- the same convention
    ``tests/test_sweep_results_json.py`` independently verifies for the Rust-side JSON
    encoder's own ``seeds`` output.

    Every map-derived list is sorted explicitly (``axisKeys``, ``scoreNames``, each sample's
    own ``scores``/``seeds`` keys, ``aggregates`` by ``(name, pointIndex)``) -- a proto
    ``map`` has no wire order of its own, matching this module's existing convention
    (``POST /api/cdm/run``'s own ``instance_ids = sorted(...)`` above).

    Nothing is ever synthesized: a study with no aggregates returns ``"aggregates": []``; a
    failed sample's ``scores``/``seeds`` are published exactly as recorded (empty, per
    ``crates/av-sweep``'s own "a failed sample never got far enough to derive any seeds"
    rule) rather than backfilled with a placeholder.
    """
    axis_keys = sorted({k for s in sr.samples for k in s.axis_values.keys()})
    score_names = sorted({a.name for a in sr.aggregates})

    samples_by_point: Dict[int, List] = {}
    for s in sr.samples:
        samples_by_point.setdefault(s.point_index, []).append(s)

    points = []
    for point_index in sorted(samples_by_point):
        samples = sorted(samples_by_point[point_index], key=lambda s: s.draw_index)
        points.append({
            "pointIndex": point_index,
            # See this function's own docstring: every draw at one point shares the same
            # axis values by construction, so the first (in draw-index order) is authoritative.
            "axisValues": dict(samples[0].axis_values),
            "samples": [
                {
                    "drawIndex": s.draw_index,
                    "runId": s.run_id,
                    "configHash": s.config_hash,
                    "seeds": {k: str(s.seeds[k]) for k in sorted(s.seeds)},
                    "scores": {name: _score_result_to_dict(s.scores[name]) for name in sorted(s.scores)},
                    "productsUri": s.products_uri,
                    "error": s.error,
                }
                for s in samples
            ],
        })

    aggregates = [
        {
            "name": a.name,
            "pointIndex": a.point_index,
            "draws": a.draws,
            "mean": a.mean,
            "stdDev": a.std_dev,
            "min": a.min,
            "max": a.max,
            # Question 165's same rule, restated for ScoreAggregate: HasField, never `or
            # None` (a real, measured pass_fraction of 0.0 must stay 0.0, not become null).
            "passFraction": a.pass_fraction if a.HasField("pass_fraction") else None,
        }
        for a in sorted(sr.aggregates, key=lambda a: (a.name, a.point_index))
    ]

    return {
        "sweepId": sr.sweep_id,
        "sweepHash": sr.sweep_hash,
        "drmHash": sr.drm_hash,
        "axisKeys": axis_keys,
        "scoreNames": score_names,
        "points": points,
        "aggregates": aggregates,
    }


def _measurement_to_dict(m: core_pb2.Measurement) -> dict:
    """``altavista.v1.Measurement`` -> the wire dict question 174 approved: ``{"id",
    "epoch", "sensorId", "frameId", "z", "r"}``. Mirrors ``_score_result_to_dict``'s own
    hand-built-dict convention (question 174's own text: "Mirrors question 165").

    ``epoch`` is ``m.epoch_ns`` converted through the same ``tai_ns_to_a1mjd`` every
    other epoch on this endpoint's own payload uses (trajectory samples, events) -- so a
    measurement lines up on the viewer's timeline exactly like everything else, never a
    second, raw ``tai_ns`` unit silently mixed onto the same payload. ``z``/``r`` are
    plain lists, copied verbatim off the proto's own repeated ``double`` fields: an
    empty ``r`` (e.g. a star tracker's unit-quaternion measurement, which declares no
    covariance -- ``crates/av-kernel/src/codec.rs``'s own ``measurements_from_field_
    values`` doc comment) is published as an empty list, never a fabricated identity or
    zero matrix (question 174's own "nothing is ever synthesized" rule).
    """
    return {
        "id": m.measurement_id,
        "epoch": cdm_adapter.tai_ns_to_a1mjd(m.epoch_ns),
        "sensorId": m.sensor_id,
        "frameId": m.frame_id,
        "z": list(m.z),
        "r": list(m.r),
    }


def _clock_fields(msg: dict) -> dict:
    """Keep only the playback fields of a clock message (t, playing, speed, scenario)."""
    return {k: msg[k] for k in ("t", "playing", "speed", "scenario") if k in msg}


def _default_texture_dir() -> Path:
    try:
        from .gmat_env import texture_dir
        return texture_dir()
    except FileNotFoundError:
        return Path("/nonexistent")


def serve(host: str = "0.0.0.0", port: int = DEFAULT_PORT, texture_dir: Optional[os.PathLike] = None,
          log_level: str = "info", profile: str = profile_loader.DEFAULT_PROFILE_ID) -> None:
    """Run the viewer server (blocking). ``profile`` -- see ``create_app``'s docstring
    (M19.5, question 132)."""
    import uvicorn

    logging.basicConfig(level=getattr(logging, log_level.upper(), logging.INFO),
                        format="%(asctime)s %(name)s: %(message)s")
    app = create_app(texture_dir=texture_dir, profile=profile)
    log.info("altavista viewer at http://%s:%d/  (textures: %s, profile: %s)",
             host, port, _default_texture_dir(), profile)
    uvicorn.run(app, host=host, port=port, log_level=log_level)
