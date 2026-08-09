"""Who is holding replicas right now, straight off the inventory bucket."""
import json, subprocess, sys

sp, pi, key = sys.argv[1], sys.argv[2], sys.argv[3]


def nats(*args):
    return subprocess.run(["nats", "--server", "127.0.0.1:4232", *args],
                          capture_output=True, text=True, timeout=20).stdout


total = 0
rows = []
for node in sorted(k for k in nats("kv", "ls", "comp-inventory").split() if k):
    raw = nats("kv", "get", "comp-inventory", node, "--raw")
    try:
        inv = json.loads(raw)
    except Exception:
        continue
    n = sum(i.get("count", 0) for i in inv.get("instances", []))
    total += n
    rows.append((node, inv.get("labels", {}).get("box", "?"), n))
for node, box, n in rows:
    print(f"    {node:8} ({box:3}) {n} replica(s) {'#' * n}")
# The desired count is 5. Printing the sum next to it is the whole assertion:
# "recovered" means the fleet is back at 5, not that some node is alive.
print(f"    {'total':8}       {total} / 5 desired"
      f"{'' if total == 5 else '   <- NOT converged'}")
