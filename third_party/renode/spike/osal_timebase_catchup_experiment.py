#!/usr/bin/env python3
"""Standalone, independent re-implementation of the exact catch-up loop in
third_party/cfs/osal/src/os/shared/src/osapi-timebase.c's OS_TimeBase_CallbackThread
(the pinned OSAL commit d2d877a69cff47452bcca274b309147d48e6c16f, function body read
directly from that file and transcribed line-for-line below), built to empirically
answer M23.2's open unknown (docs/sil-plan.md M24, M24.1 task):

    "Can sch_lockstep's 1us OS_TimerSet interval fire the timer callback more than once
    per tick when a single large elapsed-time jump is delivered?"

This script does NOT modify or link against third_party/cfs; it transcribes the one
relevant loop (osapi-timebase.c lines ~465-501 in the pinned commit) with a counter in
place of the real callback, so the answer is measured, not just asserted from reading.
No OSAL/kernel/flight-software code is touched by this task (M24.1 is a spike, no
kernel code) -- this is a measurement script only, kept under third_party/renode/spike
alongside the rest of the M24.1 spike's scripts.

OS_TimerSet(timer_id, start_time, interval_time) (osapi-time.c:342-343) sets
timecb->wait_time = start_time and timecb->interval_time = interval_time on creation --
that is where the initial wait_time values below come from.
"""


class TimeCb:
    __slots__ = ("wait_time", "interval_time", "backlog_resets", "callback_fires")

    def __init__(self, wait_time, interval_time):
        self.wait_time = wait_time
        self.interval_time = interval_time
        self.backlog_resets = 0
        self.callback_fires = 0


def one_tick(timecb: TimeCb, tick_time: int):
    """Verbatim transcription of osapi-timebase.c's single-callback catch-up loop:

        saved_wait_time    = timecb->wait_time;
        timecb->wait_time -= tick_time;
        while (timecb->wait_time <= 0)
        {
            timecb->wait_time += timecb->interval_time;
            if (timecb->wait_time < -timecb->interval_time)
            {
                ++timecb->backlog_resets;
                timecb->wait_time = -timecb->interval_time;
            }
            if (saved_wait_time > 0 && timecb->callback_ptr != NULL)
            {
                (*timecb->callback_ptr)(...);
            }
            if (timecb->interval_time <= 0)
            {
                break;
            }
        }
    """
    saved_wait_time = timecb.wait_time
    timecb.wait_time -= tick_time
    while timecb.wait_time <= 0:
        timecb.wait_time += timecb.interval_time
        if timecb.wait_time < -timecb.interval_time:
            timecb.backlog_resets += 1
            timecb.wait_time = -timecb.interval_time
        if saved_wait_time > 0:
            timecb.callback_fires += 1
        if timecb.interval_time <= 0:
            break


def report(label, timecb, tick_time):
    print(
        f"{label}: interval={timecb.interval_time}us tick={tick_time}us "
        f"-> fires_this_call={timecb.callback_fires} backlog_resets={timecb.backlog_resets} "
        f"final_wait_time={timecb.wait_time}"
    )


def main():
    # Case A: exactly the scenario in sch_lockstep_app.c's top comment --
    # SCH_LOCKSTEP_MIN_INTERVAL_US = 1 (OS_TimerSet(id, 1, 1) -> wait_time=1,
    # interval_time=1), a single 100 ms kernel Step delivered as tick_time=100000 (us).
    t = TimeCb(wait_time=1, interval_time=1)
    one_tick(t, 100000)
    report("Case A (sch_lockstep_app.c's exact scenario)", t, 100000)

    # Case B: a 1 second jump instead of 100 ms -- does the fire count scale with jump size?
    t = TimeCb(wait_time=1, interval_time=1)
    one_tick(t, 1_000_000)
    report("Case B (1s jump, same 1us interval)", t, 1_000_000)

    # Case C: nominal, no-jump case (tick_time == interval_time): must fire exactly once.
    t = TimeCb(wait_time=1, interval_time=1)
    one_tick(t, 1)
    report("Case C (nominal, no jump)", t, 1)

    # Case D: a jump of exactly 2x interval_time.
    t = TimeCb(wait_time=1, interval_time=1)
    one_tick(t, 2)
    report("Case D (2x interval jump)", t, 2)

    # Case E: interval_time matched to the step period (100ms) -- the fixed design's
    # closest OS_TimerAdd-style analogue if someone set interval_time=100000 instead of 1.
    t = TimeCb(wait_time=100000, interval_time=100000)
    one_tick(t, 100000)
    report("Case E (interval matched to step period)", t, 100000)

    # Case F: repeat Case A's 100ms jump five times in a row from persistent timecb state,
    # to see whether the fire count grows across repeated large jumps or stays bounded.
    t = TimeCb(wait_time=1, interval_time=1)
    for i in range(5):
        before = t.callback_fires
        one_tick(t, 100000)
        print(
            f"Case F step {i}: fires_this_call={t.callback_fires - before} "
            f"cumulative={t.callback_fires} backlog_resets={t.backlog_resets} "
            f"final_wait_time={t.wait_time}"
        )

    # Case G: sweep tick_time from 2us up to 10,000,000us (10s) at interval=1us and confirm
    # the fire count is bounded (does not grow without bound) -- the specific claim to check
    # against sch_lockstep_app.c's own comment ("fired... 100,000 times").
    print("Case G sweep (interval=1us):")
    for tick in [2, 10, 100, 1000, 10_000, 100_000, 1_000_000, 10_000_000]:
        t = TimeCb(wait_time=1, interval_time=1)
        one_tick(t, tick)
        print(f"  tick_time={tick:>9} -> fires={t.callback_fires} backlog_resets={t.backlog_resets}")


if __name__ == "__main__":
    main()
