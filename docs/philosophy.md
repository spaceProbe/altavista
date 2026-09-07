# Alta Vista: what we are building, and why it is one thing

Alta Vista is infrastructure for taking a mission from a concept on a whiteboard to a
spacecraft under operations without ever changing the model of the world underneath it. The
same declared systems, the same validated dynamics, the same clock and the same data model
carry a design reference mission through feasibility, detailed design, software in the loop,
hardware in the loop, operations and replay. Nothing is re-described at a phase boundary,
because the boundaries are configuration, not rewrites.

## One data model, declared and hashed

Everything that can affect a result is a declared artifact in one common data model: a
system definition says what a thing is (its state space, ports, parameters, packet codecs and
dynamics); a system-of-systems configuration says which instances exist and how they connect;
a design reference mission says what happens to them, what is measured, and what counts as
success. Each is hashed, and a run is reproducible from its hashes. A trajectory, an event, a
measurement, a command, a score: each is its own type on the wire, never a label on something
else. Nothing exists only inside one program's memory. That is what lets a viewer, a flight
computer, a ground station, a replay and an analyst all read the same run and agree on it.

## One dynamics, from concept to console

GMAT's validated models are the physics, reached through a contract (derivatives, step, state
transition, frame conversion) that other models honour too: attitude and wheels, sensors with
declared noise, ground stations with contact geometry, controllers, links. A design study, a
Monte Carlo sweep, a flight-software test and an operations rehearsal propagate the same
equations against the same goldens. When we are deliberately more complete than the reference,
the exception is recorded and asserted, not assumed.

## One kernel, many bindings

A system instance may be a model in process, a container running real flight software, a
binary on an emulated processor, or a board on a bench. The kernel does not care: it steps
every instance on one integer clock, routes typed messages between declared ports with the
link's latency, injects declared faults and maneuvers, and records what happened. Lockstep
makes the run deterministic and replayable, faster or slower than real time; real-time pacing
makes the same run drive hardware. A binding the kernel cannot honour is refused at load,
never at step ten thousand.

## The flow

1. **Concept.** Author systems and a mission in plain files. Hash them.
2. **Feasibility.** Sweep parameters and seeds over the same executor; score against declared
   objectives and measures; keep every run's products.
3. **Design.** Refine the same definitions; see them on the globe, in ICRF, in a relative
   frame; covariance and events travel with the trajectory.
4. **Software in the loop.** Bind the flight software in a container or on an emulated
   processor; the sensors it reads and the actuators it commands are the design's own models;
   port traffic is byte-identical between runs.
5. **Hardware in the loop.** Bind the board; everything but the board replays from logged
   input and output.
6. **Operations.** The ground segment is a system with the same ports. Commands walk one state
   machine from proposal through authorization to acknowledgement; telemetry comes back as the
   same measurements the design used.
7. **Replay and learning.** Any run rebuilds from its products. Models, human or machine,
   propose against read-only data and their proposals are commands in the same machine.

## How we work

The artifact is the evidence: a test that passes, a golden that matches, a byte on the wire.
Exit codes, prose, comments and attributions are claims about the work, not the work. Every
tolerance is justified against a measurement; every root cause is named before a fix lands;
every decision is numbered and kept. Profiles select components and never change behaviour, so
two people running the same mission get the same answer. Refusals are typed and early. What we
could not prove is written down as plainly as what we could.
