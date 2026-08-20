#!/usr/bin/env bash
# invoice:copilot — the invoices part
set -uo pipefail
# shellcheck source=components/invoice-copilot-domain/gate-lib.sh
. components/invoice-copilot-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

inv_requires_auth
gate_requires_capability "ratelimit:guard/limiter" \
  "counting invoices per subject is a solved problem here — a counter in the record store is how this part fails"
gate_requires_capability "money:amount/arithmetic" \
  "whether a currency can be added up is money:amount's answer, not a list of three-letter codes written here"

GATE_CONFIG="--config max-attempts=3 --config lockout-window=60"
trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
W=$(token biller)
[ -n "$W" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
INV='{"customer":"acme-gmbh","currency":"EUR"}'
new() { curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$2" "$B/api/invoices"; }
new_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$2" "$B/api/invoices"; }

expect_post 401 /api/invoices "$INV" "opening an invoice with no bearer must be 401"
RO=$(token reader '["invoices:read"]')
GOT=$(new_code "$RO" "$INV")
[ "$GOT" = 403 ] || fail "a token with only invoices:read must be 403 on a write, not $GOT"
GOT=$(new_code "$W" '{"customer":"","currency":"EUR"}')
[ "$GOT" = 400 ] || fail "an empty customer must be 400 invalid_invoice, got $GOT"

# A currency the arithmetic cannot do. Refused here rather than at posting time.
GOT=$(new_code "$W" '{"customer":"acme-gmbh","currency":"QQQ"}')
[ "$GOT" = 400 ] || fail "a currency money:amount does not know must be 400 bad_money, got $GOT — an invoice that cannot be totalled is not a draft, it is a trap"

ID=$(new "$W" "$INV" | field id)
[ -n "$ID" ] || fail "POST /api/invoices returned no id"
python3 - "$(get "/test/invoice/$ID")" <<'PY' || fail "the stored invoice is not what the contract describes"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("state") == "draft", f"a new invoice is a draft: {d}"
assert d.get("lines") == [], f"a new invoice has no lines: {d}"
assert d.get("total_units") == 0, f"a new invoice totals zero, as an integer: {d}"
assert "entry" not in d, "invoices must not invent a posted entry — that is the posting part's job"
assert str(d.get("created_at", "")).endswith("Z"), f"created_at must be RFC3339 UTC: {d}"
PY
python3 - "$(curl -s -H "authorization: Bearer $W" "$B/api/invoices/$ID")" "$ID" <<'PY' || fail "GET /api/invoices/{id} did not answer the stored invoice"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("id") == sys.argv[2], f"an invoice must carry its id: {d}"
assert d.get("currency") == "EUR", d
PY
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $W" "$B/api/invoices/nope")
[ "$GOT" = 404 ] || fail "an unknown invoice id must be 404, got $GOT"

# The limit, counting what was accepted, keyed on the subject.
BURST=$(token burst)
for i in 1 2 3; do
  GOT=$(new_code "$BURST" "$INV")
  [ "$GOT" = 201 ] || fail "invoice $i of 3 within the limit must be accepted, got $GOT"
done
LOCKED=$(new "$BURST" "$INV")
python3 - "$LOCKED" <<'PY' || fail "past the limit the part must refuse and say how long to wait"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("error") == "rate_limited", d
assert isinstance(d.get("retry_after"), int) and d["retry_after"] > 0, f"retry_after must be the limiter's seconds: {d}"
PY
GOT=$(new_code "$W" "$INV")
[ "$GOT" = 201 ] || fail "locking out one subject must not lock out another, got $GOT"

echo "invoice:copilot — the invoices part: passed"
