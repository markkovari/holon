#!/usr/bin/env bash
# invoice:copilot — the copilot part
#
# One model call, and the assertion is arithmetic rather than prose: 100.00 split three ways
# is 33.34 + 33.33 + 33.33, and every other answer is a cent short or a cent over. A model
# asked to do it says 33.33 three times and is confident; `money::allocate` is the only
# thing here allowed to produce a number, and this is where that is enforced to the cent.
#
# The model's contribution is checked separately and structurally: the memos must be about
# the prose and must not be a verbatim slice of it, which is what tells a real call from a
# canned list of "Line 1, Line 2, Line 3".
set -uo pipefail
# shellcheck source=components/invoice-copilot-domain/gate-lib.sh
. components/invoice-copilot-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

inv_requires_auth
gate_requires_capability "ai:inference/inference" \
  "the model is one interface away in this repository — not an HTTP client this part writes"
gate_requires_capability "money:amount/arithmetic" \
  "the arithmetic is a component: dividing by hand rounds a cent away and this check counts cents"

gate_shim_config
trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"biller"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
IDS=$(post /test/seed '{}' | python3 -c "import sys,json;[print(i) for i in json.load(sys.stdin).get('invoice_ids',[])]")
DRAFT=$(printf '%s' "$IDS" | sed -n 1p)
[ -n "$DRAFT" ] || fail "the fixture produced no invoices — the scaffold is broken, not the part"
suggest() { curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d "$2" "$B/api/invoices/$1/lines/suggest"; }
suggest_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" -d "$2" "$B/api/invoices/$1/lines/suggest"; }

PROSE='Two days of discovery workshops with the billing team, and a written summary of what we agreed.'
BODY="{\"prose\":\"$PROSE\",\"total\":\"100.00\",\"shares\":3}"

# --- the refusals, none of which costs a model call ----------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -d "$BODY" "$B/api/invoices/$DRAFT/lines/suggest")
[ "$GOT" = 401 ] || fail "suggesting with no bearer must be 401, got $GOT"
GOT=$(suggest_code nope "$BODY")
[ "$GOT" = 404 ] || fail "suggesting on an unknown invoice must be 404, got $GOT"
GOT=$(suggest_code "$DRAFT" "{\"prose\":\"$PROSE\",\"total\":\"100.00\",\"shares\":1}")
[ "$GOT" = 400 ] || fail "one share is not a split — must be 400 invalid_suggestion, got $GOT"
GOT=$(suggest_code "$DRAFT" "{\"prose\":\"$PROSE\",\"total\":\"100.00\",\"shares\":99}")
[ "$GOT" = 400 ] || fail "99 shares is out of range — must be 400 invalid_suggestion, got $GOT"
GOT=$(suggest_code "$DRAFT" "{\"prose\":\"$PROSE\",\"total\":\"not money\",\"shares\":3}")
[ "$GOT" = 400 ] || fail "a total money:amount cannot parse must be 400 bad_money, got $GOT"

# --- the split, to the cent ----------------------------------------------------
S=$(suggest "$DRAFT" "$BODY")
python3 - "$S" "$PROSE" <<'PY' || fail "the suggestion is not what an allocated split looks like"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
lines = d.get("lines")
assert isinstance(lines, list) and len(lines) == 3, f"three shares means three lines: {d}"
units = [l.get("units") for l in lines]
assert all(isinstance(u, int) for u in units), f"every amount is an integer in minor units: {units}"
assert sum(units) == 10000, (
    f"the lines sum to {sum(units)} minor units and the total was 10000. This is the cent "
    "that a model loses when it is asked to divide, and money::allocate is what does not: "
    "100.00 into 3 is 3334 + 3333 + 3333."
)
assert sorted(units, reverse=True) == [3334, 3333, 3333], (
    f"allocate distributes the remainder to the earliest shares: expected [3334, 3333, 3333], got {units}"
)
assert d.get("total_units") == 10000, f"the stored total must be the allocated sum: {d}"
assert d.get("total") == "100.00", f"total is money::format of the parsed amount: {d.get('total')!r}"

# The model's half: about the prose, and not a slice of it.
prose = sys.argv[2].lower()
memos = [(l.get("memo") or "").strip() for l in lines]
assert all(memos), f"every line needs a description: {memos}"
assert not all(m.lower().startswith("line ") for m in memos), (
    f"the memos are placeholders, so no model wrote them: {memos}"
)
joined = " ".join(memos).lower()
assert any(w in joined for w in ("workshop", "discovery", "summary", "billing", "agreed", "day")), (
    f"the descriptions are not about the work described: {memos}"
)
assert not any(m.lower() == prose for m in memos), "a memo is the whole prose, verbatim"
PY

python3 - "$(get "/test/invoice/$DRAFT")" <<'PY' || fail "the suggestion was answered but not stored"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert len(d.get("lines") or []) == 3, f"the lines must be stored on the invoice: {d}"
assert d.get("total_units") == 10000, f"the stored total must be the allocated sum: {d}"
assert d.get("state") == "draft", f"suggesting does not post an invoice: {d}"
PY

# A second suggestion replaces the lines: it is a draft, not an error.
S2=$(suggest "$DRAFT" "{\"prose\":\"A single day of pair programming.\",\"total\":\"50.00\",\"shares\":2}")
python3 - "$S2" <<'PY' || fail "a second suggestion must replace the lines rather than append to them"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
units = [l.get("units") for l in d.get("lines") or []]
assert len(units) == 2, f"two shares means two lines, not five: {d}"
assert sum(units) == 5000, f"the new total must be the new amount: {units}"
PY

echo "invoice:copilot — the copilot part: passed"
