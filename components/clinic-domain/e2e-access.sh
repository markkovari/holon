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
# shellcheck source=components/clinic-domain/gate-lib.sh
. components/clinic-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "auth:identity/accounts" \
  "that capability is in the world for this part to USE, and reimplementing it is the one thing this part must not do (see CONTRACT.md)"
gate_requires_capability "auth:identity/session" \
  "sessions are auth-guard's job; do not invent a token format (see CONTRACT.md)"
gate_requires_capability "search:index/index" \
  "ranked search already exists in this repository; a substring scan is not it (see CONTRACT.md)"

trap gate_cleanup EXIT
gate_serve

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
