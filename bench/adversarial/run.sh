#!/usr/bin/env bash
# ADR-0023's falsifying measurement: two tenants in ONE comp-host process, one of
# them adversarial, isolation and throughput taken from the SAME run.
set -uo pipefail
cd /Users/markkovari/DEV/markkovari/experiments/comp
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
MANIFEST=${MANIFEST:-$HERE/two-tenants.json}
rm -rf $SP/adv $SP/natsA && mkdir -p $SP/adv $SP/natsA
PIDS=()
trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

nats-server -js -sd $SP/natsA -a 127.0.0.1 -p 4232 >$SP/natsA.log 2>&1 & PIDS+=($!)
python3 $HERE/stub-control-plane.py "$MANIFEST" \
  '{"gate":"components/target/gate_domain.composed.wasm","adversary":"components/target/wasm32-wasip2/release/adversary.wasm"}' \
  8099 >$SP/plat2.log 2>&1 & PIDS+=($!)
sleep 2

# ONE host process. Both tenants live in it. That is the whole point.
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node solo \
  --lattice adv --addr 127.0.0.1:3401 --state-dir $SP/adv \
  --kv ${KV:-sqlite} --nats-url 127.0.0.1:4232 --sqlite-path $SP/adv/kv.db >$SP/solo.log 2>&1 & PIDS+=($!)
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice adv \
  --interval 3 --inventory-ttl 15 >$SP/rec2.log 2>&1 & PIDS+=($!)

echo "=== placing both tenants on one host ==="
sleep 16
grep -E "started" $SP/solo.log | sed 's/^/  /'
echo "  pid $(pgrep -f 'node solo' | head -1) — one process, two tenants"

echo
echo "=== seeding eve with data the adversary will try to read ==="
python3 - <<'PY'
import json, urllib.request
for i in range(25):
    r = urllib.request.Request("http://127.0.0.1:3401/api/ratelimit",
        data=json.dumps({"key": f"eve-secret-customer-{i}"}).encode(),
        headers={"content-type": "application/json", "Host": "shop.eve.test"})
    try: urllib.request.urlopen(r, timeout=8).read()
    except Exception as e: print("  seed failed:", str(e)[:70]); break
else: print("  wrote 25 rate-limit records into eve's store")
PY
echo "  eve's rows (sqlite only; nats keeps them in JetStream):"
sqlite3 $SP/adv/kv.db "SELECT bucket, count(*) FROM kv GROUP BY bucket;" 2>/dev/null | sed 's/^/    /' || echo "    (nats backend)"

echo
echo "=== load on eve, and the sweep runs while it is under load ==="
oha -z 20s -c 50 --no-tui -m POST -d '{"key":"load"}' \
  -H 'content-type: application/json' -H 'Host: shop.eve.test' \
  http://127.0.0.1:3401/api/ratelimit >$SP/oha.txt 2>&1 &
OHA=$!
sleep 6
echo "--- sweep, taken mid-load ---"
python3 - <<'PY'
import json, urllib.request
r = urllib.request.Request("http://127.0.0.1:3401/sweep?neighbour=eve/shop",
    headers={"Host": "probe.alice.test"})
try:
    d = json.loads(urllib.request.urlopen(r, timeout=60).read())
except Exception as e:
    print("  SWEEP FAILED:", str(e)[:120]); raise SystemExit(1)
print(f"  verdict: {d['verdict']}")
print(f"  foreign store opens : {d['foreign_opens']}")
print(f"  foreign keys read   : {d['foreign_keys']}")
print(f"  connections opened  : {d['connections']}")
print("  stores:")
for s in d["stores"]:
    mark = "  <-- BREACH" if s.get("open") == "ok" and not s.get("expected") else ""
    print(f"    {s['name']!r:38} {s['open']:8} keys={s.get('keys','-')}{mark}")
print("  egress:")
for e in d["egress"]:
    mark = "  <-- BREACH" if e["result"] == "CONNECTED" else ""
    print(f"    {e['target']:24} {e['result']}{mark}")
json.dump(d, open("sweep.json","w"), indent=1)
PY
wait $OHA
echo
echo "--- throughput, same run ---"
grep -E "Requests/sec|Success rate|^  50.00%|^  99.00%|Total:" $SP/oha.txt | sed 's/^/  /'
echo
echo "--- did the host log any refusals? ---"
grep -c "denied egress" $SP/solo.log | sed 's/^/  denied egress attempts: /'
grep "denied egress" $SP/solo.log | head -4 | sed 's/^/    /'
echo
echo "--- resident memory of the one process holding both tenants ---"
ps -o rss= -p $(pgrep -f 'node solo' | head -1) | awk '{printf "  %.0f MiB\n", $1/1024}'
