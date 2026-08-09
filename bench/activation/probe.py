"""What a caller sees when they are the first to touch a scaled-to-zero app."""
import json, subprocess, sys, time, urllib.request

sp = sys.argv[1]
URL = "http://127.0.0.1:8092/api/ratelimit"
BODY = {"key": "a", "capacity": 10**8, "refill": 10**8}


def replicas():
    out = subprocess.run(["nats", "--server", "127.0.0.1:4232", "kv", "ls", "comp-inventory"],
                         capture_output=True, text=True).stdout.split()
    total = 0
    for k in out:
        raw = subprocess.run(["nats", "--server", "127.0.0.1:4232", "kv", "get",
                              "comp-inventory", k, "--raw"], capture_output=True, text=True).stdout
        try:
            total += sum(i.get("count", 0) for i in json.loads(raw).get("instances", []))
        except Exception:
            pass
    return total


def hit():
    req = urllib.request.Request(URL, data=json.dumps(BODY).encode(),
                                 headers={"content-type": "application/json",
                                          "Host": "shop.eve.test"})
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, (time.time() - t) * 1000
    except Exception as e:
        return getattr(e, "code", 0), (time.time() - t) * 1000


def wait_for(pred, secs):
    """Inventory is a heartbeat behind reality, so a count read the instant a request
    returns says nothing. Poll for the state instead of asserting on a snapshot —
    the first version of this test failed for exactly that reason."""
    t = time.time()
    while time.time() - t < secs:
        n = replicas()
        if pred(n):
            return n, time.time() - t
        time.sleep(1)
    return replicas(), None


before = replicas()
print(f"    replicas while idle:              {before}")
code, ms = hit()
print(f"    first request (cold):             HTTP {code} in {ms:.0f} ms")
code2, ms2 = hit()
print(f"    second request (now warm):        HTTP {code2} in {ms2:.0f} ms")
up, up_after = wait_for(lambda n: n >= 1, 15)
print(f"    replicas once the heartbeat lands: {up}"
      + (f" (seen after {up_after:.0f}s)" if up_after is not None else " — never appeared"))
down, down_after = wait_for(lambda n: n == 0, 40)
print(f"    replicas after it goes idle again: {down}"
      + (f" (back to zero after {down_after:.0f}s)" if down_after is not None else " — stayed up"))

log = open(f"{sp}/ingress.log").read() + open(f"{sp}/rec.log").read()
woke = "activated" in log
print(f"    the request itself woke it:       {'yes' if woke else 'NO'}")
ok = before == 0 and code == 200 and up >= 1 and woke
print("    PASS — parked at zero, woken by a request, parked again"
      if ok and down == 0 else
      "    PASS — parked at zero, woken by a request (did not re-park in time)"
      if ok else
      f"    FAIL — idle={before} code={code} up={up} woke={woke}")
