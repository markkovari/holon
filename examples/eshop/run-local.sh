#!/usr/bin/env bash
# Run the whole eshop on native hosts: 5 services + gateway, one shared NATS
# JetStream KV (the cross-service backbone: sessions, records, event bus).
# Ctrl-C stops everything.
#
# Prereqs:
#   * NATS with JetStream on :4222
#       (docker compose -f infra/compose.yaml up -d nats, or
#        docker run -d --name eshop-nats -p 4222:4222 nats:2.10 -js)
#   * `cargo xtask compose eshop` — composes every service to the names read below
#     (catalog, basket, ordering, payment, identity = accounts-app, gateway, and
#     auth-guard/event-pusher alongside)
#
# Knobs: GRACE (ordering grace period, seconds; default 15 — smoke.sh wants ~3),
#        PAYMENT_SUCCEEDS (default true).
set -euo pipefail
cd "$(dirname "$0")/../.."

nc -z 127.0.0.1 4222 || { echo "NATS not reachable on :4222"; exit 1; }
cargo build --release --bin comp-host --manifest-path host/Cargo.toml

H=host/target/release/comp-host
C=components/target
for w in eshop_identity eshop_catalog eshop_basket eshop_ordering eshop_payment eshop_gateway; do
  [ -f "$C/$w.composed.wasm" ] || { echo "missing $C/$w.composed.wasm — run \`cargo xtask compose eshop\`"; exit 1; }
done

# comp-host takes wasi:config from `--config-file`/`--config` only (the old CFG_*/VET_* env
# scrape was a cross-tenant read on a shared node). One file per host: the example
# defaults (auth-guard policy, vault key, …) plus that service's own keys.
CONF=$(mktemp -d "${TMPDIR:-/tmp}/eshop-conf.XXXXXX")
PIDS=()
trap 'kill "${PIDS[@]}" 2>/dev/null; rm -rf "$CONF"' EXIT

conf() { # conf <name> [key=value ...] -> path of the written file
  local f="$CONF/$1.conf"; shift
  { cat examples/defaults.conf; echo "default-tenant = eshop"; printf '%s\n' "$@"; } >"$f"
  echo "$f"
}

# Every host is the same tenant/app on purpose: the services share ONE store
# (sessions minted by identity are introspected by every other service).
run() { # run <wasm> <addr> <config file> [extra comp-host args ...]
  local wasm=$1 addr=$2 cfg=$3; shift 3
  "$H" --component "$C/$wasm.composed.wasm" --addr "$addr" --kv nats \
       --tenant eshop --app eshop --config-file "$cfg" "$@" &
  PIDS+=($!)
}

run eshop_identity 127.0.0.1:3105 "$(conf identity)"
run eshop_catalog  127.0.0.1:3101 "$(conf catalog)"
run eshop_basket   127.0.0.1:3102 "$(conf basket)"
run eshop_ordering 127.0.0.1:3103 "$(conf ordering "grace-period-secs = ${GRACE:-15}")"
run eshop_payment  127.0.0.1:3104 "$(conf payment "payment-succeeds = ${PAYMENT_SUCCEEDS:-true}")"

# gateway = SPA + proxy:route; the route table is deploy-time config. comp-host is
# default-deny on egress, so the gateway is granted exactly the five services it
# forwards to (all on loopback, hence --allow-private-egress).
ROUTES="/api/identity=http://127.0.0.1:3105/,/api/catalog=http://127.0.0.1:3101,/api/basket=http://127.0.0.1:3102,/api/orders=http://127.0.0.1:3103,/pump/ordering=http://127.0.0.1:3103/internal/pump/,/pump/catalog=http://127.0.0.1:3101/internal/pump/,/pump/payment=http://127.0.0.1:3104/internal/pump/,/pump/basket=http://127.0.0.1:3102/internal/pump/"
run eshop_gateway 127.0.0.1:3100 "$(conf gateway "routes = $ROUTES")" \
  --allow-private-egress \
  --egress 127.0.0.1:3101 --egress 127.0.0.1:3102 --egress 127.0.0.1:3103 \
  --egress 127.0.0.1:3104 --egress 127.0.0.1:3105

echo
echo "eshop up — storefront http://127.0.0.1:3100 (the open page pumps the choreography itself)"
echo "smoke:      GATEWAY=http://127.0.0.1:3100 examples/eshop/smoke.sh   (start with GRACE=3)"
wait
