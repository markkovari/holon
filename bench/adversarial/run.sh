#!/usr/bin/env bash
# ADR-0023's falsifying measurement: two tenants in ONE comp-host process, one of
# them adversarial, isolation and throughput taken from the SAME run.
set -uo pipefail
# The repo this script lives in, not whatever the caller happened to be in — and
# not a hardcoded path either. This read `cd /Users/…/experiments/comp` for a
# long time: a SIBLING checkout, which meant `just adversarial` built artifacts
# here and then measured the ones over there. A number from the wrong tree is
# worse than no number.
cd "$(dirname "$0")/../.."
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
SPECS=${SPECS:-"--spec fixtures/two-tenants-eve.yaml --spec fixtures/two-tenants-alice.yaml"}
rm -rf $SP/adv $SP/natsA && mkdir -p $SP/adv $SP/natsA
PIDS=()
trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

nats-server -js -sd $SP/natsA -a 127.0.0.1 -p 4232 >$SP/natsA.log 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-stub $SPECS \
  --artifact gate=components/target/gate_domain.composed.wasm --artifact adversary=components/target/wasm32-wasip2/release/adversary.wasm --port 8099 >$SP/plat2.log 2>&1 & PIDS+=($!)
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
# 25 distinct keys, so the adversary has something with a shape to look for.
seeded=0
for i in $(seq 0 24); do
  curl -s -o /dev/null --max-time 8 -X POST http://127.0.0.1:3401/api/ratelimit \
    -H 'content-type: application/json' -H 'Host: shop.eve.test' \
    -d "{\"key\":\"eve-secret-customer-$i\"}" && seeded=$((seeded+1))
done
echo "  wrote $seeded rate-limit records into eve's store"
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
# The sweep result is one document; jq formats it and flags the breaches. Saved to
# sweep.json as well, because ADR-0023's claim is "zero", and a claim like that
# should have the raw evidence beside it.
if ! curl -sf --max-time 60 -H 'Host: probe.alice.test' \
     "http://127.0.0.1:3401/sweep?neighbour=eve/shop" > sweep.json; then
  echo "  SWEEP FAILED"; exit 1
fi
jq -r '
  "  verdict: \(.verdict)",
  "  foreign store opens : \(.foreign_opens)",
  "  foreign keys read   : \(.foreign_keys)",
  "  connections opened  : \(.connections)",
  "  stores:",
  (.stores[] | "    \(.name)  \(.open)  keys=\(.keys // "-")" +
     (if .open == "ok" and (.expected | not) then "  <-- BREACH" else "" end)),
  "  egress:",
  (.egress[] | "    \(.target)  \(.result)" +
     (if .result == "CONNECTED" then "  <-- BREACH" else "" end))
' sweep.json
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
