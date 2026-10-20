#!/usr/bin/env bash
# Usage: bench/conduit-bench.sh [memory|nats]
# Prereqs: oha, a built host + composed wasm (cargo xtask compose conduit + host build).
set -euo pipefail
KV="${1:-memory}"
DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"
ADDR=127.0.0.1:3011
BASE="http://$ADDR"
BIN="$ROOT/host/target/release/comp-host"
COMPONENT="$ROOT/components/target/conduit_domain.composed.wasm"

args=(--component "$COMPONENT" --addr "$ADDR" --kv "$KV")
# NATS_URL: the default port on a developer's machine is usually their own server.
[ "$KV" = "nats" ] && args+=(--nats-url "${NATS_URL:-nats://127.0.0.1:4222}")
# PROFILE=1 is off by default: it takes a lock per store operation.
[ "${PROFILE:-}" = 1 ] && args+=(--kv-profile)
[ -n "${CACHE_MS:-}" ] && args+=(--kv-cache-ms "$CACHE_MS")
VET_TENANT=conduit "$BIN" "${args[@]}" >/tmp/conduit-bench-host.log 2>&1 &
HPID=$!
trap 'kill $HPID 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do curl -sf "$BASE/" >/dev/null 2>&1 && break; sleep 0.1; done

jget() { node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{const o=JSON.parse(d);console.log(eval("o"+process.argv[1]))})' "$1"; }
reg() { curl -s "$BASE/api/users" -d "{\"user\":{\"username\":\"$1\",\"email\":\"$1@b.co\",\"password\":\"password123\"}}" | jget '.user.token'; }

AUTHOR=$(reg author)
for i in 1 2 3; do
  curl -s "$BASE/api/articles" -H "Authorization: Token $AUTHOR" \
    -d "{\"article\":{\"title\":\"Bench Article $i\",\"description\":\"d$i\",\"body\":\"body $i\",\"tagList\":[\"bench\",\"t$i\"]}}" >/tmp/bench-art$i.json
done
SLUG=$(jget '.article.slug' </tmp/bench-art1.json)
READER=$(reg reader)
curl -s -X POST "$BASE/api/profiles/author/follow" -H "Authorization: Token $READER" >/dev/null
curl -s -X POST "$BASE/api/articles/$SLUG/favorite" -H "Authorization: Token $READER" >/dev/null

row() {
  local name="$1" method="$2" path="$3" token="${4:-}" body="${5:-}"
  local h=(); [ -n "$token" ] && h=(-H "authorization: Token $token")
  [ -n "$body" ] && h+=(-H "content-type: application/json" -d "$body")
  local dur=10; [ "$name" = "create" ] && dur=3
  local j; j=$(oha --output-format json -z ${dur}s -c 20 -m "$method" ${h[@]+"${h[@]}"} "$BASE$path")
  printf '%-28s %8.0f  %6.1f / %-6.1f\n' "$name" \
    "$(echo "$j" | jget '.summary.requestsPerSec')" \
    "$(echo "$j" | jget '.latencyPercentiles.p50*1000')" \
    "$(echo "$j" | jget '.latencyPercentiles.p99*1000')"
}

echo "=== conduit bench (KV=$KV, oha -c 20) ==="
printf '%-28s %8s  %6s / %-6s\n' route rps 'p50' 'p99(ms)'
row "GET /api/articles"        GET  "/api/articles"
row "GET /api/articles/{slug}" GET  "/api/articles/$SLUG"
row "GET /api/articles/feed"   GET  "/api/articles/feed" "$READER"
row "GET /api/user"            GET  "/api/user" "$READER"
row "GET /api/tags"            GET  "/api/tags"
row "POST /api/users/login"    POST "/api/users/login" "" '{"user":{"email":"reader@b.co","password":"password123"}}'
row "create" POST "/api/articles" "$AUTHOR" '{"article":{"title":"Bench Create","description":"d","body":"b"}}'
row "GET / (usage)"            GET  "/"
