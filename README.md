# Alta Vista — a Three.js viewer for GMAT

Build mission scenarios in Python with the GMAT API, publish them to a small server,
and watch them in any number of browsers at once. The viewer replaces GMAT's built-in
OrbitView / OpenFrames windows for API-driven work.

```
Python session ──(GMAT API)──▶ Scenario ──POST /api/scenario──▶ altavista server ──WebSocket──▶ browser(s)
```

* **GMAT stays in your Python process.** The server only brokers JSON, so the GMAT
  singleton is never shared across processes and the server can run anywhere.
* **Any GMAT script runs headless.** Targeting, optimisation and finite burns are
  executed by GMAT's own mission sequence; the converged ephemeris is captured through
  an injected `ReportFile` (`SolverIterations = Current`, so only the final solver pass
  is kept) and GUI-only subscribers are stripped automatically.
* **Exact frames.** Trajectories, planet positions and planet orientations are
  converted with GMAT's `CoordinateConverter`, so the picture matches the numbers.
* **Shared playback.** Browsers can keep their clocks in sync (play/pause, scrub,
  speed) so a group can look at the same instant.

## Layout

```
altavista/            Python package
  gmat_env.py       find the GMAT install, load gmatpy, build the API startup file
  scenario.py       Scenario / Spacecraft builder (step propagation, script runs)
  bodies.py         planet positions + orientations in the scenario frame
  frames.py         CDM frame registry: validates FrameDefinitions against GMAT and
                    converts states between them (see altavista/FRAMES.md)
  pb/               committed Python protobuf bindings for proto/altavista/v1
                    (altavista/pb/generate.py regenerates them)
  model.py          data classes and the JSON schema sent to the browser
  server.py         FastAPI server: static viewer, REST publish, WebSocket fan-out
  client.py         publish() / set_clock() helpers (stdlib only)
  timeutil.py       A1MJD <-> UTC via GMAT's TimeSystemConverter
web/                Three.js viewer (no build step; Three.js is vendored)
examples/           runnable examples
tests/              pytest (no GMAT needed, except tests/test_frames.py)
GMAT R2026a/        the GMAT install this was built against
```

See [`altavista/FRAMES.md`](altavista/FRAMES.md) for the frame service: the `AxesKind` ->
GMAT realization table, the ENU/NED convention note, and measured tolerances.

## Setup

Requirements: a GMAT R2020a+ install with the Python API (`bin/gmatpy`), and a Python
version GMAT ships bindings for (R2026a: 3.9 – 3.14).

```bash
python3.13 -m venv .venv
.venv/bin/pip install -e ".[dev]"
```

The `dev` extra includes `grpcio` because the test suite covers the design-time gRPC
services; the runtime package itself needs only `protobuf`, and a deployment that wants the
gRPC service without the test tools installs the `grpc` extra instead.

The GMAT folder is found automatically when it sits next to this repository
(`AltaVista/GMAT R2026a`); otherwise set `GMAT_ROOT=/path/to/GMAT`. The first import
writes `bin/api_startup_file.txt` inside the GMAT folder (absolute paths), exactly as
GMAT's own `api/BuildApiStartupFile.py` does.

## Building the Rust workspace

The kernel, the GMAT shim and the services are a Cargo workspace. Two third-party trees
are fetched, not committed (`third_party/README.md`):

```bash
sh third_party/fetch-gmat-src.sh
sh third_party/fetch-cspice.sh
```

Then, with the GMAT install beside the repository (or `GMAT_ROOT` set):

```bash
cargo build --workspace
cargo test --workspace --exclude av-kernel
cargo test -p av-kernel
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

`cargo deny` needs the `cargo-deny` tool installed. The kernel suite runs GMAT in-process
and takes several minutes; run it alone rather than beside other heavy jobs. Tests that need
Docker or Renode skip with a visible reason when those are absent. The flight-software
container (`services/cfs/`) and the RTEMS toolchain (`third_party/rtems-container/`) have
their own recorded build recipes, each fetching over the network exactly once at image
build time.

## Run

Terminal 1 — the viewer server (binds all interfaces so other machines can connect):

```bash
.venv/bin/python -m altavista serve --port 8765
```

Open `http://<host>:8765/` in as many browsers as you like.

Terminal 2 — publish a scenario:

```bash
.venv/bin/python examples/01_leo_propagate.py
```

or run any GMAT script directly:

```bash
.venv/bin/python -m altavista run "GMAT R2026a/samples/Ex_HohmannTransfer.script" --frame EarthMJ2000Eq --name Hohmann
```

Scripts run with GMAT's `bin/` folder as the working directory, so relative paths such as
`../samples/SupportFiles/...` resolve exactly as they do in the GMAT GUI.

In the Claude Code / VS Code preview, `.claude/launch.json` defines an `altavista` server
configuration that starts the same server on port 8765.

## Python API

### Step propagation (interactive)

