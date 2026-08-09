"""Fill the compiled-artifact cache with garbage and check the node still starts."""
import json, os, subprocess, sys, time, urllib.request

sp = sys.argv[1]
cache = f"{sp}/n1/cache"
ledger = json.load(open(f"{sp}/n1/instances.json"))
key = sorted(ledger)[0]
start_cmd = ledger[key]
stop_cmd = {k: start_cmd[k] for k in ("tenant", "app", "component")}


def req(verb, payload):
    subprocess.run(["nats", "--server", "127.0.0.1:4232", "req",
                    f"comp.cold.cmd.n1.{verb}", json.dumps(payload), "--timeout=60s"],
                   capture_output=True, text=True)


req("stop", stop_cmd)
files = [f for f in os.listdir(cache)] if os.path.isdir(cache) else []
if not files:
    sys.exit("    no cached artifact to corrupt — did the cache write?")
for f in files:
    with open(os.path.join(cache, f), "wb") as fh:
        fh.write(b"this is not machine code, it is a sentence")
print(f"    corrupted {len(files)} cached artifact(s)")

req("start", start_cmd)
served = None
t = time.time()
while time.time() - t < 30:
    r = urllib.request.Request(
        "http://127.0.0.1:3801/api/ratelimit",
        data=json.dumps({"key": "c", "capacity": 10**8, "refill": 10**8}).encode(),
        headers={"content-type": "application/json", "Host": "shop.eve.test"})
    try:
        with urllib.request.urlopen(r, timeout=5) as resp:
            if resp.status == 200:
                served = time.time() - t
                break
    except Exception:
        time.sleep(0.05)

log = open(f"{sp}/n1.log").read()
noticed = "ignoring unusable" in log
rewritten = any(os.path.getsize(os.path.join(cache, f)) > 1000 for f in os.listdir(cache)) \
    if os.path.isdir(cache) and os.listdir(cache) else False
print(f"    served again after corruption: {'yes' if served is not None else 'NO'}")
print(f"    logged that it dropped the bad cache: {'yes' if noticed else 'NO'}")
print(f"    cache rewritten with a good artifact: {'yes' if rewritten else 'NO'}")
print("    PASS" if (served is not None and noticed and rewritten) else "    FAIL")
