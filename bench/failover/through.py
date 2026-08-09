"""Constant load through one address, bucketed per second, across a machine dying.

Threads rather than async because the interesting quantity is wall-clock seconds
with errors in them, and a blocking client with a short timeout measures that
directly. A request that hangs past the timeout counts as a failure — from a
caller's side an 8-second wait IS an outage, whatever it eventually returns.
"""
import json, sys, time, threading, urllib.request, collections

sp, secs = sys.argv[1], int(sys.argv[2])
start = time.time()
ok = collections.Counter()
bad = collections.Counter()
nodes = collections.Counter()
lock = threading.Lock()


def worker(w):
    i = 0
    while time.time() - start < secs:
        i += 1
        t = int(time.time() - start)
        req = urllib.request.Request(
            "http://127.0.0.1:8090/api/ratelimit",
            data=json.dumps({"key": f"w{w}-{i}"}).encode(),
            headers={"content-type": "application/json", "Host": "shop.eve.test"})
        try:
            with urllib.request.urlopen(req, timeout=3) as r:
                node = r.headers.get("x-comp-node", "?")
                with lock:
                    ok[t] += 1
                    nodes[node] += 1
        except Exception:
            with lock:
                bad[t] += 1


ts = [threading.Thread(target=worker, args=(w,), daemon=True) for w in range(8)]
for t in ts:
    t.start()
for t in ts:
    t.join()

events = {}
try:
    for line in open(f"{sp}/events.txt"):
        at, what = line.split(None, 1)
        events[int(float(at) - start)] = what.strip()
except FileNotFoundError:
    pass

print("    sec   ok   fail")
for t in range(secs):
    mark = f"   <- {events[t]}" if t in events else ""
    # Only seconds that are interesting: any failure, any event, or the edges.
    if bad[t] or mark or t < 2 or t == secs - 1:
        print(f"    {t:4} {ok[t]:5} {bad[t]:6}{mark}")
total_ok, total_bad = sum(ok.values()), sum(bad.values())
print(f"\n    {total_ok} ok, {total_bad} failed "
      f"({100 * total_bad / max(1, total_ok + total_bad):.2f}%)")
ev = sorted(events.items())
for i, (t, what) in enumerate(ev):
    # Bounded by the NEXT event, or the first report blames one kill for the other's
    # errors — it did, and read as a 59s recovery that never happened.
    end = ev[i + 1][0] if i + 1 < len(ev) else secs
    after = [s for s in range(t, end) if bad[s]]
    # Seconds-with-errors after the kill, and when they stop: the recovery window.
    if after:
        print(f"    after {what!r}: errors in seconds {after[0]}..{after[-1]} "
              f"({after[-1] - t + 1}s from kill to last error)")
    else:
        print(f"    after {what!r}: no failed request at all")
print(f"    served by: {dict(nodes)}")
