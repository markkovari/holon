#!/usr/bin/env bash
# support:desk — the courier part
#
# No model call, and the hardest gate in the five apps: at-least-once delivery is entirely
# about what happens when the far end refuses, and nothing about it is visible against a
# far end that works. So the gate runs its own webhook sink, records every arrival, and
# breaks it on purpose.
#
# Four things are asserted, and each one is a different way to lose a customer's reply:
#
#   * a 2xx is acked — the event leaves the outbox and is not delivered again;
#   * a 500 is NOT acked — the far end answered and refused, which is not delivery, and a
#     courier that acks it loses the reply with no trace anywhere;
#   * a refused event comes back and is delivered once the far end recovers;
#   * enough refusals dead-letter it, and a dead letter can be replayed — a reply that
#     cannot be recovered is simply gone.
set -uo pipefail
# shellcheck source=components/support-desk-domain/gate-lib.sh
. components/support-desk-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

desk_requires_auth
gate_requires_capability "outbox:dispatch/queue" \
  "claim/ack/fail is a component in this repository — a list of your own in the record store answers every happy path and loses a reply the first time the far end is down"
gate_requires_capability "notify:dispatch/dispatcher" \
  "this is the part that actually sends, and the sender is a component — an HTTP client written here is the wrong work"

# `max-attempts` low enough that a gate can exhaust it, `base-backoff` at 1 so a retry is
# visible in a test rather than in five minutes.
GATE_CONFIG="--config max-attempts=2 --config base-backoff=1"
sink_start
trap 'gate_cleanup; sink_stop' EXIT
gate_serve

T=$(post /test/token '{"subject":"agent","tenant":"acme"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
deliver() { curl -s -X POST -H "$AUTH" "$B/api/deliver"; }
enqueue() { post /test/enqueue "{\"target\":\"webhook:$SINK_URL\",\"body\":\"$1\"}" | field event; }

# --- the refusals ---------------------------------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$B/api/deliver")
[ "$GOT" = 401 ] || fail "running a delivery pass with no bearer must be 401, got $GOT"
RO=$(post /test/token '{"subject":"reader","scopes":["tickets:read"]}' | field token)
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "authorization: Bearer $RO" "$B/api/deliver")
[ "$GOT" = 403 ] || fail "delivering needs tickets:deliver — a read-only token must be 403, got $GOT"

# --- nothing to do is not an error ---------------------------------------------
python3 - "$(deliver)" <<'PY' || fail "a pass with an empty outbox must answer zeroes, not an error"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body — the route is not implemented, or it trapped"
d = json.loads(raw)
for k in ("claimed", "delivered", "failed", "dead"):
    assert d.get(k) == 0, f"an empty outbox is a pass that did nothing: {d}"
PY

# --- a 2xx is delivered, once --------------------------------------------------
E1=$(enqueue "the first reply")
[ -n "$E1" ] || fail "the fixture could not enqueue — the scaffold is broken, not the part"
python3 - "$(deliver)" <<'PY' || fail "a reply the far end accepted was not counted as delivered"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body — the route is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("claimed") == 1, f"one event was waiting: {d}"
assert d.get("delivered") == 1, f"the sink answered 200 and this pass did not count a delivery: {d}"
assert d.get("failed") == 0, d
PY
[ "$(sink_deliveries)" = 1 ] || fail "the sink saw $(sink_deliveries) arrivals, wanted exactly 1"
python3 - "$(cat "$SINK_LOG")" <<'PY' || fail "what arrived at the far end is not what was enqueued"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "nothing arrived at the far end at all, so there is nothing to compare"
line = json.loads(raw.split("\n")[0])
arrived = (line.get("body") or "").strip()
assert arrived, "a request arrived at the far end with an EMPTY body — the reply itself never left"
try:
    body = json.loads(arrived)
except json.JSONDecodeError as e:
    raise AssertionError(f"what arrived at the far end is not JSON ({e}): {arrived[:200]!r}")
assert "the first reply" in json.dumps(body), f"the reply's text did not reach the far end: {body}"
PY

