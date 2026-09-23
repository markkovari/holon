#!/usr/bin/env bash
# photoquest end-to-end: the whole stack, a real browser, CC0 photos, then gone.
#
#   bash e2e/photoquest.sh [extra playwright args, e.g. -g "Privacy"]
#
# Brings up, in order:
#   - RustFS from infra/compose.yaml (profile `media`) as its own compose project,
#     with its own volume
#   - a private `nats-server -js` on a free port with a temp store dir — never
#     the box's :4222, which on a dev machine is often somebody else's NATS.
#     comp-media is pointed at it with MEDIA_NATS_URL, which `cargo xtask host`
#     passes through to the daemons it spawns
#   - on macOS, the Swift helper (comp-media-apple) if it is missing or older
#     than its source; on Linux there is none and evaluation runs on the CPU
#   - `cargo xtask host photoquest` (comp-host on :3941 + comp-media on :8013),
#     with the host's sqlite in a temp dir (STATE_DIRECTORY), and two config keys
#     the committed apps/photoquest.toml must not carry, given for this run only
#     with `--config` (after the toml's [config], so they add to it):
#       allow-test-routes=true      POST /test/clock, to pass deadlines
#       bootstrap-admin-email=...   the account that is admin from register on
# then warms the pipeline with one upload, runs the three photoquest specs
# (tests/photoquest*.spec.js: photographer, curator, admin) on one worker — the
# evaluator is one queue and the test clock is one for the whole app — and on
# exit — pass, fail or Ctrl-C — stops the host and the daemon, the NATS, and
# removes the containers, the volume and the temp dirs.
#
# Media: only the two CC0 a7R V samples from raw.pixls.us, downloaded once into
# e2e/.photoquest-samples/ (gitignored; PHOTOQUEST_SAMPLES overrides the dir) and
# checked against the sha256 below. The JPEG sample is cut out of one of them.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
E2E="$ROOT/e2e"
SAMPLES="${PHOTOQUEST_SAMPLES:-$E2E/.photoquest-samples}"
PIXLS="https://raw.pixls.us/data/Sony/ILCE-7RM5"
COMPOSE=(docker compose -p holon-photoquest-e2e -f "$ROOT/infra/compose.yaml" --profile media)
APP_PORT=3941 MEDIA_PORT=8013   # fixed by apps/photoquest.toml
# The suite's admin (lib/photoquest.js reads the same variable). Nobody can
# register it before the suite does: the store is a fresh temp dir every run.
export PHOTOQUEST_ADMIN_EMAIL="${PHOTOQUEST_ADMIN_EMAIL:-admin@photoquest.test}"
SPECS=(tests/photoquest.spec.js tests/photoquest-curator.spec.js tests/photoquest-admin.spec.js)
STORE_PORTS=(9000 9001)         # fixed by infra/compose.yaml

say() { printf '\033[1;36m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m!! %s\033[0m\n' "$*" >&2; exit 1; }
listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# ---- teardown ---------------------------------------------------------------

