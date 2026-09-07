#!/bin/sh
# M24.2b -- run RTEMS's own `hello` and `ticker` samples, built for zynqmp_rpu_lock_step with
# the container-built toolchain, on Renode's mainline Zynq UltraScale+ Cortex-R5 platform,
# headless, capturing UART0 output to
# third_party/rtems-container/run-renode/{hello,ticker}_uart.log (docs/sil-plan.md M24;
# docs/open-questions.md questions 144, 148, 154, 157). No network use here -- Renode and the
# built ELFs are already on disk.
#
# Adapted from the read-only host reference third_party/rtems/run-samples.sh, with TWO fixes
# found in this task (both recorded in REPORT.md, not silently patched over):
#   1. Sample ELF source path: this BSP build's samples land FLAT at
#      .../testsuites/samples/<name>.exe, not .../testsuites/samples/<name>/<name>.exe as
#      that script assumed.
#   2. Invocation method: the host script's `"$renode" --disable-gui --hide-log
#      "$rundir/$sample.resc"` (script as a CLI positional argument) was tried here first and
#      hung indefinitely on hello.resc (5m46s wall, 5.48s CPU, zero output, no UART file --
#      killed, not a false success). Switched to the M24.1 spike's proven method: launch
#      Renode with `-P <port>` and drive it over the Monitor's TCP protocol via
#      run_via_monitor.py (`include @script.resc`), exactly as
#      third_party/renode/spike/measure_virtual_time.py already does successfully.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
outdir="$here/output/rtems-build"
rundir="$here/run-renode"
renode="$here/../renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
bsp_dir="$outdir/arm/zynqmp_rpu_lock_step/testsuites/samples"
py="${PYTHON:-python3}"

if [ ! -x "$renode" ]; then
    echo "run-samples.sh: $renode not found -- see third_party/renode/fetch-renode.sh" >&2
    exit 1
fi

for sample in hello ticker; do
    exe="$bsp_dir/$sample.exe"
    if [ ! -f "$exe" ]; then
        echo "run-samples.sh: $exe not found -- run build-bsp-fix.sh first" >&2
        exit 1
    fi
    cp "$exe" "$rundir/$sample.exe"
done

# Per-sample run-timeout: measured directly (see REPORT.md "this full-SoC platform runs far
# slower than real-time") at ~170s of real wall time per virtual second of `RunFor` for this
# full Zynq UltraScale+ platform description booting a real RTEMS/BSP image -- nowhere near
# the M24.1 spike's ~1:1 ratio for a trivial bare-loop firmware. `hello`'s RunFor was reduced
# to 0.3s (see hello.resc) and measured at 51.65s wall; 300s leaves ample margin. `ticker`
# cannot be shrunk (its own exit condition needs the RTEMS clock to reach 35s of virtual
# time) and is estimated at 70-100 minutes of real wall time at the measured rate; timeout set
# well above that estimate rather than tuned tight, since a false-negative kill here would
# discard a real, in-progress capture.
port=3355
for sample in hello ticker; do
    case "$sample" in
        hello) run_timeout=300 ;;
        ticker) run_timeout=7200 ;;
        *) run_timeout=300 ;;
    esac
    echo "== running $sample on Renode (headless, via monitor port $port, run-timeout ${run_timeout}s) =="
    rm -f "$rundir/${sample}_uart.log" "$rundir/${sample}_uart.log.1"
    "$py" "$here/run-renode/run_via_monitor.py" \
        --renode "$renode" \
        --resc "$rundir/$sample.resc" \
        --port "$port" \
        --renode-log "$rundir/${sample}_renode_console.log" \
        --startup-timeout 30 \
        --run-timeout "$run_timeout"
    status=$?
    if [ "$status" != "0" ]; then
        echo "run-samples.sh: $sample FAILED (run_via_monitor.py exit $status) -- see $rundir/${sample}_renode_console.log" >&2
        exit "$status"
    fi
    if [ ! -f "$rundir/${sample}_uart.log" ]; then
        echo "run-samples.sh: $sample reported success but $rundir/${sample}_uart.log is missing" >&2
        exit 1
    fi
    echo "== $sample: Renode process exited; captured UART log: $rundir/${sample}_uart.log =="
    port=$((port + 1))
done

echo "done -- inspect $rundir/hello_uart.log and $rundir/ticker_uart.log"
