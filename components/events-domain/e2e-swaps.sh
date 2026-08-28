#!/usr/bin/env bash
# swaps: a ticket changes hands, and the house is no fuller than it was.
#
# The assertion that decides this part is the last one: `remaining` before a swap and
# after it must be identical. A swap moves a ticket between holders — it is not a
# release and a re-claim, and a part that models it that way frees a place to the
# public in the gap and passes every test that looks only at who holds what.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

trap gate_cleanup EXIT
gate_serve
events_seed

T=$(apost "$ATTENDEE" "/api/events/$EVENT_ID/tickets" '{}')
TID=$(printf '%s' "$T" | field id)
[ -n "$TID" ] || { echo "SKIP-CAUSE: the tickets part answered: $T"; fail "cannot judge swaps without a ticket"; }

BEFORE=$(aget "$ORGANIZER" "/api/events/$EVENT_ID" | field remaining)

expect_unauthenticated POST /api/swaps "{\"ticket_id\":\"$TID\"}"

# --- only the holder may offer -------------------------------------------------
aexpect_post "$OTHER" 403 /api/swaps "{\"ticket_id\":\"$TID\"}" \
  "a swap may only be offered by the ticket's holder"

S=$(apost "$ATTENDEE" /api/swaps "{\"ticket_id\":\"$TID\"}")
SID=$(printf '%s' "$S" | field id)
[ -n "$SID" ] || fail "offering a swap returned no id: $S"

aexpect_post "$ATTENDEE" 409 /api/swaps "{\"ticket_id\":\"$TID\"}" \
  "the same ticket may not carry two open offers"

LIST=$(aget "$OTHER" /api/swaps)
case "$LIST" in *"$SID"*) ;; *) fail "GET /api/swaps must list the offered swap: $LIST" ;; esac

# --- you may not accept your own -------------------------------------------------
aexpect_post "$ATTENDEE" 403 "/api/swaps/$SID/accept" '{}' \
  "the person who offered a swap may not accept it"

# --- somebody else takes it --------------------------------------------------------
OK=$(apcode "$OTHER" "/api/swaps/$SID/accept" '{}')
[ "$OK" = "200" ] || fail "another attendee must be able to accept the offer (got $OK)"

DOC=$(stored tickets "$TID")
OTHER_SUBJECT=$(post /test/seed '{}' | python3 -c "import sys,json;print(json.load(sys.stdin)['tokens']['other']['subject'])" 2>/dev/null)
case "$DOC" in *"$OTHER_SUBJECT"*) ;; *) fail "after an accepted swap the ticket's holder is the acceptor: $DOC" ;; esac

SDOC=$(stored swaps "$SID")
case "$SDOC" in *'"state":"accepted"'*|*'"state": "accepted"'*) ;; *) fail "the swap must become accepted: $SDOC" ;; esac

aexpect_post "$OTHER" 409 "/api/swaps/$SID/accept" '{}' "an accepted swap cannot be accepted again"

# --- the house is no fuller ----------------------------------------------------------
AFTER=$(aget "$ORGANIZER" "/api/events/$EVENT_ID" | field remaining)
[ "$BEFORE" = "$AFTER" ] || fail "a swap changed remaining from $BEFORE to $AFTER — a swap moves a ticket, it does not release and re-claim a place (see CONTRACT.md)"

echo "PASSED: swaps — offered, refused to its owner, accepted by another, and capacity unmoved ($AFTER remaining)"
