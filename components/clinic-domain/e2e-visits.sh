#!/usr/bin/env bash
# clinic: the visits half
#
# Judges ONE half, against a pet the scaffold seeds — so it passes while
# `src/owners.rs` is still a stub.
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
# shellcheck source=components/clinic-domain/gate-lib.sh
. components/clinic-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

trap gate_cleanup EXIT
gate_serve

# --- a pet to book against, from the scaffold's fixture ----------------------
#
# `visits` cannot create a pet: pets belong to the other half, and this gate has to
# pass while `src/owners.rs` is still a stub. `POST /test/seed` is scaffold, not
# either part's code, and exists for exactly this.
SEED=$(curl -s -X POST "$B/test/seed")
PET=$(echo "$SEED" | python3 -c "import sys,json;print(json.load(sys.stdin).get('pet_id',''))" 2>/dev/null)
[ -n "$PET" ] || fail "the seed fixture gave no pet: $SEED"

# --- visits, and the rule a compiler cannot check -----------------------------
V1=$(post /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:00:00Z\",\"minutes\":30}" | field id)
[ -n "$V1" ] || fail "POST /api/visits returned no id"
expect_post 409 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:15:00Z\",\"minutes\":30}" "an overlapping visit for the same vet must be a 409"
expect_post 201 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:30:00Z\",\"minutes\":30}" "touching at the boundary is not an overlap"
expect_post 201 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-b\",\"start\":\"2026-09-01T09:15:00Z\",\"minutes\":30}" "a different vet at the same time is fine"
expect_post 400 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T11:00:00Z\",\"minutes\":45}" "45 minutes is not one of 15/30/60"
expect_post 404 /api/visits '{"pet_id":"nosuch","vet":"vet-a","start":"2026-09-01T14:00:00Z","minutes":30}' "a visit for an unknown pet is a 404"
curl -s "$B/api/visits?vet=vet-a&day=2026-09-01" | grep -q "$V1" || fail "the day's visits do not list"

DEL=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$B/api/visits/$V1")
[ "$DEL" = "204" ] || fail "DELETE of a visit is a 204 (got $DEL)"
expect_post 201 /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"2026-09-01T09:00:00Z\",\"minutes\":30}" "a deleted visit must free its slot"

expect_get 404 "$B/api/nope" "an unknown route is a 404"
echo "clinic: the visits half: passed"
