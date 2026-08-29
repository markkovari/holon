#!/usr/bin/env bash
# The 24-hour reminder, and the notification stack behind it.
#
# The reminder is scheduled when the event is created, for `starts_at` minus 24
# hours. That is a time in the future, so the test does not wait for it — it makes
# an event that starts SOON, which puts its reminder in the past and therefore due.
# The schedule is real either way; only the clock is chosen.
#
# What this proves that the notify gate cannot: the whole thing composed. An event,
# a ticket, a preference, a timer, a fan-out, an inbox and a real mailbox, in the
# order a real evening happens in.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

gate_require_tools
gate_build
events_start_mail
gate_compose

gate_component_requires events-domain "sched:timer/timer" \
  "a reminder 24 hours from now cannot be waited for by a request handler, and working it out on every read sends nothing if nobody loads the page and twice if two people do"
gate_component_requires events-domain "notify:prefs/preferences" \
  "whether a reminder is an email or an in-app note is the PERSON's answer, read from what they set — an app that sent its own email would re-decide it for everyone"

trap 'events_stop_mail; gate_cleanup' EXIT
gate_serve
events_seed

RUN="r$$"

# --- an event far away schedules a reminder for later ------------------------------
FAR=$(apost "$ORGANIZER" /api/events '{"title":"Far Away","starts_at":"2027-01-01T18:00:00Z","capacity":10}')
FID=$(printf '%s' "$FAR" | field id)
PEEK=$(aget "$ORGANIZER" "/api/events/$FID/reminder")
case "$PEEK" in *'"scheduled":true'*) ;; *) fail "creating an event must put its reminder on the clock: $PEEK" ;; esac
DUE_IN=$(printf '%s' "$PEEK" | python3 -c "import sys,json;print(json.load(sys.stdin)['due_in_seconds'])")
[ "$DUE_IN" -gt 0 ] || fail "an event in 2027 must have a reminder in the FUTURE, not $DUE_IN seconds ago"

# Nothing fires for it.
FIRED=$(apost "$ORGANIZER" /api/reminders/run '{}' | field fired)
[ "$FIRED" = "0" ] || fail "a reminder that is not due yet must not fire (fired $FIRED)"

# --- an event SOON has a reminder that is already due --------------------------------
#
# `date -v` is BSD and `date -d` is GNU; both are tried so this runs on either.
SOON=$(date -u -v+2H '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || date -u -d '+2 hours' '+%Y-%m-%dT%H:%M:%SZ')
EV=$(apost "$ORGANIZER" /api/events "{\"title\":\"Tonight $RUN\",\"starts_at\":\"$SOON\",\"capacity\":10}")
EID=$(printf '%s' "$EV" | field id)
[ -n "$EID" ] || fail "could not create the soon event: $EV"

# --- somebody holds a ticket, and wants both channels ----------------------------------
T=$(apost "$ATTENDEE" "/api/events/$EID/tickets" '{}')
[ -n "$(printf '%s' "$T" | field id)" ] || fail "no ticket: $T"

PUT=$(curl -s -X PUT -H 'content-type: application/json' -H "authorization: Bearer $ATTENDEE" \
  -d "{\"default_channels\":[\"in-app\",\"email\"],\"email_address\":\"ada-$RUN@example.test\",\"overrides\":{}}" \
  "$B/api/prefs")
case "$PUT" in *'"ok":true'*) ;; *) fail "could not set preferences: $PUT" ;; esac

BEFORE_MAIL=$(mail_count_containing "Tonight $RUN")
BEFORE_UNREAD=$(aget "$ATTENDEE" /api/notifications/unread | field unread)

# --- the clock ticks -------------------------------------------------------------------
RUN_OUT=$(apost "$ORGANIZER" /api/reminders/run '{}')
FIRED=$(printf '%s' "$RUN_OUT" | field fired)
[ "$FIRED" = "1" ] || fail "exactly one reminder was due and $FIRED fired: $RUN_OUT"

# --- it reached the inbox ----------------------------------------------------------------
AFTER_UNREAD=$(aget "$ATTENDEE" /api/notifications/unread | field unread)
[ "$AFTER_UNREAD" -gt "$BEFORE_UNREAD" ] || fail "the badge did not move: $BEFORE_UNREAD -> $AFTER_UNREAD"
NOTES=$(aget "$ATTENDEE" /api/notifications)
case "$NOTES" in *'"kind":"event-reminder"'*) ;; *) fail "no event-reminder in the inbox: $NOTES" ;; esac
case "$NOTES" in *"Tonight $RUN"*) ;; *) fail "the reminder does not name the event: $NOTES" ;; esac

# --- and a REAL email arrived ---------------------------------------------------------------
sleep 0.5
AFTER_MAIL=$(mail_count_containing "Tonight $RUN")
[ "$AFTER_MAIL" -gt "$BEFORE_MAIL" ] || fail "MailHog holds no reminder for this event — the fan-out reported success and nothing arrived"

# --- firing twice does not remind twice -------------------------------------------------------
#
# `ack` is what makes that true. A reminder that repeats every time a scheduler
# ticks is worse than one that never comes.
AGAIN=$(apost "$ORGANIZER" /api/reminders/run '{}' | field fired)
[ "$AGAIN" = "0" ] || fail "an acked reminder fired again ($AGAIN) — it would repeat on every tick"

# --- cancelling the event cancels the reminder --------------------------------------------------
C=$(apost "$ORGANIZER" /api/events "{\"title\":\"Doomed $RUN\",\"starts_at\":\"$SOON\",\"capacity\":5}")
CID=$(printf '%s' "$C" | field id)
apost "$ATTENDEE" "/api/events/$CID/tickets" '{}' >/dev/null
adelete "$ORGANIZER" "/api/events/$CID" >/dev/null
case "$(aget "$ORGANIZER" "/api/events/$CID/reminder")" in
  *'"scheduled":false'*) ;;
  *) fail "cancelling an event must take its reminder off the clock — it must not still tell people to come" ;;
esac
# ...and the holder was told it was cancelled.
case "$(aget "$ATTENDEE" /api/notifications)" in
  *'"kind":"event-cancelled"'*) ;;
  *) fail "cancelling an event must tell the people holding tickets for it" ;;
esac

echo "PASSED: reminders — scheduled on creation, fired once when due, reached an inbox and a real mailbox, and cancelled with the event"
