#!/usr/bin/env bash
# support:desk — the tickets part
#
# No model call. The one rule worth its own check: a delivery address nothing can deliver
# to is refused HERE. Accepted, it becomes a ticket that drafts a reply, spends the budget,
# enqueues, and dead-letters days later for a reason nobody upstream can act on.
set -uo pipefail
# shellcheck source=components/support-desk-domain/gate-lib.sh
. components/support-desk-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose
desk_requires_auth

trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
W=$(token agent)
[ -n "$W" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
TICKET='{"subject":"Cannot export my data","body":"The export button spins forever.","customer":"webhook:https://acme.test/hooks/ada"}'
open_ticket() { curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$2" "$B/api/tickets"; }
open_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$2" "$B/api/tickets"; }

expect_post 401 /api/tickets "$TICKET" "opening a ticket with no bearer must be 401"
RO=$(token reader '["tickets:read"]')
GOT=$(open_code "$RO" "$TICKET")
[ "$GOT" = 403 ] || fail "a token with only tickets:read must be 403 on a write, not $GOT"
GOT=$(open_code "$W" '{"subject":"","body":"x","customer":"webhook:https://acme.test/h"}')
[ "$GOT" = 400 ] || fail "an empty subject must be 400 invalid_ticket, got $GOT"

# The address check. `mailto:` is a real scheme and still not something this desk delivers.
for bad in 'ada@example.test' 'mailto:ada@example.test' 'https://acme.test/hooks/ada' ''; do
  GOT=$(open_code "$W" "{\"subject\":\"s\",\"body\":\"b\",\"customer\":\"$bad\"}")
  [ "$GOT" = 400 ] || fail "customer '$bad' cannot be delivered to and must be 400 invalid_ticket, got $GOT"
done

ID=$(open_ticket "$W" "$TICKET" | field id)
[ -n "$ID" ] || fail "POST /api/tickets returned no id"
python3 - "$(get "/test/ticket/$ID")" <<'PY' || fail "the stored ticket is not what the contract describes"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("state") == "open", f"a new ticket is open: {d}"
assert d.get("customer") == "webhook:https://acme.test/hooks/ada", d
assert "reply" not in d, "tickets must not invent a reply — that is the reply part's job"
assert str(d.get("opened_at", "")).endswith("Z"), f"opened_at must be RFC3339 UTC: {d}"
PY
READ=$(curl -s -H "authorization: Bearer $W" "$B/api/tickets/$ID")
python3 - "$READ" "$ID" <<'PY' || fail "GET /api/tickets/{id} did not answer the stored ticket"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("subject") == "Cannot export my data", d
assert d.get("id") == sys.argv[2], f"a ticket must carry its id: {d}"
PY
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $W" "$B/api/tickets/nope")
[ "$GOT" = 404 ] || fail "an unknown ticket id must be 404, got $GOT"

# The list, which is an index lookup and oldest first.
python3 - "$(curl -s -H "authorization: Bearer $W" "$B/api/tickets")" "$ID" <<'PY' || fail "GET /api/tickets did not list the ticket just opened"
import json, sys
items = json.loads(sys.argv[1] or "{}").get("tickets")
assert isinstance(items, list) and items, f"the open list is empty right after a ticket was opened: {items}"
assert sys.argv[2] in [t.get("id") for t in items], f"the new ticket is missing: {items}"
for t in items:
    assert t.get("state") == "open", f"the default list is the open one: {t}"
stamps = [t.get("opened_at") for t in items]
assert stamps == sorted(stamps), f"oldest first, and these are not: {stamps}"
PY
python3 - "$(curl -s -H "authorization: Bearer $W" "$B/api/tickets?state=answered")" <<'PY' || fail "filtering by a state nothing is in must be empty"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("tickets") == [], "nothing is answered yet and the list said otherwise"
PY

echo "support:desk — the tickets part: passed"
