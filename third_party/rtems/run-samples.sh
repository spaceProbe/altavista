#!/bin/sh
# Run RTEMS's own `hello` and `ticker` samples (built for zynqmp_rpu_lock_step by
# build-bsp.sh) on Renode's mainline Zynq UltraScale+ Cortex-R5 platform, headless, capturing
# UART0 output to third_party/rtems/run-renode/{hello,ticker}_uart.log (docs/sil-plan.md M24;
# docs/open-questions.md question 144). No network use here -- Renode and the built ELFs are
# already on disk.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
outdir="$here/rtems-build"
rundir="$here/run-renode"
renode="$here/../renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
bsp_dir="$outdir/arm/zynqmp_rpu_lock_step/testsuites/samples"

if [ ! -x "$renode" ]; then
    echo "run-samples.sh: $renode not found -- see third_party/renode/fetch-renode.sh" >&2
    exit 1
fi

for sample in hello ticker; do
    exe="$bsp_dir/$sample/$sample.exe"
    if [ ! -f "$exe" ]; then
        echo "run-samples.sh: $exe not found -- run build-bsp.sh first" >&2
        exit 1
    fi
    cp "$exe" "$rundir/$sample.exe"
done

for sample in hello ticker; do
    echo "== running $sample on Renode (headless) =="
    rm -f "$rundir/${sample}_uart.log" "$rundir/${sample}_uart.log.1"
    "$renode" --disable-gui --hide-log "$rundir/$sample.resc" > "$rundir/${sample}_renode_console.log" 2>&1
    echo "== $sample: Renode process exited; captured UART log: $rundir/${sample}_uart.log =="
done

echo "done -- inspect $rundir/hello_uart.log and $rundir/ticker_uart.log"
