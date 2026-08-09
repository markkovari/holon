"""Replica count before, during and after a load that is certain to be shed."""
import json, subprocess, sys, time

sp = sys.argv[1]


def replicas():
    keys = subprocess.run(["nats", "--server", "127.0.0.1:4232", "kv", "ls", "comp-inventory"],
                          capture_output=True, text=True).stdout.split()
    total = 0
    for k in keys:
        raw = subprocess.run(["nats", "--server", "127.0.0.1:4232", "kv", "get",
                              "comp-inventory", k, "--raw"], capture_output=True, text=True).stdout
        try:
            total += sum(i.get("count", 0) for i in json.loads(raw).get("instances", []))
        except Exception:
            pass
    return total


before = replicas()
print(f"    replicas before load:  {before}")

load = subprocess.Popen(
    ["oha", "-z", "45s", "-c", "120", "--no-tui", "--output-format", "json", "-m", "POST",
     "-d", '{"key":"s","capacity":100000000,"refill":100000000}',
     "-H", "content-type:application/json", "-H", "Host:shop.eve.test",
     "http://127.0.0.1:8093/api/ratelimit"],
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)

peak, trace = before, []
t = time.time()
while time.time() - t < 45:
    n = replicas()
    peak = max(peak, n)
    trace.append(n)
    time.sleep(3)
out, _ = load.communicate(timeout=60)

codes = {}
try:
    codes = json.loads(out).get("statusCodeDistribution", {})
except Exception:
    pass
shed = sum(v for k, v in codes.items() if k.startswith("5"))
served = sum(v for k, v in codes.items() if k.startswith("2"))
print(f"    served {served}, shed {shed}")
print(f"    replicas during load:  peak {peak}  (trace {trace})")
after, t = before, time.time()
while time.time() - t < 40:
    after = replicas()
    if after <= 1:
        break
    time.sleep(3)
print(f"    replicas once idle:    {after}")
ok = shed > 0 and peak > before
print("    PASS — shedding grew the app" if ok else
      f"    FAIL — shed={shed} before={before} peak={peak}")
