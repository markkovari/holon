#!/usr/bin/env bash
# RealWorld API conformance for conduit (docs/apps/CONDUIT.md rung 4).
#
# Spins the composed conduit app on the native Rust host (in-memory KV, so each
# run starts clean) and runs the OFFICIAL RealWorld Hurl suite (vendored under
# ./hurl, pinned from gothinkster/realworld specs/api/hurl) against it.
#
# Prereqs: `hurl` (https://hurl.dev), plus a built host + composed wasm — the
# `just conformance-conduit` recipe builds both first.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
HOST="${HOST:-http://127.0.0.1:3009}"
ADDR="${ADDR:-127.0.0.1:3009}"
COMPONENT="$ROOT/components/target/conduit_domain.composed.wasm"
BIN="$ROOT/host/target/release/comp-host"

[ -x "$BIN" ] || { echo "host binary missing: $BIN (build it first)"; exit 1; }
[ -f "$COMPONENT" ] || { echo "composed wasm missing: $COMPONENT (just compose-conduit)"; exit 1; }
command -v hurl >/dev/null || { echo "hurl not installed — see https://hurl.dev"; exit 1; }

VET_TENANT=conduit "$BIN" --component "$COMPONENT" --addr "$ADDR" --kv memory >/tmp/conduit-conformance-host.log 2>&1 &
HOST_PID=$!
trap 'kill "$HOST_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 100); do
  curl -sf "$HOST/" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "Running RealWorld Hurl conformance against $HOST"
hurl --test --jobs 1 \
  --variable "host=$HOST" \
  --variable "uid=$(date +%s)$$" \
  "$DIR"/hurl/*.hurl
