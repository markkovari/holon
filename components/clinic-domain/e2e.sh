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
# shellcheck source=components/clinic-domain/gate-lib.sh
. components/clinic-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

trap gate_cleanup EXIT
gate_serve

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

# --- staff access, and search behind it ----------------------------------------
#
# The JOIN, which is what this gate is for: the four parts share one component and
# one store, so a report has to see visits the visits half wrote, and a search has
# to find a pet the owners half created. Each half's own gate can only prove its
# half; nothing but this proves they add up to a clinic.
expect_post 201 /api/staff '{"email":"vet@clinic.test","password":"correct-horse"}' \
  "a staff account is created"
TOKEN=$(post /api/staff/login '{"email":"vet@clinic.test","password":"correct-horse"}' | field token)
[ -n "$TOKEN" ] || fail "a correct login returned no token"
expect_get 401 "$B/api/pets/search?q=Rex" "search without a token is a 401"
curl -s -H "Authorization: Bearer $TOKEN" "$B/api/pets/search?q=Rex" | grep -q "$PET" \
  || fail "search does not find the pet the owners half created"

# --- reports over what the visits half booked -----------------------------------
CSV=$(curl -s "$B/api/reports/visits.csv?day=2026-09-01")
echo "$CSV" | head -1 | grep -q '^id,pet_id,pet_name,vet,start,minutes' \
  || fail "the CSV report has no header: $CSV"
echo "$CSV" | grep -q "vet-b" || fail "the report does not show the visits that were booked: $CSV"
SUM=$(curl -s "$B/api/reports/summary?day=2026-09-01")
echo "$SUM" | grep -q '"by_species"' || fail "the summary has no by_species: $SUM"
python3 - "$SUM" <<'CHECK' || fail "the summary does not count what the visits half booked: $SUM"
import json, sys
s = json.loads(sys.argv[1])
# Three visits survive the day: two for vet-a (one rebooked after a delete) and
# one for vet-b.
assert s["visits"] == 3, s
assert s["by_vet"].get("vet-b") == 1, s
CHECK

expect_get 404 "$B/api/nope" "an unknown route is a 404"
echo "clinic: every route behaved"
