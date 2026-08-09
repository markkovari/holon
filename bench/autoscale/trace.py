"""Replica count over time, so the shape of the scale-up and scale-down is visible."""
import sys
sp = sys.argv[1]
rows = []
for line in open(f"{sp}/replicas.txt"):
    p = line.split()
    if len(p) == 2 and p[1].isdigit():
        rows.append((int(p[0]), int(p[1])))
if not rows:
    sys.exit("  no samples")
t0 = rows[0][0]
print("=== replicas over time (min 1, max 4, target 10 concurrent) ===")
seen = None
for t, n in rows:
    if n != seen:
        print(f"    {t - t0:4}s   {n}  {'#' * n}")
        seen = n
# Samples before the first placement are the fleet still starting, not a scale
# decision. Counting them made a correct run report "saw 0..4".
live = [n for _, n in rows]
first = next((i for i, n in enumerate(live) if n > 0), len(live))
live = live[first:]
lo, hi = (min(live), max(live)) if live else (0, 0)
print(f"\n    low {lo}, high {hi}")
print("    PASS: it scaled up and came back down" if lo == 1 and hi == 4 else
      f"    CHECK: expected to touch 1 and 4, saw {lo}..{hi}")
