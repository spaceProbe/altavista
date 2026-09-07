#!/usr/bin/env python3
"""M24.4d, load-sensitivity test: does the ~2.5s include-reply-to-listener-appears gap (measured
in isolation by listener_race_probe.py, six times, all within 2.40-2.56s) grow under real system
contention -- specifically, several Renode processes booting the same platform+ELF at once, each
with its OWN, non-shared, per-iteration resc path (so this isolates *contention* from the
shared-file race already characterised separately in concurrency_probe.py)? If the gap stays
small under N-way contention, the field failure (port 50201, 15s budget exhausted) needs a
different or additional explanation than "this host was just busy." If it grows close to or past
15s, that alone explains an occasional real-world timeout on a loaded host.
"""
import sys
import threading
import time
import json

sys.path.insert(0, "/Users/probe/code/AltaVista/third_party/renode/M24_4d")
import listener_race_probe as p  # noqa: E402


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 4
    results = []
    lock = threading.Lock()
    def worker(idx):
        local_results = []
        p.run_one_iteration(100 + idx, local_results)
        with lock:
            results.extend(local_results)

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(n)]
    t_start = time.monotonic()
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=180)
    print(f"all {n} iterations finished in {time.monotonic()-t_start:.2f}s wall time")
    for r in sorted(results, key=lambda r: r.get("idx", -1)):
        print(json.dumps({
            "idx": r.get("idx"),
            "include_reply_first_pass_elapsed_s": r.get("include_reply_first_pass_elapsed_s"),
            "tcp_connect_succeeded_within_15s": r.get("tcp_connect_succeeded_within_15s"),
            "tcp_connect_elapsed_from_include_sent_s": r.get("tcp_connect_elapsed_from_include_sent_s"),
            "lsof_first_listen_offset_from_poll_start_s": r.get("lsof_first_listen_offset_from_poll_start_s"),
            "exception": r.get("exception"),
        }))
    with open(f"{p.SCRATCH}/contention_probe_results.json", "w") as f:
        json.dump(results, f, indent=2, default=str)


if __name__ == "__main__":
    main()