TMP=$(mktemp -d "${TMPDIR:-/tmp}/photoquest-e2e.XXXXXX")
HOST_PID="" NATS_PID="" STORE_UP=""
cleanup() {
  local code=$?
  trap - EXIT INT TERM
  say "tearing down"
  # `cargo xtask host` runs in its own process group (set -m below): comp-host
  # and comp-media are its children, and a SIGTERM to xtask alone would not
  # run its Drop guard, so the whole group is signalled.
  if [[ -n "$HOST_PID" ]]; then
    kill -TERM -- "-$HOST_PID" 2>/dev/null || true
    for _ in $(seq 50); do kill -0 -- "-$HOST_PID" 2>/dev/null || break; sleep 0.1; done
    kill -KILL -- "-$HOST_PID" 2>/dev/null || true
  fi
  [[ -n "$NATS_PID" ]] && { kill "$NATS_PID" 2>/dev/null || true; wait "$NATS_PID" 2>/dev/null || true; }
  [[ -n "$STORE_UP" ]] && "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$TMP"
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# ---- preflight --------------------------------------------------------------

for tool in docker nats-server node npx cargo curl; do
  command -v "$tool" >/dev/null || die "$tool is not on PATH"
done
docker info >/dev/null 2>&1 || die "docker is not running"
for p in "$APP_PORT" "$MEDIA_PORT" "${STORE_PORTS[@]}"; do
  listening "$p" && die "something already listens on 127.0.0.1:$p — stop it first (a running 'cargo xtask host photoquest' or compose rustfs?)"
done

# ---- samples (CC0, raw.pixls.us) ---------------------------------------------

sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
fetch() { # name sha256
  local f="$SAMPLES/$1"
  if [[ ! -f "$f" ]] || [[ "$(sha "$f")" != "$2" ]]; then
    say "downloading $1 (CC0, raw.pixls.us)"
    curl -fL --retry 3 -o "$f.part" "$PIXLS/$1"
    [[ "$(sha "$f.part")" == "$2" ]] || die "$1: sha256 mismatch"
    mv "$f.part" "$f"
  fi
}
mkdir -p "$SAMPLES"
fetch 7RM5-LosslessUncompressed.ARW 8918ba274f0919be4f9902f9eebcab4663445ebe441d22fb48f3f328198ac2ee
fetch 7RM5-LosslessCompressedLarge.ARW c294c388ad9bccbee169f87b9ceb661001515fb039d63f587dcabba76df5636d
printf 'Source: %s/\nLicense: CC0 1.0 (public domain), per raw.pixls.us\n7RM5-preview.jpg: the JPEG preview embedded in 7RM5-LosslessCompressedLarge.ARW\n' "$PIXLS" > "$SAMPLES/LICENSE.txt"

# ---- node deps ----------------------------------------------------------------

cd "$E2E"
[[ -d node_modules/@playwright/test ]] || { say "npm ci (e2e)"; npm ci --no-audit --no-fund; }
npx playwright install chromium >/dev/null
node lib/photoquest.js derive-jpeg "$SAMPLES/7RM5-LosslessCompressedLarge.ARW" "$SAMPLES/7RM5-preview.jpg"

# ---- build --------------------------------------------------------------------

cd "$ROOT"
# Always compose: `cargo xtask host` composes only when the artifact is missing,
# and a stale one would test yesterday's component. comp-media is built by
# `cargo xtask host` itself (below) — not here: a plain `cargo build` outside
# xtask sees a different environment, and the two builds kept invalidating each
# other's fingerprints (ring, rustls, … rebuilt on every run).
say "composing photoquest"
cargo xtask compose photoquest
HELPER=reconciler/target/comp-media-apple
if [[ "$(uname -s)" == Darwin ]]; then
  if [[ ! -x "$HELPER" || tools/media-apple/main.swift -nt "$HELPER" ]]; then
    say "building comp-media-apple (Core Image, Metal, Vision)"
    swiftc -O -o "$HELPER" tools/media-apple/main.swift
  fi
else
  say "not macOS: no Swift helper, comp-media evaluates on the CPU (rawler, no Vision)"
fi

# ---- store ----------------------------------------------------------------------

say "starting RustFS"
STORE_UP=1
"${COMPOSE[@]}" up -d --wait rustfs

# ---- private JetStream ----------------------------------------------------------

NATS_PORT=$(node -e 'const s=require("net").createServer().listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
say "starting a private nats-server -js on :$NATS_PORT"
nats-server -js -a 127.0.0.1 -p "$NATS_PORT" -sd "$TMP/jetstream" >"$TMP/nats.log" 2>&1 &
NATS_PID=$!
for _ in $(seq 50); do listening "$NATS_PORT" && break; sleep 0.1; done
listening "$NATS_PORT" || { cat "$TMP/nats.log"; die "nats-server did not start"; }

# ---- the app + comp-media -------------------------------------------------------

say "starting cargo xtask host photoquest"
mkdir -p "$TMP/state"
set -m  # its own process group, so cleanup can stop comp-host and comp-media with it
MEDIA_NATS_URL="nats://127.0.0.1:$NATS_PORT" STATE_DIRECTORY="$TMP/state" \
  cargo xtask host photoquest --addr "127.0.0.1:$APP_PORT" \
    --config allow-test-routes=true \
    --config "bootstrap-admin-email=$PHOTOQUEST_ADMIN_EMAIL" >"$TMP/host.log" 2>&1 &
HOST_PID=$!
set +m

ready() { curl -fsS "$1" >/dev/null 2>&1; }
# Up to 15 minutes: from a clean checkout this includes building comp-host and
# comp-media in release.
say "waiting for :$APP_PORT and comp-media on :$MEDIA_PORT (builds them first if needed; log: $TMP/host.log)"
for _ in $(seq 4500); do
  ready "http://127.0.0.1:$APP_PORT/health" && ready "http://127.0.0.1:$MEDIA_PORT/health" && break
  kill -0 "$HOST_PID" 2>/dev/null || { cat "$TMP/host.log"; die "cargo xtask host exited"; }
  sleep 0.2
done
ready "http://127.0.0.1:$APP_PORT/health" || { tail -50 "$TMP/host.log"; die "app did not answer on :$APP_PORT"; }
HEALTH=$(curl -fsS "http://127.0.0.1:$MEDIA_PORT/health")
say "comp-media: $HEALTH"
[[ "$HEALTH" == *'"store":true'* && "$HEALTH" == *'"queue":true'* ]] || { tail -50 "$TMP/host.log"; die "comp-media is not healthy"; }
# The test routes answer 404 unless the extra config reached the component.
curl -fsS -X POST -H 'content-type: application/json' -d '{"offset_secs":0}' \
  "http://127.0.0.1:$APP_PORT/test/clock" >/dev/null || die "POST /test/clock refused — did --config allow-test-routes=true reach the app?"

# ---- warm, then test --------------------------------------------------------------

cd "$E2E"
say "warming the pipeline (the first job pays Core Image's cold start)"
node lib/photoquest.js warm "$SAMPLES/7RM5-LosslessCompressedLarge.ARW" || { tail -50 "$TMP/host.log"; die "warm-up upload failed"; }

say "running ${SPECS[*]}"
status=0
PHOTOQUEST_SAMPLES="$SAMPLES" npx playwright test "${SPECS[@]}" --workers=1 "$@" || status=$?
if [[ $status -ne 0 ]]; then
  say "last lines of the host log"
  tail -40 "$TMP/host.log"
fi
exit $status
