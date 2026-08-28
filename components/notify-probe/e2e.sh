#!/usr/bin/env bash
# notify: one call, two channels, and a real email in a real mailbox.
#
# The assertion that matters is the last kind: MailHog is asked what it actually
# HOLDS. "the send returned 2xx" passes against a relay that swallows everything,
# the same way a length check passes on an image that has been through
# `from_utf8_lossy`. Assert the artifact, not the status code.
#
# And the negative: a subject who opted out of email must produce NO message. A
# notification system that cannot be turned off is a mailing list.
set -uo pipefail
# shellcheck source=components/notify-probe/gate-lib.sh
. components/notify-probe/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_component_requires notify-prefs "notify:inbox/inbox" \
  "the in-app channel is a capability this component CALLS — a fan-out that stored its own messages would be a second inbox nothing else can read"
gate_component_requires notify-prefs "mail:send/sender" \
  "email leaves through mail:send, whose backend is a deploy-time choice — Resend or the local relay — and not something this component decides"

notify_start_mail
trap 'notify_stop_mail; gate_cleanup' EXIT
gate_serve

RUN="run$$"
ADA="ada-$RUN"
BOB="bob-$RUN"

# --- a subject nobody has configured -----------------------------------------------
#
# Not an error, and in-app only: the setting that cannot deliver anything anywhere
# it should not is the right default for one nobody chose.
DEF=$(get "/prefs?subject=$ADA")
case "$DEF" in
  *'"in-app"'*) ;;
  *) fail "an unconfigured subject must default to in-app: $DEF" ;;
esac
case "$DEF" in
  *'"email"'*) fail "an unconfigured subject must NOT default to email: $DEF" ;;
esac

# --- opt in to both -----------------------------------------------------------------
PUT=$(curl -s -X PUT -H 'content-type: application/json' \
  -d "{\"subject\":\"$ADA\",\"default_channels\":[\"in-app\",\"email\"],\"email_address\":\"$ADA@example.test\",\"overrides\":{}}" \
  "$B/prefs")
case "$PUT" in *'"ok":true'*) ;; *) fail "could not set preferences: $PUT" ;; esac

MARKER="marker-$RUN-both"
OUT=$(post /notify "{\"subject\":\"$ADA\",\"kind\":\"ticket-swapped\",\"title\":\"Your ticket was swapped\",\"body\":\"$MARKER\",\"payload\":\"tkt_1\"}")
[ "$(outcome_ok "$OUT" in-app)" = "yes" ] || fail "the in-app channel did not deliver: $OUT"
[ "$(outcome_ok "$OUT" email)" = "yes" ] || fail "the email channel did not deliver: $OUT"

# --- it is IN the inbox --------------------------------------------------------------
BOX=$(get "/inbox?subject=$ADA&after=0&limit=10")
case "$BOX" in *"$MARKER"*) ;; *) fail "the note is not in the inbox: $BOX" ;; esac
case "$BOX" in *'"kind":"ticket-swapped"'*) ;; *) fail "the note lost its kind: $BOX" ;; esac
UNREAD=$(get "/unread?subject=$ADA" | field unread)
[ "$UNREAD" = "1" ] || fail "one delivered note is one unread, not '$UNREAD'"

