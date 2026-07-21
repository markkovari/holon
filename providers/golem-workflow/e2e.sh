#!/usr/bin/env bash
# Live end-to-end: provider bridge -> a real durable Golem worker.
# Downloads the Golem 1.5 binary if needed, runs the local server, deploys the
# bundled demo agent, and runs the gated bridge test against it. macOS/arm64.
set -euo pipefail
cd "$(dirname "$0")"
BIN=.bin/golem
ARCH="$(uname -m)"; OS="$(uname -s)"
[ "$OS" = Darwin ] && PLAT="$ARCH-apple-darwin" || PLAT="$ARCH-unknown-linux-gnu"

if [ ! -x "$BIN" ]; then
  echo "downloading golem 1.5.5 ($PLAT)..."; mkdir -p .bin
  curl -fsSL "https://github.com/golemcloud/golem/releases/download/v1.5.5/golem-$PLAT" -o "$BIN"
  chmod +x "$BIN"
fi

# start the local server if the gateway (:9006) isn't already up
if ! curl -s -o /dev/null -m 2 http://127.0.0.1:9006/ 2>/dev/null; then
  echo "starting golem server..."; "$BIN" server run --clean >/tmp/golem-server.log 2>&1 &
  for _ in $(seq 1 60); do lsof -nP -iTCP:9006 -sTCP:LISTEN >/dev/null 2>&1 && break; sleep 2; done
fi

# scaffold the demo agent fresh (deterministic; gitignored — Golem app dirs
# carry generated state that shouldn't be vendored)
if [ ! -d golem-agent ]; then
  echo "scaffolding demo agent (golem new)..."
  "$BIN" new --template rust --component-name book:flight --yes golem-agent >/dev/null
fi
echo "building + deploying the demo agent..."
( cd golem-agent && "../$BIN" build && "../$BIN" deploy -Y ) 2>&1 | tail -3
# discover the deployed gateway host (e.g. golem-agent.localhost:9006)
HOST=$(cd golem-agent && "../$BIN" deploy -Y 2>&1 | grep -oE '[a-z0-9-]+\.localhost:9006' | head -1)
HOST="${HOST:-bookapp.localhost:9006}"
echo "gateway host: $HOST"

echo "=== running the live bridge test ==="
GOLEM_E2E=1 GOLEM_HOST="$HOST" cargo test --release bridge_invokes_a_real_durable_golem_worker -- --nocapture
