#!/usr/bin/env bash
# The whole ticketing API, driven the way a real evening goes.
#
# The gate no single part can pass. Each of the four has its own, and each is written
# so a part that invented its own storage shape still passes it — that is what a
# per-part gate can see. This one drives ONE ticket through all four: events makes
# the event, tickets issues the place, swaps moves it to somebody else, and checkin
# admits the person who ended up holding it. A part that agreed with the contract
# only in its own file fails here and nowhere else.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

trap gate_cleanup EXIT
gate_serve
events_seed

# --- an organizer opens the doors -------------------------------------------------
E=$(apost "$ORGANIZER" /api/events '{"title":"The Whole Evening","starts_at":"2026-11-05T19:00:00Z","capacity":2}')
EID=$(printf '%s' "$E" | field id)
[ -n "$EID" ] || fail "events: no event was created: $E"

# --- an attendee takes a place ------------------------------------------------------
T=$(apost "$ATTENDEE" "/api/events/$EID/tickets" '{}')
TID=$(printf '%s' "$T" | field id)
[ -n "$TID" ] || fail "tickets: no ticket was issued: $T"

AFTER_CLAIM=$(aget "$ORGANIZER" "/api/events/$EID" | field remaining)
[ "$AFTER_CLAIM" = "1" ] || fail "capacity 2 minus one claim is 1 remaining, not '$AFTER_CLAIM' — events reads this from quota:meter, which tickets must be the thing that moved"

# --- they cannot come, and pass it on -------------------------------------------------
S=$(apost "$ATTENDEE" /api/swaps "{\"ticket_id\":\"$TID\"}")
SID=$(printf '%s' "$S" | field id)
[ -n "$SID" ] || fail "swaps: no offer was created: $S"
OK=$(apcode "$OTHER" "/api/swaps/$SID/accept" '{}')
[ "$OK" = "200" ] || fail "swaps: the offer could not be accepted (got $OK)"

STILL=$(aget "$ORGANIZER" "/api/events/$EID" | field remaining)
[ "$STILL" = "1" ] || fail "a swap moved remaining from 1 to '$STILL' — the ticket changed hands, the house did not empty a seat"

# --- the new holder is the one who gets in ---------------------------------------------
CODE=$(aget "$OTHER" "/api/tickets/$TID" | field code)
[ -n "$CODE" ] || fail "the acceptor cannot read the ticket they now hold — tickets and swaps disagree about who the holder is"

IN=$(apost "$ORGANIZER" /api/checkin "{\"code\":\"$CODE\"}")
case "$IN" in *'checked-in'*) ;; *) fail "checkin: the swapped ticket was not admitted: $IN" ;; esac

HOLDER=$(printf '%s' "$IN" | field holder)
OTHER_SUBJECT=$(post /test/seed '{}' | python3 -c "import sys,json;print(json.load(sys.stdin)['tokens']['other']['subject'])" 2>/dev/null)
[ "$HOLDER" = "$OTHER_SUBJECT" ] || fail "the door admitted '$HOLDER' but the ticket belongs to '$OTHER_SUBJECT' after the swap — checkin is reading a holder that swaps already changed"

# --- and the person who gave it away cannot get in ---------------------------------------
aexpect_post "$ORGANIZER" 409 /api/checkin "{\"code\":\"$CODE\"}" \
  "the ticket is spent — a second scan is refused whoever presents it"

echo "PASSED: the whole evening — event opened, place taken, ticket passed on, and the right person walked in"
