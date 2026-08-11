#!/usr/bin/env bash
# Two organisations, five members each, both deploying and both under load.
#
# The interesting question is not "how fast is one app" — ADR-0030 answered that.
# It is what happens when the platform is doing the thing it exists for: several
# organisations, several people in each, sharing one fleet. So this measures three
# things in ONE run, because measuring them apart is how you get a throughput
# number from an idle box and a safety number from a quiet one:
#
#   1. what the control plane costs   (register / login / upload / deploy)
#   2. what the data plane delivers   (rps and tail, both orgs at once)
#   3. whether isolation survives it  (org A's data, read from org B, under load)
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
NODES=${NODES:-3}
# Set PI=<host> to put nodes on a second machine too. The point is NOT that org A
# lives on one box and org B on another — that is just pinning tenants to
# computers with extra steps. Both orgs' apps must interleave across every node.
PI=${PI:-}
PI_NODES=${PI_NODES:-2}
APPS=${APPS:-3}
KEY="$HOME/.ssh/markkovari_picur_ssh"
# Local by default; only checked when a second machine was actually asked for.
. bench/preflight.sh
need_cmd nats-server
[ -z "$PI" ] || need_remote "markkovari@$PI" "$KEY" "the Pi ($PI)"
DURATION=${DURATION:-20s}
CONNS=${CONNS:-40}
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT
mkdir -p "$SP/nats" "$SP/plat"
C=./cli/target/release/comp
run() { local who=$1; shift; COMP_CREDENTIALS="$SP/$who.json" $C "$@"; }
# `date +%s%3N` is GNU-only; this is the portable spelling and costs no process.
ms() { perl -MTime::HiRes=time -e "printf(\"%d\", time()*1000)"; }

nats-server -js -sd "$SP/nats" -a 0.0.0.0 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
./host/target/release/comp-host --component components/target/platform_domain.composed.wasm \
  --addr 127.0.0.1:8080 --kv sqlite --sqlite-path "$SP/plat/kv.db" \
  --tenant platform --app control-plane \
  --config applier-secret=s3cret --config ingress-suffix=bench.test >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8080 --secret s3cret \
  --nats-url nats://127.0.0.1:4232 --lattice bench --interval 3 >"$SP/rec.log" 2>&1 & PIDS+=($!)
for n in $(seq 1 "$NODES"); do
  mkdir -p "$SP/n$n"
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "n$n" --lattice bench \
    --addr "127.0.0.1:39$(printf '%02d' "$n")" --advertise-addr "127.0.0.1:39$(printf '%02d' "$n")" \
    --state-dir "$SP/n$n" >"$SP/n$n.log" 2>&1 & PIDS+=($!)
done
if [ -n "$PI" ]; then
  MAC_IP=${MAC_IP:-192.168.100.8}
  for n in $(seq 1 "$PI_NODES"); do
    ssh -f -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
      "bash -lc 'mkdir -p ~/comp-lattice/b$n; exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC_IP:4232 --node pi-$n --lattice bench --addr 0.0.0.0:39$(printf '%02d' $((50+n))) --advertise-addr $PI:39$(printf '%02d' $((50+n))) --state-dir ~/comp-lattice/b$n > ~/comp-lattice/b$n.log 2>&1'"
  done
  sleep 2
fi
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8095 --nats-url nats://127.0.0.1:4232 \
  --lattice bench --refresh-secs 2 >"$SP/ingress.log" 2>&1 & PIDS+=($!)
sleep 4

echo "=== 1. control plane: 10 users across 2 orgs ==="
t0=$(ms)
for org in acme globex; do
  for i in 1 2 3 4 5; do
    # The personal tenant is derived from the email's LOCAL PART, so `u1@acme.test`
    # and `u1@globex.test` would collide into one tenant. Distinct local parts.
    run "$org$i" login --url http://127.0.0.1:8080 --email "$org-u$i@bench.test" \
      --password "correct-horse-$org-$i" --register >/dev/null 2>&1
  done
done
t1=$(ms)
printf "  register+login  10 users   %5d ms total  %5d ms each\n" $((t1-t0)) $(((t1-t0)/10))

t0=$(ms)
for org in acme globex; do
  run "${org}1" org create "$org" >/dev/null 2>&1 || echo "    $org: org create failed"
  for i in 2 3 4 5; do
    CODE=$(run "${org}1" org invite "$org" --role member 2>/dev/null | awk '/invite code/{print $3}')
    run "$org$i" org join "$CODE" >/dev/null 2>&1
  done
done
t1=$(ms)
printf "  create+invite   2 orgs     %5d ms total\n" $((t1-t0))
for org in acme globex; do
  printf "    %-8s members: %s\n" "$org" "$(run "${org}1" org members "$org" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')"
done

