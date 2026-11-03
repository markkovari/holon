#!/usr/bin/env bash
# The showcase E2E suite: each app composed, hosted on an ephemeral port and driven over CDP
# via `holon-e2e` (Lightpanda for fast functional E2E, or Chromium with --record-gif-dir for GIFs).
#
#   e2e/run_all.sh [app-domain ...]     every app, or the ones named
#
# Offline on purpose: an app's model is `mock-provider` (scripted by e2e.conf, which also
# gives Stripe its test key and the outbox a short retry policy), so a spec passes or fails
# on the app, not on a provider. E2E_SKIP_BUILD=1 uses what is built (CI fetches it).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

if [ "${E2E_SKIP_BUILD:-}" != 1 ]; then
  cargo build --release -p holon-host -p holon-cdp --bin holon-host --bin holon-e2e
  cp -f target/release/holon-host target/release/comp-host
  cargo xtask build
elif [ ! -x "$ROOT/target/release/holon-e2e" ]; then
  cargo build --release -p holon-cdp --bin holon-e2e
fi

exec "$ROOT/target/release/holon-e2e" "$@"
