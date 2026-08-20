#!/usr/bin/env bash
# support:desk — the reply part
#
# One model call. What is checked besides the draft:
#
#   * IT MUST NOT SEND. The assertion is on the artifact's imports: a part that calls
#     `notify:dispatch` has thrown away at-least-once delivery, and it would pass every
#     request-shaped test in this file while doing so.
#   * 202, not 200. Nothing has been delivered when this route answers.
#   * CSRF first, before the budget and before the model — a request that did not come from
#     the page must cost nothing at all.
set -uo pipefail
# shellcheck source=components/support-desk-domain/gate-lib.sh
. components/support-desk-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

desk_requires_auth
gate_requires_capability "ai:inference/inference" \
  "the model is one interface away in this repository — not an HTTP client this part writes"
gate_requires_capability "quota:meter/meter" \
  "counting what a tenant spent in a period is a solved problem here — a counter in the record store is how this part fails"
gate_requires_capability "session:store/store" \
  "the CSRF token belongs to the session component that issued it — comparing strings here is not a check"
gate_requires_capability "outbox:dispatch/queue" \
  "a drafted reply is ENQUEUED; the outbox is what makes delivery survive a far end that is down"

# NOT an import check. The obvious way to assert "this part must not send" is to look for
# `notify:dispatch` in the artifact's imports — and it is wrong, because all three parts
# compile into ONE component: the check passes only while the courier part happens to be a
# stub, and fails the moment both are implemented, which is every composed build. An
# artifact-level import says what the COMPONENT calls, never which part called it.
#
# So the property is asserted where it is actually visible: a sink this gate runs, reachable
# (egress is granted to it), and a reply drafted with nothing arriving at it. A part that
# sends inline delivers to that sink and is caught; a part that enqueues cannot, because the
# courier is a stub here and nothing else moves the outbox.
GATE_CONFIG="--config reply-budget=1 --config reply-period-secs=3600"
sink_start
gate_shim_config
trap 'gate_cleanup; sink_stop' EXIT
gate_serve

T=$(post /test/token '{"subject":"agent","tenant":"acme"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
SESS=$(post /test/session '{}')
SID=$(printf '%s' "$SESS" | field session)
CSRF=$(printf '%s' "$SESS" | field csrf)
[ -n "$SID" ] && [ -n "$CSRF" ] || fail "the fixture could not open a session — the scaffold is broken, not the part"
IDS=$(post /test/seed "{\"target\":\"webhook:$SINK_URL\"}" \
  | python3 -c "import sys,json;[print(i) for i in json.load(sys.stdin).get('ticket_ids',[])]")
ONE=$(printf '%s' "$IDS" | sed -n 1p)
TWO=$(printf '%s' "$IDS" | sed -n 2p)
[ -n "$ONE" ] && [ -n "$TWO" ] || fail "the fixture produced no tickets — the scaffold is broken, not the part"

reply() { curl -s -X POST -H "$AUTH" -H "x-session: $SID" -H "x-csrf: $CSRF" "$B/api/tickets/$1/reply"; }
reply_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" -H "x-session: $SID" -H "x-csrf: $CSRF" "$B/api/tickets/$1/reply"; }

# --- CSRF comes first, and costs nothing ---------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" "$B/api/tickets/$ONE/reply")
[ "$GOT" = 403 ] || fail "a reply with no session or csrf header must be 403 csrf_required, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" -H "x-session: $SID" -H "x-csrf: wrong" "$B/api/tickets/$ONE/reply")
[ "$GOT" = 403 ] || fail "a reply with the wrong csrf token must be 403 csrf_invalid, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "authorization: Bearer $T" -H "x-session: nope" -H "x-csrf: $CSRF" "$B/api/tickets/$ONE/reply")
[ "$GOT" = 403 ] || fail "a reply against a session that does not exist must be 403 session_expired, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "x-session: $SID" -H "x-csrf: $CSRF" "$B/api/tickets/$ONE/reply")
[ "$GOT" = 401 ] || fail "no bearer at all is 401, before any csrf talk, got $GOT"
GOT=$(reply_code nope)
[ "$GOT" = 404 ] || fail "replying to an unknown ticket must be 404, got $GOT"

# --- the draft -----------------------------------------------------------------
#
# The status matters as much as the body: 200 tells a customer's agent the reply has been
# sent when nothing has left the building yet.
CODE=$(curl -s -w '%{http_code}' -X POST -H "$AUTH" -H "x-session: $SID" \
  -H "x-csrf: $CSRF" -o /tmp/gate-reply-body "$B/api/tickets/$ONE/reply")
[ "$CODE" = 202 ] || fail "a drafted reply is 202 Accepted, not $CODE — nothing has been delivered yet"
R=$(cat /tmp/gate-reply-body)
python3 - "$R" <<'PY' || fail "POST /api/tickets/{id}/reply did not answer a usable draft"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("event"), f"the answer must name the outbox event the reply is waiting in: {d}"
assert d.get("remaining") == 0, f"one draft out of a budget of one leaves 0 remaining: {d}"
PY
CODE=$(reply_code "$TWO")
[ "$CODE" = 429 ] || fail "the second draft on a budget of one must be 429 budget_exhausted, got $CODE"

# The stored ticket, and the draft that the model actually wrote.
python3 - "$(get "/test/ticket/$ONE")" <<'PY' || fail "the draft was answered but not stored on the ticket"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("state") == "answered", f"a ticket with a draft is answered: {d}"
r = d.get("reply")
assert isinstance(r, dict), f"the ticket has no reply block: {d}"
text = (r.get("text") or "").strip()
assert len(text) >= 20, f"no usable draft was stored: {text!r}"
assert r.get("event"), f"the stored reply must name its outbox event: {r}"
assert str(r.get("drafted_at", "")).endswith("Z"), f"drafted_at must be RFC3339 UTC: {r}"
# About THIS ticket, and not a slice of it: the seeded ticket is about being charged for
# the wrong plan, and a canned sentence mentions none of it.
low = text.lower()
assert any(w in low for w in ("plan", "invoice", "charg", "team", "pro", "billing")), \
    f"the draft is not about the ticket it answers: {text!r}"
assert text not in (d.get("body") or ""), "the draft is a verbatim slice of the customer's message"
PY

# --- and nothing was sent ------------------------------------------------------
#
# The reply is drafted, stored and queued. The courier part is a stub in this run, so the
# only way anything reaches the sink is this part sending it itself — which is the failure
# the whole app is built to prevent, and which every check above would pass while doing.
[ "$(sink_deliveries)" = 0 ] || fail \
  "the reply reached the far end without any delivery pass — this part is sending inline. When the far end is down that reply is lost, the budget was already spent, and nothing records that it existed. Enqueue it (outbox:dispatch) and let the courier deliver it."

# --- and a second reply to the same ticket is a conflict -----------------------
GOT=$(reply_code "$ONE")
[ "$GOT" = 409 ] || fail "a second reply to an already-answered ticket must be 409, got $GOT"

echo "support:desk — the reply part: passed"
