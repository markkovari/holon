"""Stop and start one instance N times, measuring what a caller sees.

The host reports its own phase timings; what this adds is the two numbers a caller
actually experiences: how long the start command takes to ack, and whether the very
next request is served — the ack claims "will serve", and an untested claim is a
comment.
"""
import json, os, subprocess, sys, time, urllib.request

sp, n = sys.argv[1], int(sys.argv[2])
LEDGER = f"{sp}/n1/instances.json"
SUBJ = "comp.cold.cmd.n1"


def nats_req(verb, payload):
    t = time.time()
    r = subprocess.run(["nats", "--server", "127.0.0.1:4232", "req", f"{SUBJ}.{verb}",
                        json.dumps(payload), "--timeout=60s"],
                       capture_output=True, text=True)
    return time.time() - t, r.stdout + r.stderr


def probe():
    """Time to a served request. Retries, because 'refused' right after a stop is
    the expected state, not a failure."""
    t = time.time()
    while time.time() - t < 30:
        req = urllib.request.Request(
            "http://127.0.0.1:3801/api/ratelimit",
            data=json.dumps({"key": "probe", "capacity": 10**8, "refill": 10**8}).encode(),
            headers={"content-type": "application/json", "Host": "shop.eve.test"})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                if r.status == 200:
                    return time.time() - t
        except Exception:
            time.sleep(0.002)
    return None


ledger = json.load(open(LEDGER))
key = sorted(ledger)[0]
start_cmd = ledger[key]
stop_cmd = {k: start_cmd[k] for k in ("tenant", "app", "component")}
print(f"  instance {key}, digest {start_cmd['digest'][:19]}...")

warm, cold = [], []
for i in range(n):
    # Every other iteration deletes BOTH caches — the pulled .wasm and the compiled
    # .cwasm — so the run contains real cold starts and not just re-pulls that still
    # hit the compile cache.
    evict = i % 2 == 1
    nats_req("stop", stop_cmd)
    if evict:
        for d in (f"{sp}/n1/artifacts", f"{sp}/n1/cache"):
            if os.path.isdir(d):
                for f in os.listdir(d):
                    os.remove(os.path.join(d, f))
    ack, _ = nats_req("start", start_cmd)
    served = probe()
    (cold if evict else warm).append((ack, served))
    tag = "cold (both caches cleared)" if evict else "warm                      "
    print(f"    {i + 1:2}. {tag}  ack {1000 * ack:7.0f} ms   first request served after "
          + (f"{1000 * served:6.0f} ms" if served is not None else "NEVER"))


def stat(rows, label):
    if not rows:
        return
    acks = sorted(r[0] for r in rows)
    served = sorted(r[1] for r in rows if r[1] is not None)
    print(f"  {label:14} ack median {1000 * acks[len(acks) // 2]:6.0f} ms"
          + (f"   first-request median {1000 * served[len(served) // 2]:6.0f} ms"
             if served else "   never served"))


print()
stat(warm, "cache warm")
stat(cold, "cache evicted")
print("  (ack includes the `nats` CLI's own startup, ~100ms — the host's own phase\n"
      "   timings below are the honest cost)")