# --- and a REAL email is in a REAL mailbox --------------------------------------------
#
# Delivered over genuine SMTP by comp-mailrelay, which is the only thing in this
# picture that is not the component under test.
sleep 0.5
FOUND=$(mail_find "$MARKER")
[ -n "$FOUND" ] || fail "MailHog holds no message containing $MARKER — the send reported success and nothing arrived: $(mailbox | head -c 300)"
TO=${FOUND%%|*}; REST=${FOUND#*|}; SUBJ=${REST%%|*}
[ "$TO" = "$ADA@example.test" ] || fail "the email went to '$TO', not the address in the preference"
[ "$SUBJ" = "Your ticket was swapped" ] || fail "the subject line was '$SUBJ'"

# --- reading it ------------------------------------------------------------------------
SEQ=$(printf '%s' "$BOX" | python3 -c "import sys,json;print(json.load(sys.stdin)['notes'][0]['seq'])")
MARKED=$(post /read "{\"subject\":\"$ADA\",\"seqs\":[$SEQ]}" | field marked)
[ "$MARKED" = "1" ] || fail "marking one note read reported '$MARKED'"
[ "$(get "/unread?subject=$ADA" | field unread)" = "0" ] || fail "after reading the only note the badge must be 0"
# Twice must not drive it negative — a client that retries is not a client that
# should make the badge wrong.
post /read "{\"subject\":\"$ADA\",\"seqs\":[$SEQ]}" >/dev/null
[ "$(get "/unread?subject=$ADA" | field unread)" = "0" ] || fail "marking the same note read twice moved the badge"

# --- the cursor is a cursor --------------------------------------------------------------
post /notify "{\"subject\":\"$ADA\",\"kind\":\"event-cancelled\",\"title\":\"Cancelled\",\"body\":\"second-$RUN\",\"payload\":\"\"}" >/dev/null
TAIL=$(get "/inbox?subject=$ADA&after=$SEQ&limit=10")
case "$TAIL" in *"second-$RUN"*) ;; *) fail "after=$SEQ must return what came next: $TAIL" ;; esac
case "$TAIL" in *"$MARKER"*) fail "after=$SEQ must NOT return what came before it: $TAIL" ;; esac

# --- opting out of email is REAL ----------------------------------------------------------
BEFORE=$(mail_count_containing "quiet-$RUN")
curl -s -X PUT -H 'content-type: application/json' \
  -d "{\"subject\":\"$BOB\",\"default_channels\":[\"in-app\"],\"email_address\":\"$BOB@example.test\",\"overrides\":{}}" \
  "$B/prefs" >/dev/null
OUT=$(post /notify "{\"subject\":\"$BOB\",\"kind\":\"ticket-swapped\",\"title\":\"Quiet\",\"body\":\"quiet-$RUN\",\"payload\":\"\"}")
case ",$(outcome_channels "$OUT")," in
  *,email,*) fail "a subject who did not ask for email must get no email OUTCOME at all: $OUT" ;;
esac
sleep 0.5
AFTER=$(mail_count_containing "quiet-$RUN")
[ "$AFTER" = "$BEFORE" ] || fail "an opted-out subject received $AFTER email(s) — opting out has to be real, not cosmetic"
case "$(get "/inbox?subject=$BOB&after=0&limit=5")" in
  *"quiet-$RUN"*) ;;
  *) fail "opting out of email must not cost the in-app copy" ;;
esac

# --- muting ONE kind ------------------------------------------------------------------------
#
# An empty override is a real answer and means "not this one". Falling back to the
# defaults on an empty list would make muting a single kind impossible.
curl -s -X PUT -H 'content-type: application/json' \
  -d "{\"subject\":\"$ADA\",\"default_channels\":[\"in-app\",\"email\"],\"email_address\":\"$ADA@example.test\",\"overrides\":{\"noisy\":[]}}" \
  "$B/prefs" >/dev/null
OUT=$(post /notify "{\"subject\":\"$ADA\",\"kind\":\"noisy\",\"title\":\"Muted\",\"body\":\"muted-$RUN\",\"payload\":\"\"}")
[ -z "$(outcome_channels "$OUT")" ] || fail "a kind muted with an empty override must attempt NOTHING, but tried: $(outcome_channels "$OUT")"
case "$(get "/inbox?subject=$ADA&after=0&limit=20")" in
  *"muted-$RUN"*) fail "a muted kind reached the inbox anyway" ;;
esac
# ...and the same subject still gets the kinds they did not mute.
OUT=$(post /notify "{\"subject\":\"$ADA\",\"kind\":\"ticket-swapped\",\"title\":\"Still on\",\"body\":\"still-$RUN\",\"payload\":\"\"}")
[ "$(outcome_ok "$OUT" email)" = "yes" ] || fail "muting one kind muted the others: $OUT"

echo "PASSED: notify — one call reached an inbox and a real mailbox, opting out was real, and one kind was muted without silencing the rest"