```python
import altavista as gv

sc = gv.Scenario("LEO demo", frame="EarthMJ2000Eq")
sat = sc.spacecraft("Sat", epoch="01 Jan 2026 00:00:00.000",
                    keplerian=dict(SMA=6778, ECC=0.0005, INC=51.6, RAAN=120, AOP=30, TA=0),
                    DryMass=420000, DragArea=1500, Cd=2.2)

fm = sc.force_model("Earth", degree=8, order=8, point_masses=["Luna", "Sun"], drag="JacchiaRoberts")
prop = sc.propagator(fm, integrator="PrinceDormand78", max_step=120)

sc.propagate(sat, hours=3, step=60, propagator=prop)      # samples every 60 s
sc.maneuver(sat, dv=[0.020, 0, 0], frame="VNB", name="Reboost")
sc.propagate(sat, hours=2, step=60, propagator=prop)      # continues from the burned state
sc.publish()                                              # -> every open browser
```

`sat.obj` is the underlying GMAT `Spacecraft`; use the raw API on it whenever you need
something the wrapper does not expose (`sat.obj.SetField(...)`, `sat.keplerian()`).
Several spacecraft sharing an epoch can be propagated together: `sc.propagate([a, b], ...)`.

### GMAT scripts (solvers, finite burns, anything GMAT can do)

```python
sc = gv.Scenario.from_script("mission.script", frame="EarthMJ2000Eq")
sc = gv.Scenario.from_script_text(script_string, name="Lunar transfer", bodies=["Earth", "Luna", "Sun"])
sc.publish()
```

Maneuver epochs are read from GMAT's command summaries and shown as events. Use
`max_points=N` to decimate long runs; the browser interpolates with the recorded
velocities (cubic Hermite), so sparse samples still draw smooth orbits.

Options common to both: `frame` (`EarthMJ2000Eq`, `SunMJ2000Ec`, `MarsInertial`,
`LunaFixed`, any `Create CoordinateSystem` in the script, or an `(origin, axes)` tuple),
`bodies` (which planets to draw; default: frame origin, Sun, central bodies, Moon for Earth),
`url` (viewer server, default `http://127.0.0.1:8765`, or env `ALTAVISTA_URL`).

`sc.save("scenario.json")` writes the same JSON the browser receives;
`gv.set_clock(t, playing=True, speed=600)` drives every browser's clock from Python.

## Feasibility studies

A study sweeps a DRM over declared axes (parameter values or event values, explicit lists or
`min..max` in steps) with dispersed draws per point, runs each sample in its own process
through the kernel, and aggregates the scores per point. Declare it from Python
(`altavista.feasibility.SweepDeclaration`, `SweepAxis.parameter_axis`, `SweepAxis.event_axis`),
or write the YAML by hand; either way the sweep is hashed like the DRM. `run_study(...)`
launches the `av-sweep` binary (`--sweep --drm --sos --system ... --out-dir --workers N`,
`--store-dir` for the file-backed study store) and returns the `SweepResults` it wrote.
Each sample records its seeds and config hash, so any sample re-runs alone, byte for byte,
with `av-sweep --run-sample`. `POST /api/cdm/sweep` publishes the results; a sweep scenario
opens in the study layout, with the feasibility panel showing the grid coloured by a score
and each point's distribution across draws. The worked example is
`docs/studies/drag-sail-vs-burn.md`:

```bash
.venv/bin/python altavista/feasibility/study_drag_sail_vs_burn.py --grid both --out-dir /path/to/out
```

## Viewer

* Left panel: pick a published scenario, toggle spacecraft/bodies, jump to events,
  choose a focus object (camera follows it), trajectory mode (full / past only / hidden),
  frame axes, XY-plane grid, labels, stars, clock sync.
* Bottom bar: play/pause (space), jump to start (Home/End), speed, scrubber with event ticks,
  UTC epoch.
* Mouse: drag to orbit, wheel to zoom, right-drag to pan.

Bodies are textured with the maps shipped in GMAT's `data/graphics/texture` and are
oriented with GMAT's body-fixed frames (Earth rotation, lunar libration, IAU poles).

## Tests

```bash
.venv/bin/python -m pytest
```

The unit tests cover script preparation and the JSON model and do not need GMAT.
The examples double as integration tests against the real GMAT install.

## Notes and limitations

* API-built mission sequences (`gmat.Command(...)` + `gmat.Execute()`) do not deliver data to
  `ReportFile` subscribers in R2026a, and `gmat.SaveScript()` crashes, so solver-based
  missions must go through script text (see `examples/03_lunar_transfer.py` for the
  Python-template pattern). Step propagation and impulsive burns work with the object API.
* One GMAT engine per process: `from_script` replaces the engine's configuration. Build
  object-API scenarios in a fresh session or before running scripts.
* Planet orientation is sampled every ≤ 0.25 day and spun about the pole in between; for
  multi-year runs increase `build(body_samples=...)` if you need exact lunar libration.
