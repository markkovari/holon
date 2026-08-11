#!/usr/bin/env bash
# Open-loop load, generated on a THIRD machine, across a machine dying.
#
# ADR-0035 killed machines under load and lost zero requests — but the generator
# was closed-loop and ran on the box under test. Both of those flatter the result:
#
#   * closed-loop means a fixed number of threads WAITING for replies, so when
#     nodes die the offered load falls by itself. Real arrivals don't do that. The
#     survivors never actually got the dead nodes' share.
#   * on-box means the load never crossed a network the failure could affect.
#
# So: `oha -q` fixes an arrival RATE (with --latency-correction, which is the flag
# that stops coordinated omission from hiding exactly the stalls we are hunting),
# driven from bobocat, against a fleet of nodes here and on malna.
#
# The rate is picked as a fraction of the measured ceiling such that the SURVIVORS
# cannot serve it. That is the point: an overloaded recovery window is the case
# ADR-0035 could not produce.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
MAC=${MAC:-192.168.100.8}
PI=${PI:-192.168.100.14}
LOAD_HOST=${LOAD_HOST:-bobocat}
KEY="$HOME/.ssh/markkovari_picur_ssh"
# Three machines here — the fleet spans this box and the Pi, and the load comes off
# a third. Any of them absent changes what is being measured, so check first.
. bench/preflight.sh
need_cmd nats-server ssh
need_local_addr "$MAC"
need_remote "markkovari@$PI" "$KEY" "the Pi ($PI)"
# `LOAD_HOST=local` is a supported mode: the generator runs here instead.
[ "$LOAD_HOST" = local ] || need_remote "$LOAD_HOST" "" "the load box ($LOAD_HOST)"
PIDS=()
cleanup() {
  ssh -n -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=10 "markkovari@$PI" \
    "bash -lc 'pkill -f comp-lattice/comp-host; rm -rf ~/comp-lattice/s1 ~/comp-lattice/s2 ~/comp-lattice/s*.log'" >/dev/null 2>&1
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  sleep 1
}
trap cleanup EXIT

# The body lives in a file on the load box rather than inline: it has to survive
# ssh -> fish -> bash -> oha quoting, and a body that arrives mangled is a 400 that
# looks like a platform failure.
#
# `capacity`/`refill` are set high ON PURPOSE. gate-domain is a rate limiter; with
# the default bucket and one key, the first run answered 429 to 13218 of 14078
# requests — measuring the REJECT path, which is cheap and touches none of the
# storage the real path does. (Worth knowing when re-reading ADR-0033's 100k rps:
# oha's "success rate" counts completed requests, not 2xx, so a wall of 429s reads
# as 100% success there too.)
printf %s '{"key":"s","capacity":100000000,"refill":100000000}' > "$SP/body.json"
if [ "$LOAD_HOST" != local ]; then
  scp -q "$SP/body.json" "$LOAD_HOST:comp-load.json"
fi

# LOAD_HOST=local runs the generator here instead of on the load box. Off-box is
# better (ADR-0036) because a kill cannot then quietly reduce offered load through a
# local shortcut — but for an A/B of one ingress setting against another on the same
# fleet, both sides are affected identically and the comparison still holds. Used
# when the load box is asleep.
remote_oha() {
  local label=$1; shift
  if [ "$LOAD_HOST" = local ]; then
    oha --output-format json --no-tui -m POST -D "$SP/body.json" \
      -H content-type:application/json -H Host:shop.eve.test "$@" \
      "http://127.0.0.1:8090/api/ratelimit" >"$SP/oha-$label.json" 2>"$SP/oha-$label.err"
  else
    ssh -n -o BatchMode=yes "$LOAD_HOST" \
      "bash -lc 'oha --output-format json --no-tui -m POST -D ~/comp-load.json \
        -H content-type:application/json -H Host:shop.eve.test $* \
        http://$MAC:8090/api/ratelimit'" >"$SP/oha-$label.json" 2>"$SP/oha-$label.err"
  fi
  ./reconciler/target/release/comp-bench summarise "$SP/oha-$label.json" "$label"
}

mkdir -p "$SP/nats"
nats-server -js -sd "$SP/nats" -a 0.0.0.0 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-stub --spec fixtures/five-replicas.yaml \
  --artifact gate=components/target/gate_domain.composed.wasm --port 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
