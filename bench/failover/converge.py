"""Time from each kill until the fleet is back at 5 replicas."""
import sys

sp = sys.argv[1]
samples = []
for line in open(f"{sp}/replicas.txt"):
    parts = line.split()
    if len(parts) == 2 and parts[1].isdigit():
        samples.append((int(parts[0]), int(parts[1])))
events = []
try:
    for line in open(f"{sp}/events.txt"):
        at, what = line.split(None, 1)
        events.append((int(float(at)), what.strip()))
except FileNotFoundError:
    pass
if not samples:
    sys.exit("    no samples")
t0 = samples[0][0]
ev = list(events)
for i, (at, what) in enumerate(ev):
    end = ev[i + 1][0] if i + 1 < len(ev) else 10**12
    after = [(t, n) for t, n in samples if at <= t < end]
    # The total does NOT drop the moment a node is killed — its inventory entry
    # lingers until the TTL expires, which is the whole point of ADR-0022's "a
    # missed heartbeat is not death". So: find the DIP first, then the recovery.
    # Latching onto the pre-dip value instead reported a 1s recovery that was
    # really the reading taken before anything had been noticed.
    dip = next(((t, n) for t, n in after if n < 5), None)
    if dip is None:
        print(f"    {what}: never observed below 5 (sampled every ~1s)")
        continue
    back = next((t for t, n in after if t > dip[0] and n >= 5), None)
    low = min(n for _, n in after)
    print(f"    {what}: noticed {dip[0] - at}s after the kill, "
          f"low water {low}/5, "
          + (f"back to 5 {back - at}s after the kill ({back - dip[0]}s to re-place)"
             if back else "not restored before the run ended"))
print("    trace: " + " ".join(f"{t - t0}s:{n}" for t, n in samples if n is not None))
