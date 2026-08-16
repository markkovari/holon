#!/usr/bin/env bash
# clinic: the owners-and-pets half
#
# Judges ONE half. The visits routes are not exercised, so this passes while
# `src/visits.rs` is still a stub — which is what lets the two halves be worked on
# at once without waiting for each other.
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

expect_get 404 "$B/api/nope" "an unknown route is a 404"
echo "clinic: the owners-and-pets half: passed"
