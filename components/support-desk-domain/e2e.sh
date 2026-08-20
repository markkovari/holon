#!/usr/bin/env bash
# support:desk — the whole API, all three parts at once
#
# The JOIN gate, and it uses no fixture: the ticket is opened through `tickets`, the reply
# drafted through `reply`, and delivered through `courier` — to a sink this gate runs and
# can break. What only this gate can see is the payload shape passing between two parts that
# never call each other: `reply` writes it into the outbox and `courier` reads it out, and
# nothing else in the app would notice if they disagreed.
#
# One model call.
set -uo pipefail
# shellcheck source=components/support-desk-domain/gate-lib.sh
. components/support-desk-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

desk_requires_auth
gate_requires_capability "quota:meter/meter" "the composed API must still be metering drafts"
gate_requires_capability "ai:inference/inference" "the composed API must still be drafting through ai-inference"
gate_requires_capability "outbox:dispatch/queue" "the composed API must still be enqueuing rather than sending inline"
gate_requires_capability "notify:dispatch/dispatcher" "the composed API must still have something that actually sends"
gate_requires_capability "session:store/store" "the composed API must still be checking CSRF against the session that issued it"

GATE_CONFIG="--config reply-budget=5 --config reply-period-secs=3600 --config max-attempts=3 --config base-backoff=1"
sink_start
gate_shim_config
trap 'gate_cleanup; sink_stop' EXIT
gate_serve

T=$(post /test/token '{"subject":"agent","tenant":"acme"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the parts"
AUTH="authorization: Bearer $T"
SESS=$(post /test/session '{}')
SID=$(printf '%s' "$SESS" | field session)
CSRF=$(printf '%s' "$SESS" | field csrf)

# --- a ticket, through the part that owns tickets ------------------------------
ID=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d \
  "{\"subject\":\"Charged twice this month\",\"body\":\"There are two invoices dated the same day.\",\"customer\":\"webhook:$SINK_URL\"}" \
  "$B/api/tickets" | field id)
[ -n "$ID" ] || fail "the tickets part did not accept a ticket, so nothing else can be judged"

# --- a reply, through the part that drafts -------------------------------------
R=$(curl -s -X POST -H "$AUTH" -H "x-session: $SID" -H "x-csrf: $CSRF" "$B/api/tickets/$ID/reply")
EVENT=$(printf '%s' "$R" | field event)
[ -n "$EVENT" ] || fail "the reply part drafted nothing usable, so there is nothing to deliver: $R"
[ "$(sink_deliveries)" = 0 ] || fail "the reply reached the far end before any delivery pass ran — the reply part is sending inline, which is the failure this app exists to prevent"

# --- delivered, through the part that sends ------------------------------------
python3 - "$(curl -s -X POST -H "$AUTH" "$B/api/deliver")" <<'PY' || fail "the courier part could not deliver what the reply part enqueued"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("claimed") == 1, (
    "the courier claimed nothing. If the reply part enqueued under a different topic than "
    f"the contract's support.reply, the two never meet: {d}"
)
assert d.get("delivered") == 1, f"the sink answered 200 and this was not counted as delivered: {d}"
PY
[ "$(sink_deliveries)" = 1 ] || fail "the reply did not reach the far end exactly once (saw $(sink_deliveries) arrivals)"

# THE join assertion: what arrived is what the customer should read, which means the two
# parts agreed on every field of a payload neither of them shows anyone.
python3 - "$(cat "$SINK_LOG")" "$(get "/test/ticket/$ID")" <<'PY' || fail "the parts disagree about the payload — see which field below failed"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "nothing arrived at the far end at all"
arrival = json.loads(raw.split("\n")[0])
arrived = (arrival.get("body") or "").strip()
assert arrived, "a request arrived at the far end with an EMPTY body — the reply never left"
try:
    body = json.loads(arrived)
except json.JSONDecodeError as e:
    raise AssertionError(f"what arrived at the far end is not JSON ({e}): {arrived[:200]!r}")
ticket = json.loads(sys.argv[2] or "{}")
drafted = ((ticket.get("reply") or {}).get("text") or "").strip()
assert drafted, f"the ticket has no stored draft to compare against: {ticket}"
blob = json.dumps(body)
assert drafted[:40] in blob, (
    "what arrived at the customer's endpoint is not the draft that was stored. `reply` "
    "writes the payload and `courier` reads it, and nothing else in the app would notice "
    f"if they disagreed about a field name. Arrived: {blob[:300]}"
)
assert "Charged twice" in blob, f"the subject did not survive the trip: {blob[:300]}"
PY

echo "support:desk — the whole API: passed"
