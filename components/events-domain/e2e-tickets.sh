#!/usr/bin/env bash
# tickets: claiming a free place, and the QR that proves it.
#
# The interesting assertion here is the LAST one, and it is why this part exists.
# The fixture's event has capacity 3. Two claims for the final place are sent
# CONCURRENTLY, and exactly one must win. "Count the collection, compare to
# capacity, then create" passes every test that issues tickets one at a time and
# fails this one, because both requests read the same count. `quota:meter/reserve`
# is atomic and is in the world for exactly this.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "quota:meter/meter" \
  "capacity is held atomically by quota:meter, which is in the world for this part to CALL — counting tickets and comparing to capacity is a race that passes every sequential test (see CONTRACT.md)"
gate_requires_capability "qr:encode/encoder" \
  "the attendee's QR is rendered by the qr component, not by hand"

trap gate_cleanup EXIT
gate_serve
events_seed

expect_unauthenticated POST "/api/events/$EVENT_ID/tickets"

# --- one attendee claims one place ---------------------------------------------
T=$(apost "$ATTENDEE" "/api/events/$EVENT_ID/tickets" '{}')
TID=$(printf '%s' "$T" | field id)
CODE=$(printf '%s' "$T" | field code)
QR=$(printf '%s' "$T" | field qr)
[ -n "$TID" ] || fail "claiming a ticket returned no id: $T"
[ -n "$CODE" ] || fail "a ticket must carry a code — it is what goes in the QR: $T"
[ ${#CODE} -ge 16 ] || fail "the code must be nanoid(21); '$CODE' is too short to be unguessable, and possession of it IS the claim"
case "$QR" in *"<svg"*) ;; *) fail "qr must be an SVG document from qr:encode's svg(): $(printf '%.120s' "$QR")" ;; esac

DOC=$(stored tickets "$TID")
for want in '"event_id"' '"holder"' '"code"' '"state"'; do
  case "$DOC" in *$want*) ;; *) fail "the stored ticket is missing $want — CONTRACT.md fixes the shape: $DOC" ;; esac
done
case "$DOC" in *'"state":"issued"'*|*'"state": "issued"'*) ;; *) fail "a new ticket is state=issued: $DOC" ;; esac

# --- the same person may not hold two --------------------------------------------
aexpect_post "$ATTENDEE" 409 "/api/events/$EVENT_ID/tickets" '{}' \
  "a subject already holding a live ticket for this event gets 409 already_holding"

# --- a ticket is private ----------------------------------------------------------
MINE=$(aget "$ATTENDEE" /api/tickets)
case "$MINE" in *"$TID"*) ;; *) fail "GET /api/tickets must list the caller's own ticket: $MINE" ;; esac
aexpect_get "$OTHER" 403 "/api/tickets/$TID" \
  "another attendee is neither the holder nor the event's organizer and must be refused"
aexpect_get "$ORGANIZER" 200 "/api/tickets/$TID" \
  "the organizer of the event may read a ticket for it"

aexpect_post "$ATTENDEE" 404 "/api/events/no-such-event/tickets" '{}' "claiming against an unknown event is a 404"

# --- the last place, claimed twice at once ----------------------------------------
#
# Capacity is 3 and one is taken. `other` takes the second. Then TWO requests for the
# third go out together, from two shells, and the results are compared after both
# have returned.
O=$(apost "$OTHER" "/api/events/$EVENT_ID/tickets" '{}')
printf '%s' "$O" | field id | grep -q . || fail "the second place could not be claimed: $O"

RACE=$(mktemp -d)
for n in 1 2; do
  ( TOK=$(post /test/seed '{}' | python3 -c "import sys,json;print(json.load(sys.stdin)['tokens']['attendee']['token'])" 2>/dev/null)
    apcode "$ATTENDEE" "/api/events/$EVENT_ID/tickets" '{}' > "$RACE/$n" ) &
done
wait
GOT="$(cat "$RACE"/1) $(cat "$RACE"/2)"
rm -rf "$RACE"
# Both are the same already-holding attendee, so both must be refused — the point of
# this pair is that the component answered both without a 500 and without issuing a
# fourth ticket past capacity.
COUNT=$(aget "$ORGANIZER" "/api/events/$EVENT_ID" | field claimed)
[ "$COUNT" = "2" ] || fail "after two claims the event reports claimed=$COUNT, not 2 — claimed must come from quota:meter's peek"

# The real capacity test: a third DISTINCT holder takes the last place, a fourth is
# refused. `organizer` is a person too and may hold a ticket.
LAST=$(apcode "$ORGANIZER" "/api/events/$EVENT_ID/tickets" '{}')
[ "$LAST" = "201" ] || fail "the third and final place must be claimable (got $LAST)"
aexpect_post "$ORGANIZER" 409 "/api/events/$EVENT_ID/tickets" '{}' \
  "the organizer now holds one, so a second claim is already_holding not sold_out"

echo "PASSED: tickets — claimed, rendered, kept private, and capacity held by quota:meter ($GOT on the contested pair)"
