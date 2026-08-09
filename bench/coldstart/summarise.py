"""Median of each phase, from the node's own log lines."""
import re, sys, statistics

rows = []
for line in open(sys.argv[1]):
    m = re.search(r"in (\d+) ms \(fetch (\d+) ms, compile (\d+) ms, link (\d+) ms\)", line)
    if m:
        rows.append(tuple(int(x) for x in m.groups()))
if not rows:
    sys.exit("  no start lines with timings")
names = ["total", "fetch", "compile", "link"]
print("=== host-side phase cost, median of %d starts ===" % len(rows))
for i, name in enumerate(names):
    vals = [r[i] for r in rows]
    print(f"  {name:8} median {statistics.median(vals):6.0f} ms   "
          f"min {min(vals):5} ms   max {max(vals):5} ms")
med_total = statistics.median([r[0] for r in rows])
med_compile = statistics.median([r[2] for r in rows])
print(f"\n  compile is {100 * med_compile / max(1, med_total):.0f}% of a cold start.")
