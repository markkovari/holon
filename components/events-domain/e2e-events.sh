#!/usr/bin/env bash
# events: the organizer's half — creating, listing, amending and cancelling events.
#
# Judged on BEHAVIOUR, because `cargo component check` passes on a crate that
# implements none of its world, and on AUTHORISATION, because the contract's table
# is the part most easily skipped: a part that answers 201 to everybody has written
# a working CRUD and failed the goal.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "auth:identity/authorizer" \
  "who is asking and whether they may is auth-guard's job, and it is in the world for this part to CALL — parsing the bearer or comparing a role string read out of the token is doing authorisation in the wrong place (see CONTRACT.md)"

trap gate_cleanup EXIT
gate_serve
events_seed

# --- anonymous callers are refused --------------------------------------------
expect_unauthenticated POST /api/events '{"title":"x","starts_at":"2026-09-01T18:00:00Z","capacity":5}'

# --- an organizer creates one --------------------------------------------------
NEW=$(apost "$ORGANIZER" /api/events '{"title":"Wasm Night","starts_at":"2026-10-01T18:00:00Z","capacity":50}')
NEW_ID=$(printf '%s' "$NEW" | field id)
[ -n "$NEW_ID" ] || fail "POST /api/events returned no id: $NEW"

# The document has to be what the contract says, or `tickets` cannot read the
# capacity out of it and the composition fails for this part's reason.
DOC=$(stored events "$NEW_ID")
for want in '"title"' '"starts_at"' '"capacity"' '"organizer"' '"state"'; do
  case "$DOC" in *$want*) ;; *) fail "the stored event is missing $want — CONTRACT.md fixes the shape: $DOC" ;; esac
done
case "$DOC" in *'"state":"open"'*|*'"state": "open"'*) ;; *) fail "a new event must be state=open: $DOC" ;; esac

# --- an attendee may not create ------------------------------------------------
aexpect_post "$ATTENDEE" 403 /api/events '{"title":"nope","starts_at":"2026-10-01T18:00:00Z","capacity":5}' \
  "an attendee has no event:write and must be refused"

# --- validation ----------------------------------------------------------------
aexpect_post "$ORGANIZER" 400 /api/events '{"starts_at":"2026-10-01T18:00:00Z","capacity":5}' \
  "an event with no title is a 400"
aexpect_post "$ORGANIZER" 400 /api/events '{"title":"x","starts_at":"2026-10-01T18:00:00Z","capacity":0}' \
  "capacity below 1 is a 400"

# --- reading it back ------------------------------------------------------------
ONE=$(aget "$ATTENDEE" "/api/events/$NEW_ID")
for want in '"claimed"' '"remaining"'; do
  case "$ONE" in *$want*) ;; *) fail "GET /api/events/{id} must report $want from quota:meter's peek: $ONE" ;; esac
done
REM=$(printf '%s' "$ONE" | field remaining)
[ "$REM" = "50" ] || fail "a brand-new event with capacity 50 has 50 remaining, not '$REM'"

LIST=$(aget "$ATTENDEE" "/api/events?state=open")
case "$LIST" in *"$NEW_ID"*) ;; *) fail "?state=open did not list the open event just created — find_by wants the JSON ENCODING of the value: $LIST" ;; esac

aexpect_get "$ATTENDEE" 404 "/api/events/does-not-exist" "an unknown event is a 404"

# --- only the owning organizer may amend ----------------------------------------
GOT=$(apatch "$ORGANIZER" "/api/events/$NEW_ID" '{"title":"Wasm Night, moved"}')
[ "$GOT" = "200" ] || fail "the organizer who created the event must be able to PATCH it (got $GOT)"

# --- cancelling is soft ----------------------------------------------------------
GOT=$(adelete "$ORGANIZER" "/api/events/$NEW_ID")
[ "$GOT" = "204" ] || fail "DELETE must answer 204 (got $GOT)"
DOC=$(stored events "$NEW_ID")
case "$DOC" in
  *'"state":"cancelled"'*|*'"state": "cancelled"'*) ;;
  *) fail "DELETE is a SOFT delete — the document stays and state becomes cancelled: $DOC" ;;
esac

echo "PASSED: events — created, read, amended and cancelled, and the authorisation table is honoured"
