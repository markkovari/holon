#!/usr/bin/env bash
# clinic: staff access and pet search
#
# Judges the `access-and-search` part, and judges it on two different things.
#
# BEHAVIOUR, because compiling proves nothing: `cargo component build` passes on a
# crate that implements none of its world, measured twice in this repository. So
# this starts the component and asks it for things.
#
# COMPOSITION, because behaviour alone cannot tell "called auth-guard" from
# "wrote its own sha256 and a `HashMap<String, String>` of sessions". Both answer
# 200. The composed component's IMPORTS can tell them apart, and the whole point
# of this part is reuse rather than reimplementation.
#
# The plug chain is not written here. `comp-plug` derives it from the component's
# own imports (`reconciler/src/plug.rs`, which wraps `wac` as a library) — so when a
# part reaches for a capability the world already carries, the gate keeps working
# with no edit. That is the only way an agent can use a capability nobody handed it.
set -uo pipefail

MANIFEST=components/Cargo.toml
HOST="${COMP_HOST:-}"
[ -x "$HOST" ] || { echo "no comp-host at '${HOST}' — the gate cannot run what it built"; exit 1; }

BUILD_LOG="$(mktemp -t clinic-build-XXXX)"
cargo component build --target wasm32-wasip2 --manifest-path "$MANIFEST" \
  -p clinic-domain -p record-store -p id-generate -p auth-guard -p rate-limiter -p audit-log -p search-index -p csv >"$BUILD_LOG" 2>&1 || {
  echo "the clinic does not compile:"; tail -25 "$BUILD_LOG"; rm -f "$BUILD_LOG"; exit 1; }
rm -f "$BUILD_LOG"

T="${CARGO_TARGET_DIR:-components/target}"
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
  echo "the clinic does not compose: $COMPOSED"; exit 1; }
[ -f "$COMPOSED" ] || { echo "the clinic does not compose: $COMPOSED"; exit 1; }

# --- what it was built out of -------------------------------------------------
#
# Read off the UNCOMPOSED artifact, and the distinction is the whole check. The
# compiler drops an import nothing calls, so a part that hand-rolled its password
# hashing has no `auth:identity` import here however many `use` lines it left
# behind. The COMPOSED component cannot answer this question at all: plugging
# `auth-guard` SATISFIES the import, which removes it just as thoroughly as never
# calling it did — both read as absent, and the check would fail every candidate
# including a correct one.
have() { wasm-tools component wit "$OUT/clinic_domain.wasm" | grep -q "$1"; }
for want in "auth:identity/accounts" "auth:identity/session" "search:index/index"; do
  have "$want" || {
    echo "FAILED: the component never calls $want — that capability is in \
the world for this part to USE, and reimplementing it is the one thing this part \
must not do (see CONTRACT.md)"; exit 1; }
done

LOG="$(mktemp -t clinic-log-XXXX)"
PORT=$(( 20000 + RANDOM % 20000 ))
cleanup() { [ -n "${HOST_PID:-}" ] && kill "$HOST_PID" 2>/dev/null; rm -f "$LOG"; }
trap cleanup EXIT

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
post() { curl -s -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
pcode() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
# One helper rather than `[ "$(pcode … "{\"json\":…}")" = 409 ]`: that nest of
# quotes made bash answer `[: too many arguments`, which read in the run's report
# as the CANDIDATE failing. Three generations of a real model were judged against
# a shell bug once already.
expect_post() { # expect_post <code> <path> <body> <message>
  local want="$1" path="$2" body="$3" msg="$4" got
  got=$(pcode "$path" "$body")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}
field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" 2>/dev/null; }

# --- pets to search, from the scaffold's fixture ------------------------------
#
# `access-and-search` cannot create a pet: pets belong to another half, and this
# gate has to pass while `src/owners.rs` is still a stub.
curl -s -X POST "$B/test/seed" >/dev/null || fail "the seed fixture did not answer"

# --- accounts ------------------------------------------------------------------
expect_post 400 /api/staff '{"email":"vet@clinic.test","password":"short"}' \
  "a password under 8 characters is a 400"
STAFF=$(post /api/staff '{"email":"vet@clinic.test","password":"correct-horse"}' | field id)
[ -n "$STAFF" ] || fail "POST /api/staff returned no id"
expect_post 409 /api/staff '{"email":"vet@clinic.test","password":"correct-horse"}' \
  "registering an email twice is a 409"

expect_post 401 /api/staff/login '{"email":"vet@clinic.test","password":"wrong"}' \
  "a wrong password is a 401"
expect_post 401 /api/staff/login '{"email":"nobody@clinic.test","password":"correct-horse"}' \
  "an unknown email is a 401, the same answer as a wrong password"
TOKEN=$(post /api/staff/login '{"email":"vet@clinic.test","password":"correct-horse"}' | field token)
[ -n "$TOKEN" ] || fail "a correct login returned no token"

# --- search, and the token that guards it --------------------------------------
CODE=$(curl -s -o /dev/null -w '%{http_code}' "$B/api/pets/search?q=cat")
[ "$CODE" = "401" ] || fail "search without a token is a 401 (got $CODE)"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer not-a-real-token" "$B/api/pets/search?q=cat")
[ "$CODE" = "401" ] || fail "search with a made-up token is a 401 (got $CODE)"

AUTH=(-H "Authorization: Bearer $TOKEN")
CODE=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" "$B/api/pets/search?q=")
[ "$CODE" = "400" ] || fail "an empty q is a 400 (got $CODE)"

HITS=$(curl -s "${AUTH[@]}" "$B/api/pets/search?q=Marbles")
echo "$HITS" | grep -q "Marbles" || fail "searching a pet's name does not find it: $HITS"
echo "$HITS" | grep -q "Biscuit" && fail "searching 'Marbles' returned an unrelated pet: $HITS"
curl -s "${AUTH[@]}" "$B/api/pets/search?q=dog" | grep -q "Biscuit" \
  || fail "searching by species does not find the dog"

# Ranked, best first: the pet whose NAME is the query outranks one that merely
# shares a species with it.
ORDER=$(curl -s "${AUTH[@]}" "$B/api/pets/search?q=Marbles%20cat")
python3 - "$ORDER" <<'PY' || fail "results are not ranked best-first: $ORDER"
import json, sys
pets = json.loads(sys.argv[1]).get("pets", [])
names = [p.get("name") for p in pets]
sys.exit(0 if names and names[0] == "Marbles" else 1)
PY

echo "clinic: staff access and pet search: passed"
