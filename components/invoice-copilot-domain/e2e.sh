#!/usr/bin/env bash
# invoice:copilot — the whole API, all three parts at once
#
# The JOIN gate, and it uses no fixture: the invoice is opened through `invoices`, its lines
# are suggested through `copilot`, and it is posted through `posting`. What only this gate
# sees is that the currency one part chose, the allocation another performed, and the entry
# the third balanced are all about the same money — three parts that never call each other
# and share nothing but a stored document.
#
# One model call.
set -uo pipefail
# shellcheck source=components/invoice-copilot-domain/gate-lib.sh
. components/invoice-copilot-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

inv_requires_auth
gate_requires_capability "ratelimit:guard/limiter" "the composed API must still be counting invoices through the limiter"
gate_requires_capability "ai:inference/inference" "the composed API must still be asking the model for the words"
gate_requires_capability "money:amount/arithmetic" "the composed API must still be doing its arithmetic in money:amount"
gate_requires_capability "ledger:doubleentry/ledger" "the composed API must still be balancing through the ledger"
gate_requires_capability "idempotency:guard/store" "the composed API must still post exactly once"

GATE_CONFIG="--config max-attempts=10 --config lockout-window=60"
gate_shim_config
trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"biller"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the parts"
AUTH="authorization: Bearer $T"

# --- an invoice, through the part that owns invoices ---------------------------
ID=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"customer":"acme-gmbh","currency":"EUR"}' "$B/api/invoices" | field id)
[ -n "$ID" ] || fail "the invoices part did not accept an invoice, so nothing else can be judged"

# --- lines, through the part that talks to the model --------------------------
#
# 10.00 into 3 is 3.34 + 3.33 + 3.33. A part that divides by hand loses a cent here and the
# ledger refuses the entry two steps later — which is the whole chain in one number.
S=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"prose":"An afternoon reviewing the billing migration, and notes on what to change.","total":"10.00","shares":3}' \
  "$B/api/invoices/$ID/lines/suggest")
python3 - "$S" <<'PY' || fail "the copilot part could not produce an allocated split"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
units = [l.get("units") for l in d.get("lines") or []]
assert len(units) == 3, f"three shares means three lines: {d}"
assert sum(units) == 1000, f"10.00 into three shares still totals 1000 minor units, got {sum(units)}: {units}"
assert sorted(units, reverse=True) == [334, 333, 333], f"expected [334, 333, 333]: {units}"
PY

# --- posted, through the part that owns posting --------------------------------
P=$(curl -s -X POST -H "$AUTH" -H "idempotency-key: join-key" "$B/api/invoices/$ID/post")
python3 - "$P" "$(get "/test/invoice/$ID")" <<'PY' || fail "the three parts do not agree — see which claim below failed"
import json, sys
posted = json.loads(sys.argv[1] or "{}")
inv = json.loads(sys.argv[2] or "{}")
assert "error" not in posted, (
    "the composed API refused to post an invoice built through its own routes. If this is "
    "nothing_to_post, `copilot` (src/copilot.rs) stored lines somewhere `posting` "
    f"(src/posting.rs) does not read. Got: {posted}"
)
assert posted.get("total_units") == 1000, (
    f"the posted total is not the allocated total. `copilot` and `posting` disagree about "
    f"the invoice's shape: {posted} vs {inv}"
)
e = inv.get("entry") or {}
lines = e.get("lines") or []
debits = sum(l["amount"] for l in lines if l.get("side") == "debit")
credits = sum(l["amount"] for l in lines if l.get("side") == "credit")
assert debits == credits == 1000, (
    f"the ledger entry does not balance against the allocated total: debits {debits}, "
    f"credits {credits}, invoice {inv.get('total_units')}"
)
PY

# And the retry a real client would make.
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" -H "idempotency-key: join-key" "$B/api/invoices/$ID/post")
[ "$CODE" = 201 ] || fail "the retry of a successful posting must answer as the first did (201), got $CODE"

echo "invoice:copilot — the whole API: passed"
