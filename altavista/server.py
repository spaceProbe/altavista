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
            meta={"configHash": run_products.provenance.config_hash, "runId": run_id,
                  # M26.4b (question 165): additive, present even when RunProducts declared
                  # no scores at all (viewer_scores == {}) -- same honest-provenance posture
                  # as bodiesSource below ("a reader of meta must be able to tell where the
                  # (possibly empty) value came from").
                  "scoresSource": "RunProducts.scores"},
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

        scenario = scenario_data.to_dict()
        name = hub.put(scenario)
        if hub.clock and hub.clock.get("scenario") != name:
            hub.clock = None
        await hub.broadcast({"type": "list", "names": hub.names()})
        await hub.broadcast({"type": "scenario", "scenario": scenario})
        log.info("published RunProducts %r (%d trajectories, %d events, %d bodies, config_hash %s) to %d client(s)",
                 run_id, len(run_products.trajectories), len(run_products.events), len(scenario_data.bodies),
                 run_products.provenance.config_hash, len(hub.clients))
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