# A second pass must not deliver it again: an acked event is gone from the outbox.
python3 - "$(deliver)" <<'PY' || fail "an acked event was claimed a second time"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body — the route is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("claimed") == 0, f"a delivered event must not be claimable again: {d}"
PY
[ "$(sink_deliveries)" = 1 ] || fail "the reply was delivered twice — the first pass did not ack it"

# --- a 500 is NOT delivered, and comes back -----------------------------------
sink_break
E2=$(enqueue "the second reply")
python3 - "$(deliver)" <<'PY' || fail "a refusal from the far end was counted as a delivery"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body — the route is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("claimed") == 1, f"one event was waiting: {d}"
assert d.get("delivered") == 0, (
    "the far end answered 500 and this pass counted it as delivered. A courier that acks a "
    f"refusal loses the reply with no trace anywhere: {d}"
)
assert d.get("failed") == 1, f"a refused send is a failure the outbox has to be told about: {d}"
PY
[ "$(sink_deliveries)" = 2 ] || fail "the refused attempt did not reach the sink at all"

# It comes back after the backoff, and arrives once the far end recovers.
sink_repair
sleep 2
python3 - "$(deliver)" <<'PY' || fail "a refused reply was never retried — it is lost"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body — the route is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("claimed") == 1, (
    "the refused event did not come back after its backoff. If `fail` was never called it "
    f"is still leased and nothing will ever deliver it: {d}"
)
assert d.get("delivered") == 1, f"the far end works again and the retry did not land: {d}"
PY
[ "$(sink_deliveries)" = 3 ] || fail "the retry did not reach the sink"

# --- enough refusals dead-letter it, and a dead letter can be replayed --------
sink_break
E3=$(enqueue "the third reply")
# The pass that exhausts `max-attempts` must SAY so. The outbox dead-letters on its own
# whatever the courier reads, so the dead-letter list below would pass a part that never
# looks at `fail`'s return value — and then nothing in the app ever reports that a reply was
# abandoned. This is the only place that distinguishes the two.
LAST=""
for _ in 1 2 3; do
  LAST=$(deliver)
  sleep 2
done
python3 - "$LAST" <<'PY' || fail "the pass that abandoned a reply did not report it"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body"
d = json.loads(raw)
assert d.get("dead", 0) >= 1, (
    "max-attempts is spent and this pass reported dead=0. `fail` RETURNS the event's new "
    "state and `dead` is the only signal that a reply has been abandoned for good — a part "
    f"that discards it leaves nothing anywhere to report the loss: {d}"
)
PY
python3 - "$(curl -s -H "$AUTH" "$B/api/dead-letters")" <<'PY' || fail "past max-attempts the reply must be dead-lettered, not retried forever"
import json, sys
events = json.loads(sys.argv[1] or "{}").get("events")
assert isinstance(events, list) and events, (
    "max-attempts is 2 and the far end refused every time; nothing is in the dead letters. "
    "Either `fail` was not called or its returned state was ignored."
)
e = events[0]
assert e.get("id"), f"a dead letter without its id cannot be replayed: {e}"
assert isinstance(e.get("attempts"), int) and e["attempts"] >= 2, f"attempts must be carried: {e}"
assert isinstance(e.get("payload"), dict), f"the payload must come back parsed, not as bytes: {e}"
PY
DEAD=$(curl -s -H "$AUTH" "$B/api/dead-letters" | python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    sys.exit('GET /api/dead-letters answered an empty body')
events = json.loads(raw).get('events') or []
if not events:
    sys.exit('GET /api/dead-letters answered no events')
print(events[0]['id'])
") || fail "the dead letter could not be read back, so replay cannot be judged"
sink_repair
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" "$B/api/dead-letters/$DEAD/replay")
[ "$GOT" = 204 ] || fail "replaying a dead letter must be 204, got $GOT"
sleep 1
python3 - "$(deliver)" <<'PY' || fail "a replayed dead letter was not delivered"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "POST /api/deliver answered with an empty body — the route is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("delivered") == 1, f"a replayed reply must be deliverable again: {d}"
PY
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" "$B/api/dead-letters/nope/replay")
[ "$GOT" = 404 ] || fail "replaying something the outbox does not know must be 404, got $GOT"

echo "support:desk — the courier part: passed"
