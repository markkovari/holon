#!/usr/bin/env bash
# checkin: the organizer scans a code at the door.
#
# The part is small and one thing about it is not: checking the same ticket in twice
# must be refused WITH the state the machine reports. `fsm:workflow` returns
# IllegalTransition carrying the current state, which is exactly the 409 body — a
# part that keeps its own `if state == "checked-in"` ladder passes the happy path
# and drifts from the machine the moment anything else moves a ticket.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "fsm:workflow/engine" \
  "the ticket lifecycle is a DEFINITION registered with fsm-workflow, not a ladder of string comparisons — and the refusal to check a ticket in twice comes from the machine, carrying the current state (see CONTRACT.md)"

trap gate_cleanup EXIT
gate_serve
events_seed

# A ticket to scan. `tickets` owns claiming and may still be a stub, so the fixture's
# event is used through the same route and the gate fails clearly if it is not there
# yet rather than blaming this part for it.
T=$(apost "$ATTENDEE" "/api/events/$EVENT_ID/tickets" '{}')
CODE=$(printf '%s' "$T" | field code)
TID=$(printf '%s' "$T" | field id)
[ -n "$CODE" ] || { echo "SKIP-CAUSE: no ticket to scan — the tickets part answered: $T"; fail "cannot judge check-in without a ticket"; }

expect_unauthenticated POST /api/checkin "{\"code\":\"$CODE\"}"

# --- an attendee may not scan ----------------------------------------------------
aexpect_post "$ATTENDEE" 403 /api/checkin "{\"code\":\"$CODE\"}" \
  "an attendee has no checkin:write"

# --- the organizer scans ----------------------------------------------------------
IN=$(apost "$ORGANIZER" /api/checkin "{\"code\":\"$CODE\"}")
for want in '"ticket_id"' '"event_id"' '"holder"' '"state"'; do
  case "$IN" in *$want*) ;; *) fail "the check-in reply is missing $want: $IN" ;; esac
done
case "$IN" in *'checked-in'*) ;; *) fail "after a scan the state is checked-in: $IN" ;; esac

# The document must move too, or GET /api/tickets/{id} disagrees with the machine.
DOC=$(stored tickets "$TID")
case "$DOC" in *'checked-in'*) ;; *) fail "the ticket DOCUMENT still does not say checked-in — move both (see CONTRACT.md): $DOC" ;; esac

# --- twice is a 409 that names the state -------------------------------------------
AGAIN=$(apost "$ORGANIZER" /api/checkin "{\"code\":\"$CODE\"}")
case "$AGAIN" in
  *'already_checked_in'*) ;;
  *) fail "a second scan must be 409 already_checked_in: $AGAIN" ;;
esac
case "$AGAIN" in
  *'checked-in'*) ;;
  *) fail "the 409 must carry the CURRENT state, which fsm's IllegalTransition already gives you: $AGAIN" ;;
esac
aexpect_post "$ORGANIZER" 409 /api/checkin "{\"code\":\"$CODE\"}" "a repeat scan is 409"

# --- an unknown code ----------------------------------------------------------------
aexpect_post "$ORGANIZER" 404 /api/checkin '{"code":"not-a-real-code-at-all"}' \
  "an unknown code is 404 no_such_ticket, not 500 and not 200"

echo "PASSED: checkin — scanned once, refused twice with the machine's own state, and closed to attendees"
