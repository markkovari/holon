#!/usr/bin/env bash
# Run all three gate patterns as REAL Golem agents and prove EXACT serialization:
# fire concurrent bursts at one key and the counts are exact (rate limit admits
# exactly capacity, throttle exactly burst+1, batch accounts for every submit) —
# because a Golem worker is a single-threaded durable actor per key (no CAS).
# Contrast: the shared-store gate-domain over-admits / re-buckets under the load.
#
# Reuses the Golem 1.5 binary vendored for the golem-workflow provider e2e.
set -euo pipefail
cd "$(dirname "$0")/golem"
G="../../../providers/golem-workflow/.bin/golem"

if [ ! -x "$G" ]; then
  echo "Golem binary not found — run \`just golem-e2e\` once to fetch it." >&2
  exit 1
fi

# start the local Golem server if the gateway (:9006) isn't already up.
if ! python3 -c "import urllib.request as u; u.urlopen('http://127.0.0.1:9006/', timeout=2)" 2>/dev/null; then
  echo "starting golem server..."; "$G" server run --clean >/tmp/golem-server.log 2>&1 &
  for _ in $(seq 1 60); do lsof -nP -iTCP:9006 -sTCP:LISTEN >/dev/null 2>&1 && break; sleep 2; done
fi

echo "building + deploying the gate agent..."
"$G" deploy -Y 2>&1 | tail -3

echo "=== exact serialization on Golem (a single-writer durable worker per key) ==="
python3 - <<'PY'
import urllib.request as u, threading, json, time
H = {"Host": "gate.localhost:9006"}
def call(path, method):
    for a in range(5):
        try: return u.urlopen(u.Request("http://127.0.0.1:9006" + path, method=method, headers=H), timeout=15).read().decode()
        except Exception:
            if a == 4: raise
            time.sleep(2)
def pj(r): return json.loads(json.loads(r))
ok = True

# 1) RATE LIMIT — token bucket, capacity 10. 24 concurrent takes -> exactly 10.
print("rate limit  (token bucket, capacity 10): 24 concurrent takes")
for t in range(3):
    k = f"rl{t}"; call(f"/gate/{k}/reset", "POST")
    res = [None] * 24
    def hit(i, k=k): res[i] = pj(call(f"/gate/{k}/take", "POST"))["allowed"]
    ts = [threading.Thread(target=hit, args=(i,)) for i in range(24)]; [x.start() for x in ts]; [x.join() for x in ts]
    a = sum(1 for x in res if x); ok = ok and a == 10
    print(f"  trial {t}: {a}/24 admitted -> {'EXACT (10)' if a == 10 else 'got ' + str(a)}")

# 2) THROTTLE — GCRA 5/s, burst 2. 12 concurrent -> exactly burst+1 = 3.
print("throttle    (GCRA 5/s, burst 2): 12 concurrent")
for t in range(3):
    k = f"th{t}"; call(f"/gate/{k}/reset", "POST")
    res = [None] * 12
    def hit(i, k=k): res[i] = pj(call(f"/gate/{k}/throttle", "POST"))["allowed"]
    ts = [threading.Thread(target=hit, args=(i,)) for i in range(12)]; [x.start() for x in ts]; [x.join() for x in ts]
    a = sum(1 for x in res if x); ok = ok and a == 3
    print(f"  trial {t}: {a}/12 admitted -> {'EXACT (3)' if a == 3 else 'got ' + str(a)}")

# 3) BATCH — coalesce, max 4. 10 concurrent submits -> exactly 10 accounted, no loss/dup.
print("batch       (coalesce, max 4): 10 concurrent submits")
for t in range(3):
    k = f"ba{t}"
    def sub(i, k=k): call(f"/batch/{k}/submit/item{i}", "POST")
    ts = [threading.Thread(target=sub, args=(i,)) for i in range(10)]; [x.start() for x in ts]; [x.join() for x in ts]
    s = pj(call(f"/batch/{k}/stats", "GET")); call(f"/batch/{k}/flush", "POST"); s2 = pj(call(f"/batch/{k}/stats", "GET"))
    good = s["total"] == 10 and s2["flushed_total"] == 10 and s2["pending"] == 0; ok = ok and good
    print(f"  trial {t}: total={s['total']}, flushed={s2['flushed_total']}, pending={s2['pending']} -> {'EXACT (no lost/dup)' if good else 'MISMATCH'}")

# 4) BACKPRESSURE — a Golem promise per submit: the caller durably suspends until
#    its batch flushes. 3 submits block; the 4th fills the max-4 batch; all 4 wake.
print("backpressure(promise): 3 submits BLOCK, the 4th fills the batch -> all 4 release")
k = "bp"; res = {}; done = set()
def go(i): res[i] = call(f"/submit/{k}/item{i}/go", "POST"); done.add(i)
ts = [threading.Thread(target=go, args=(i,)) for i in range(3)]; [x.start() for x in ts]
time.sleep(4)
blocked = len(done) == 0
ts.append(threading.Thread(target=go, args=(3,))); ts[-1].start()
for x in ts: x.join()
allret = len(done) == 4 and all((res.get(i) or "").find(f"ITEM{i}") >= 0 for i in range(4))
ok = ok and blocked and allret
print(f"  3 blocked while batch not full: {blocked}; 4th released all 4 with results: {allret}")

print("\nEach key is a single-writer durable worker -> exact + real backpressure.")
print("(gate-domain's shared-store CAS over-admits / re-buckets and can only poll, not block.)")
raise SystemExit(0 if ok else 1)
PY