echo
echo "=== 2. each org deploys, by a different member than the owner ==="
t0=$(ms)
for org in acme globex; do
  run "${org}3" component push components/target/gate_domain.composed.wasm --id gate >/dev/null 2>&1
done
t1=$(ms); printf "  component push  2 uploads  %5d ms\n" $((t1-t0))
sleep 9
t0=$(ms)
for org in acme globex; do
 for a in $(seq 1 "$APPS"); do
  run "${org}3" app create "shop$a" --strategy fused --component gate --org "$org" >/dev/null 2>&1
  ID=$(run "${org}3" app ls 2>/dev/null | awk -v want="shop$a" '$2==want{print $1}' | head -1)
  [ "$a" = 1 ] && echo "$ID" > "$SP/$org.id"
  # A fused deploy needs one distribution pass before the composed artifact has a
  # content address (ADR-0028), so the first save legitimately fails.
  for attempt in 1 2 3 4 5 6; do
    if out=$(run "${org}3" app deploy "$ID" 2>&1); then
      break
    fi
    [ "$attempt" = 6 ] && echo "    $org/shop$a FAILED: $(echo "$out" | head -1)"
    sleep 6
  done
 done
done
t1=$(ms); printf "  create+deploy   2 apps     %5d ms (includes waiting for distribution)\n" $((t1-t0))
sleep 16
echo "  placement — every node should hold apps from BOTH orgs:"
# Straight from inventory, which already covers the remote nodes — this used to
# scrape every host's log and ssh to the Pi to collect the ones it could not see.
./reconciler/target/release/comp-bench tenants --nats-url nats://127.0.0.1:4232 --lattice bench

echo
echo "=== 3. both orgs under load at once, ${DURATION} x ${CONNS} conns each ==="
LOAD=()
for org in acme globex; do
  oha -z "$DURATION" -c "$CONNS" --no-tui -m POST -d '{"key":"load","capacity":100000000,"refill":100000000}' \
    -H 'content-type: application/json' -H "Host: shop1.$org.bench.test" \
    http://127.0.0.1:8095/api/ratelimit >"$SP/oha-$org.txt" 2>&1 &
  LOAD+=($!)
done
# Wait for the LOAD generators specifically. A bare `wait` waits for every
# background job, and the servers started above never exit — measured, by hanging.
wait "${LOAD[@]}"
for org in acme globex; do
  printf "  %-8s %s\n" "$org" "$(awk '/Requests\/sec/{r=$2} /^  50.00%/{p50=$3} /^  99.00%/{p99=$3} END{printf "%8.0f rps  p50 %-9s p99 %-9s", r, p50, p99}' "$SP/oha-$org.txt")"
  # oha's "success rate" counts COMPLETED requests, not 2xx. Reporting it alone once
  # published 102k rps that were 1.5M ingress 503s: the apps had been renamed to
  # shop1..N and the load still asked for `shop.<org>`, so nothing was ever routed
  # to a component. The status codes are the assertion; the rps is just a number.
  codes=$(awk '/Status code distribution/{f=1;next} /^$/{f=0} f{gsub(/[][]/,"");printf "%s:%s ", $1, $2}' "$SP/oha-$org.txt")
  printf "           codes: %s\n" "$codes"
  case "$codes" in
    *200:*) ;;
    *) echo "           ^^ NO 2xx AT ALL — this measured an error path, not the platform" ;;
  esac
done

echo
echo "=== 4. isolation, checked after the load rather than before ==="
# A lattice node defaults to --kv nats (ADR-0027), so the stores are JetStream
# buckets rather than files. One bucket per (org, app), which is the isolation
# claim: derived from the ORG, not from whoever ran the deploy.
for b in $(nats --server 127.0.0.1:4232 kv ls 2>/dev/null | grep -o 'b-app-[a-z0-9-]*' | sort -u); do
  printf "    %-22s %s keys\n" "$b" "$(nats --server 127.0.0.1:4232 kv ls "$b" 2>/dev/null | wc -l | tr -d ' ')"
done
echo "  can an acme member read globex's app?"
AID=$(cat "$SP/globex.id" 2>/dev/null)
run acme4 app show "$AID" 2>&1 | head -1 | sed 's/^/    /'
echo "  can an acme member deploy into globex?"
run acme4 app create sneak --component gate --org globex 2>&1 | head -1 | sed 's/^/    /'

echo
echo "=== 5. what it cost to hold both ==="
for n in $(seq 1 "$NODES"); do
  pid=$(pgrep -f "node n$n --lattice bench" | head -1)
  [ -n "$pid" ] && ps -o rss= -p "$pid" | awk -v n="$n" '{printf "    node %s  %6.0f MiB\n", n, $1/1024}'
done
pid=$(pgrep -f "app control-plane" | head -1)
[ -n "$pid" ] && ps -o rss= -p "$pid" | awk '{printf "    control plane %.0f MiB\n", $1/1024}'
