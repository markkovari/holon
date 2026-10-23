#!/usr/bin/env bash
# The Playwright suite: each app composed, hosted on :3000 and driven in a browser, one at a time.
#
#   e2e/run_all.sh [app-domain ...]     every app below, or the ones named
#
# Offline on purpose: an app's model is `mock-provider` (scripted by e2e.conf, which also
# gives Stripe its test key and the outbox a short retry policy), so a spec passes or fails
# on the app, not on a provider. E2E_SKIP_BUILD=1 uses what is built (CI fetches it).
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

APPS=(
  "book-lending-domain"
  "ecommerce-fulfillment-domain"
  "moderation-domain"
  "photosocial-domain"
  "pipeline-domain"
  "real-estate-escrow-domain"
  "smart-home-domain"
  "support-domain"
  "ticket-triage-domain"
  "volunteer-shift-domain"
)
[ $# -gt 0 ] && APPS=("$@")

if [ "${E2E_SKIP_BUILD:-}" != 1 ]; then
  cargo build --release -p comp-host --bin comp-host -p comp-fleet --bin comp-plug || exit 1
  cargo xtask build || exit 1
fi

# Earlier `--dir`s win: with only the mock in the first, it is what `llm:inference` resolves to.
MOCKS=$(mktemp -d)
cp components/target/wasm32-wasip2/release/mock_provider.wasm "$MOCKS/" || exit 1
trap 'rm -rf "$MOCKS"' EXIT

failed=()
for APP in "${APPS[@]}"; do
  NAME=${APP%-domain}
  echo "=== $APP"
  ART=$(target/release/comp-plug --dir "$MOCKS" "$APP") || { failed+=("$APP"); continue; }
  RUN=$(mktemp -d)
  (cd "$RUN" && exec "$ROOT/target/release/comp-host" --app "$APP" --component "$ART" \
    --addr 127.0.0.1:3000 --kv sqlite --config "default-tenant=$NAME" \
    --config-file "$ROOT/e2e/e2e.conf") >"$RUN/host.log" 2>&1 &
  HOST_PID=$!
  for _ in $(seq 1 60); do curl -s -o /dev/null http://127.0.0.1:3000/ && break; sleep 0.25; done
  if ! (cd e2e && npx playwright test "tests/$NAME.spec.js" --reporter=line); then
    echo "--- host log ($RUN/host.log)"; tail -n 20 "$RUN/host.log"
    failed+=("$APP")
  fi
  kill "$HOST_PID" 2>/dev/null; wait "$HOST_PID" 2>/dev/null
done

[ ${#failed[@]} -eq 0 ] || { echo "${#failed[@]} app(s) failed: ${failed[*]}"; exit 1; }
echo "all ${#APPS[@]} app(s) passed"
