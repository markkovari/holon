#!/usr/bin/env bash
# Organisations end to end: two people share one org, a third is refused, and the
# deployment lands under the ORG rather than under whoever typed the command.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT
mkdir -p "$SP/nats" "$SP/plat" "$SP/node"

nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
./host/target/release/comp-host --component components/target/platform_domain.composed.wasm \
  --addr 127.0.0.1:8080 --kv sqlite --sqlite-path "$SP/plat/kv.db" \
  --tenant platform --app control-plane \
  --config applier-secret=s3cret --config ingress-suffix=apps.local >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8080 --secret s3cret \
  --nats-url nats://127.0.0.1:4232 --lattice orgs --interval 3 >"$SP/rec.log" 2>&1 & PIDS+=($!)
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node n1 --lattice orgs \
  --addr 127.0.0.1:3801 --advertise-addr 127.0.0.1:3801 --state-dir "$SP/node" >"$SP/n1.log" 2>&1 & PIDS+=($!)
sleep 3

C=./cli/target/debug/comp
as() { COMP_CREDENTIALS="$SP/$1.json" shift; }
run() { local who=$1; shift; COMP_CREDENTIALS="$SP/$who.json" $C "$@"; }

echo "=== three people register; each gets a solo org ==="
for who in ada grace linus; do
  run $who login --url http://127.0.0.1:8080 --email "$who@corp.test" --password "correct-horse-$who" --register >/dev/null
  echo -n "  $who: "; run $who org ls | tail -n +2 | awk '{printf "%s(%s) ", $1, $3}'; echo
done

echo
echo "=== ada creates a shared org and invites grace ==="
run ada org create "Acme Corp" | sed 's/^/  /'
CODE=$(run ada org invite acme-corp --role member | awk '/invite code/{print $3}')
echo "  code: $CODE"
run grace org join "$CODE" | sed 's/^/  /'
echo "  members of acme-corp:"; run ada org members acme-corp | tail -n +2 | sed 's/^/    /'

echo
echo "=== grace deploys INTO the shared org ==="
run grace component push components/target/gate_domain.composed.wasm --id gate >/dev/null
sleep 8
run grace app create shop --strategy fused --component gate --org acme-corp | sed 's/^/  /'
ID=$(run grace app ls | awk 'NR==2{print $1}')
# A fused deployment composes on the first save and needs one distribution pass
# before the composed artifact has a content address (ADR-0028). Two saves, by
# design — retried here rather than papered over.
for attempt in 1 2 3; do
  if run grace app deploy "$ID" 2>/dev/null | sed 's/^/  /'; then break; fi
  echo "  (waiting for the composed artifact to be distributed)"; sleep 8
done

echo
echo "=== can ada see it? (same org, she never touched it) ==="
run ada app ls | sed 's/^/  /'
echo "=== can linus? (not a member) ==="
run linus app ls | sed 's/^/  /'
echo "  linus tries to read it directly:"
run linus app show "$ID" 2>&1 | head -2 | sed 's/^/    /'
echo "  linus tries to deploy into acme-corp:"
run linus app create sneak --component gate --org acme-corp 2>&1 | head -2 | sed 's/^/    /'

echo
echo "=== whose storage bucket did it get? ==="
# jq rather than a language runtime: this is one field out of one document, which
# is what jq is for.
run ada app manifest "$ID" 2>/dev/null | jq -r '.manifest |
  "    \(.tenant)/\(.app) -> env \(.env) | ingress \(.ingress.host)"' 
