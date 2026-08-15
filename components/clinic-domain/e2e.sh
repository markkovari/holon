#!/usr/bin/env bash
# The gate for the clinic goal: compose the halves, RUN them, and make real
# requests.
#
# Compiling proves nothing here — `cargo component check` and `cargo component
# build` both pass on a crate that implements none of its world, measured twice in
# this repository. The only thing that can tell a working clinic from a plausible
# one is asking it for something and reading the answer, so that is what this does:
# it plugs the component together with the storage it imports, starts it on the
# host, and drives the sequence in CONTRACT.md.
#
# `$COMP_HOST` is passed in by `holon goal run` (the sandbox holds the base tree
# and nothing else, so the binary cannot be found by path).
set -uo pipefail

MANIFEST=components/Cargo.toml
T="${CARGO_TARGET_DIR:-components/target}"
HOST="${COMP_HOST:-}"
[ -x "$HOST" ] || { echo "no comp-host at '${HOST}' — the gate cannot run what it built"; exit 1; }

# Quiet on success: a check's output IS the feedback a repair reads, and cargo's
# progress drowns the one line that says what went wrong. On failure the build log
# is printed, because then it is the answer.
BUILD_LOG="$(mktemp -t clinic-build-XXXX)"
cargo component build --target wasm32-wasip2 --manifest-path "$MANIFEST" \
  -p clinic-domain -p record-store -p id-generate -p auth-guard -p rate-limiter -p audit-log -p search-index >"$BUILD_LOG" 2>&1 || {
  echo "the halves do not compile:"; tail -25 "$BUILD_LOG"; rm -f "$BUILD_LOG"; exit 1; }
rm -f "$BUILD_LOG"


LOG="$(mktemp -t clinic-log-XXXX)"
PORT=$(( 20000 + RANDOM % 20000 ))
cleanup() { [ -n "${HOST_PID:-}" ] && kill "$HOST_PID" 2>/dev/null; rm -f "$LOG"; }
trap cleanup EXIT

# The plug chain is derived from the component's own imports, not written here: a
# part that reaches for a capability the world already carries would otherwise fail
# this gate for a reason that has nothing to do with its code.
# cargo-component writes to wasip1 or wasip2 depending on version; the freshly
# built artifact is the one this gate must judge, so it is found and passed
# explicitly rather than left to the search order.
for d in wasm32-wasip2 wasm32-wasip1; do
  [ -f "$T/$d/debug/clinic_domain.wasm" ] && OUT="$T/$d/debug" && break
done
[ -n "${OUT:-}" ] || { echo "nothing built under $T"; exit 1; }
PLUG="${COMP_PLUG:-reconciler/target/release/comp-plug}"
[ -x "$PLUG" ] || { echo "no comp-plug at '$PLUG' — the gate cannot assemble what it built"; exit 1; }
COMPOSED="$("$PLUG" clinic-domain --dir "$OUT" 2>&1 | tail -1)" || {
  echo "the halves do not compose: $COMPOSED"; exit 1; }
[ -f "$COMPOSED" ] || { echo "the halves do not compose: $COMPOSED"; exit 1; }

"$HOST" --app clinic --config default-tenant=clinic \
  --component "$COMPOSED" --addr "127.0.0.1:$PORT" >"$LOG" 2>&1 &
HOST_PID=$!

for _ in $(seq 1 60); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)" = "200" ] && break
  sleep 0.5
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
  echo "the component never served /health — it is not up: $(tail -3 "$LOG")"; exit 1; }

B="http://127.0.0.1:$PORT"
fail() { echo "FAILED: $*"; exit 1; }
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
post() { curl -s -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
pcode() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
# `[ "$(pcode … "{\"json\":…}")" = 409 ]` is a nest of quotes inside a command
# substitution inside a test, and bash answered `[: too many arguments` — which
# read, in the run's report, as the CANDIDATE failing. Three generations of a real
# model were judged against a shell bug. One helper, no nesting:
expect_post() { # expect_post <code> <path> <body> <message>
  local want="$1" path="$2" body="$3" msg="$4" got
  got=$(pcode "$path" "$body")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}
expect_get() { # expect_get <code> <url> <message>
  local want="$1" url="$2" msg="$3" got
  got=$(curl -s -o /dev/null -w '%{http_code}' "$url")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}
field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" 2>/dev/null; }

# --- owners and pets ---------------------------------------------------------
OWNER=$(post /api/owners '{"name":"Ada","email":"ada@example.test"}' | field id)
[ -n "$OWNER" ] || fail "POST /api/owners returned no id"
expect_get 200 "$B/api/owners/$OWNER" "the owner does not read back"
expect_post 400 /api/owners '{"name":"","email":"x@y"}' "an empty name is a 400"
expect_post 400 /api/owners '{"name":"Bo","email":"nope"}' "an email without @ is a 400"
expect_get 404 "$B/api/owners/nosuch" "an unknown owner is a 404"

PET=$(post "/api/owners/$OWNER/pets" '{"name":"Rex","species":"dog","born":"2020-01-01"}' | field id)
[ -n "$PET" ] || fail "POST pets returned no id"
[ "$(pcode "/api/owners/$OWNER/pets" '{"name":"X","species":"dragon","born":"2020-01-01"}')" = "400" ] \
  || fail "an unknown species is a 400"
[ "$(pcode "/api/owners/nosuch/pets" '{"name":"X","species":"cat","born":"2020-01-01"}')" = "404" ] \
  || fail "a pet for an unknown owner is a 404"
curl -s "$B/api/owners?q=ada" | grep -q "$OWNER" || fail "search by name does not find the owner"
curl -s "$B/api/owners/$OWNER/pets" | grep -q "$PET" || fail "the owner's pets do not list"

# --- visits, and the rule a compiler cannot check -----------------------------
V1=$(post /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:00:00Z\",\"minutes\":30}" | field id)
[ -n "$V1" ] || fail "POST /api/visits returned no id"
expect_post 409 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:15:00Z\",\"minutes\":30}" "an overlapping visit for the same vet must be a 409"
expect_post 201 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:30:00Z\",\"minutes\":30}" "touching at the boundary is not an overlap"
expect_post 201 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-b\",\"start\":\"2026-09-01T09:15:00Z\",\"minutes\":30}" "a different vet at the same time is fine"
expect_post 400 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T11:00:00Z\",\"minutes\":45}" "45 minutes is not one of 15/30/60"
expect_post 404 /api/visits '{"pet_id":"nosuch","vet":"vet-a","start":"2026-09-01T14:00:00Z","minutes":30}' "a visit for an unknown pet is a 404"
curl -s "$B/api/visits?vet=vet-a&day=2026-09-01" | grep -q "$V1" || fail "the day's visits do not list"

[ "$(code -X DELETE "$B/api/visits/$V1")" = "204" ] || fail "DELETE of a visit is a 204"
expect_post 201 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:00:00Z\",\"minutes\":30}" "a deleted visit must free its slot"

expect_get 404 "$B/api/nope" "an unknown route is a 404"
echo "clinic: every route behaved"
