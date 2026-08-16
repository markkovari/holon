#!/usr/bin/env bash
# conduit app-path bench (docs/apps/CONDUIT.md rung 5). Every request goes
# browser -> hyper -> wasmtime -> conduit_domain.composed.wasm
# (conduit-domain + auth-guard + record-store + slug) -> wasi:keyvalue backend.
#
# Usage: bench/conduit-bench.sh [memory|nats]
# Prereqs: oha, a built host + composed wasm (just compose-conduit + host build).
set -euo pipefail
KV="${1:-memory}"
DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"
ADDR=127.0.0.1:3011
BASE="http://$ADDR"
BIN="$ROOT/host/target/release/comp-host"
COMPONENT="$ROOT/components/target/conduit_domain.composed.wasm"

args=(--component "$COMPONENT" --addr "$ADDR" --kv "$KV")
# NATS_URL so a run can point at a server of its own rather than whatever is on
# the default port — which on a developer's machine is usually theirs.
[ "$KV" = "nats" ] && args+=(--nats-url "${NATS_URL:-nats://127.0.0.1:4222}")
# PROFILE=1 counts what the app asks the store for and reports on shutdown. Off by
# default because it takes a lock per operation, which is not what a bench should
# be measuring.
[ "${PROFILE:-}" = 1 ] && args+=(--kv-profile)
# CACHE_MS=<n> turns on the per-node read cache (ADR-0063). Compare a run with and
# without: the guest-side op counts are identical, only what reaches the store moves.
[ -n "${CACHE_MS:-}" ] && args+=(--kv-cache-ms "$CACHE_MS")
VET_TENANT=conduit "$BIN" "${args[@]}" >/tmp/conduit-bench-host.log 2>&1 &
HPID=$!
trap 'kill $HPID 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do curl -sf "$BASE/" >/dev/null 2>&1 && break; sleep 0.1; done

jget() { node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{const o=JSON.parse(d);console.log(eval("o"+process.argv[1]))})' "$1"; }
reg() { curl -s "$BASE/api/users" -d "{\"user\":{\"username\":\"$1\",\"email\":\"$1@b.co\",\"password\":\"password123\"}}" | jget '.user.token'; }

# Seed: author with 3 articles, a reader who follows + favorites.
AUTHOR=$(reg author)
for i in 1 2 3; do
  curl -s "$BASE/api/articles" -H "Authorization: Token $AUTHOR" \
    -d "{\"article\":{\"title\":\"Bench Article $i\",\"description\":\"d$i\",\"body\":\"body $i\",\"tagList\":[\"bench\",\"t$i\"]}}" >/tmp/bench-art$i.json
done
SLUG=$(jget '.article.slug' </tmp/bench-art1.json)
READER=$(reg reader)
curl -s -X POST "$BASE/api/profiles/author/follow" -H "Authorization: Token $READER" >/dev/null
curl -s -X POST "$BASE/api/articles/$SLUG/favorite" -H "Authorization: Token $READER" >/dev/null

row() { # name method path [token] [body]
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