for n in 1 2; do
  mkdir -p "$SP/mac$n"
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "mac-$n" \
    --lattice stress --addr "127.0.0.1:382$n" --advertise-addr "127.0.0.1:382$n" \
    --state-dir "$SP/mac$n" --label box=mac >"$SP/mac$n.log" 2>&1 & PIDS+=($!)
done
for n in 1 2; do
  ssh -f -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
    "bash -lc 'exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC:4232 --node pi-$n --lattice stress --addr 0.0.0.0:382$n --advertise-addr $PI:382$n --state-dir ~/comp-lattice/s$n --label box=pi > ~/comp-lattice/s$n.log 2>&1'"
done
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice stress --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
# 0.0.0.0, because the load is arriving from another machine — the whole point.
# SHED=0 restores the pre-ADR-0041 behaviour (queue without bound) so the two can
# be compared on the same fleet, same rate, same generator.
./reconciler/target/release/comp-ingress --addr 0.0.0.0:8090 \
  --nats-url nats://127.0.0.1:4232 --lattice stress --refresh-secs 2 \
  --max-inflight "${SHED:-64}" >"$SP/ingress.log" 2>&1 & PIDS+=($!)

echo "  settling (5 replicas over 4 nodes, two machines)..."
sleep 26
./reconciler/target/release/comp-bench inventory --nats-url nats://127.0.0.1:4232 --lattice stress

echo
echo "=== 1. ceiling: closed-loop from $LOAD_HOST, 200 connections, 10s ==="
# This measures the PATH, not the fleet: at ~24ms LAN RTT, 200 connections cap out
# around 8k rps no matter how fast the nodes are. ADR-0033's 100k rps was generated
# on the box being measured, which is why it is so much larger — the difference is
# the network, not the platform.
remote_oha ceiling -c 200 -z 10s
# One field out of oha's JSON. `comp-bench summarise` prints the whole line; this
# wants just the rate, so it takes it from the same file with the same tool.
CEIL=$(./reconciler/target/release/comp-bench summarise "$SP/oha-ceiling.json" ceiling | awk '{print int($2)}')
RATE=${RATE:-$((CEIL * 60 / 100))}
echo "  -> ceiling ${CEIL} rps; open-loop rate set to ${RATE} rps (60%)"

echo
echo "=== 2. baseline: open-loop at ${RATE} rps for 20s, whole fleet up ==="
remote_oha baseline -q "$RATE" -c 200 -z 20s --latency-correction

echo
echo "=== 3. bursts on a HEALTHY fleet: same total offered, delivered in spikes ==="
# BEFORE the kills, deliberately. On the first run this sat after them and so
# measured bursts against a Pi-only fleet — which reads as a burst finding and is
# really the previous phase's wreckage.
#
# Arrivals in real life are not smooth. `--burst-rate N --burst-delay 1s` sends N
# at once, then waits — the thundering-herd shape a steady rate cannot produce.
remote_oha burst --burst-rate "$((RATE / 2))" --burst-delay 1s -c 200 -z 20s --latency-correction

echo
echo "=== 4. same rate, 70s, killing BOTH local nodes so malna absorbs everything ==="
# Killing the Pi and leaving the Mac cannot overload anything — the Mac serves the
# whole rate alone, which is what ADR-0035 measured and why it lost nothing. The
# stress case is the reverse: take out the fast half and make the surviving machine,
# on the far end of the lattice, take the full arrival rate while it is also being
# handed replicas it was not running.
#
# Killed by NODE NAME, not by `--lattice stress`: the reconciler and the ingress
# carry that same string in their own command lines, so the obvious pkill takes out
# the control plane and turns "malna absorbed it" into "nothing was watching".
( sleep 15; echo "$(date +%s) kill mac-1 and mac-2" >>"$SP/events.txt"
  pkill -9 -f -- "--node mac-1"; pkill -9 -f -- "--node mac-2" ) &
PIDS+=($!)
remote_oha killed -q "$RATE" -c 200 -z 70s --latency-correction

echo
echo "=== after ==="
./reconciler/target/release/comp-bench inventory --nats-url nats://127.0.0.1:4232 --lattice stress
