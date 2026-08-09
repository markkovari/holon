"""Median of each phase, from the node's own log lines."""
import re, sys, statistics

# Two shapes now: a start that compiled, and one that loaded a cached .cwasm.
# Averaging them together would hide the whole point of the cache.
compiled, cached = [], []
for line in open(sys.argv[1]):
    m = re.search(r"in (\d+) us \(fetch (\d+) us, (compile|cache-load) (\d+) us, link (\d+) us\)", line)
    if m:
        total, fetch, kind, build, link = m.groups()
        row = (int(total), int(fetch), int(build), int(link))
        (cached if kind == "cache-load" else compiled).append(row)
if not compiled and not cached:
    sys.exit("  no start lines with timings")


def table(rows, label):
    if not rows:
        return
    print(f"=== {label}: {len(rows)} start(s) ===")
    for i, name in enumerate(["total", "fetch", "build", "link"]):
        vals = [r[i] for r in rows]
        print(f"  {name:8} median {statistics.median(vals) / 1000:8.2f} ms   "
              f"min {min(vals) / 1000:7.2f} ms   max {max(vals) / 1000:7.2f} ms")


table(compiled, "cold: nothing cached, wasmtime compiles")
table(cached, "warm: compiled artifact loaded from cache")
if compiled and cached:
    a = statistics.median([r[0] for r in compiled])
    b = statistics.median([r[0] for r in cached])
    print(f"\n  {a / 1000:.1f} ms -> {b / 1000:.2f} ms, a {a / max(1, b):.0f}x cut "
          f"({100 * (a - b) / max(1, a):.1f}% of the start removed).")
