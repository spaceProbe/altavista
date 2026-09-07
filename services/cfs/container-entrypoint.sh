#!/bin/sh
# M23.4 (docs/sil-plan.md M23, docs/open-questions.md question 153): this image's own
# ENTRYPOINT. Runs the two processes question 153's own decision requires in the same
# container -- "a small Rust lockstep shim process ... runs beside the flight software in the
# same container" -- and orders their startup so cFS's `io_lockstep` app (which makes exactly
# ONE connection attempt to the shim's Unix socket, no retry -- see
# services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c's own `connect_to_shim` call site)
# never races the shim's own socket creation.
#
# `av-lockstep-shim` (services/cfs/bin/av-lockstep-shim, a stripped Linux/aarch64 binary --
# see services/cfs/Dockerfile's own top comment for exactly how it is built and why it is
# prebuilt rather than compiled inside this Dockerfile) listens for the kernel's
# `altavista.v1.LockstepService` gRPC on `0.0.0.0:50070` -- the fixed port this image's own
# `container.control_port` convention uses (drms/demo_attitude_control_controller_cfs.system.
# yaml declares the same 50070) -- and for cFS's own connection on the Unix socket at
# `/var/run/lockstep/lockstep-local.sock` (`io_lockstep_app.c`'s own `IO_LOCKSTEP_SOCKET_PATH`
# default, unchanged).
set -eu

SOCKET_DIR=/var/run/lockstep
SOCKET_PATH="$SOCKET_DIR/lockstep-local.sock"
mkdir -p "$SOCKET_DIR"
rm -f "$SOCKET_PATH"

/cfs/av-lockstep-shim --socket-path "$SOCKET_PATH" --grpc-addr 0.0.0.0:50070 &
SHIM_PID=$!

# `io_lockstep`'s own connect_to_shim() makes exactly one attempt (question 153's README: "the
# shim listens ... the flight-software peer connects") -- wait for the socket file to exist
# (the same `wait_until(... || socket_path.exists())` pattern
# crates/av-lockstep-shim/tests/end_to_end_kernel_path.rs already uses for this exact shim
# binary) before starting cFS, rather than a bare fixed sleep. A bounded 10 s deadline (100 x
# 100 ms) -- if the shim never creates its socket, this is a typed failure (nonzero exit,
# visible in `docker logs`), never a silent hang.
i=0
while [ ! -S "$SOCKET_PATH" ]; do
    if ! kill -0 "$SHIM_PID" 2>/dev/null; then
        echo "container-entrypoint: av-lockstep-shim (pid $SHIM_PID) exited before creating $SOCKET_PATH" >&2
        wait "$SHIM_PID" || true
        exit 1
    fi
    i=$((i + 1))
    if [ "$i" -gt 100 ]; then
        echo "container-entrypoint: av-lockstep-shim never created $SOCKET_PATH within 10s" >&2
        kill "$SHIM_PID" 2>/dev/null || true
        exit 1
    fi
    sleep 0.1
done

# M23.4 FIX: `libpsp_lockstep.so` (services/cfs/apps/io_lockstep/CMakeLists.txt's own top
# comment has the full account -- it must be a real SHARED library, not statically linked twice
# into io_lockstep.so and sch_lockstep.so, so both share its mutable tick-queue state) is
# installed to this same "cf/" directory alongside the app modules that dlopen it. Setting
# LD_LIBRARY_PATH here is the belt-and-suspenders fallback (alongside that CMakeLists.txt's own
# install-path choice) so the dynamic linker finds it regardless of this platform's own RPATH
# handling for a plain, non-add_cfe_app SHARED target.
export LD_LIBRARY_PATH="/cfs/cpu1/cf${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# `exec` replaces this shell with core-cpu1 (PID 1 inside the container becomes cFS's own
# process) -- `docker stop`/`docker rm` on this container tears down the whole cgroup/namespace
# (the backgrounded shim included) regardless, matching `ManagedContainer::stop_and_remove`'s
# own "stop then rm" contract; no separate reaping of $SHIM_PID is needed here.
cd /cfs/cpu1
exec ./core-cpu1
