#!/usr/bin/env bash
# invoice:copilot — the posting part
#
# No model call. Two properties, and both are about money leaving twice:
#
#   * the SAME idempotency key gets the SAME answer, and posts once. Not a 409 on the
#     second call — a caller retrying a timed-out request must receive what it would have
#     received the first time, or it cannot tell "already done" from "never happened".
#   * the ledger refuses an entry that does not balance, and this part does not post it
#     anyway.
#
# The retry is the whole reason this part exists: every HTTP client makes one, so a posting
# route without a key is a double charge waiting for a network hiccup.
set -uo pipefail
# shellcheck source=components/invoice-copilot-domain/gate-lib.sh
. components/invoice-copilot-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

inv_requires_auth
gate_requires_capability "idempotency:guard/store" \
  "posting once is a component in this repository — a flag in the record store races with the retry it is supposed to stop"
gate_requires_capability "ledger:doubleentry/ledger" \
  "double entry is a component: an entry that does not balance must be refused by something that knows what balanced means"

trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"biller"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
IDS=$(post /test/seed '{}' | python3 -c "import sys,json;[print(i) for i in json.load(sys.stdin).get('invoice_ids',[])]")
EMPTY=$(printf '%s' "$IDS" | sed -n 1p)
FILLED=$(printf '%s' "$IDS" | sed -n 2p)
[ -n "$FILLED" ] || fail "the fixture produced no invoices — the scaffold is broken, not the part"
post_it() { curl -s -X POST -H "$AUTH" -H "idempotency-key: $2" "$B/api/invoices/$1/post"; }
post_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" -H "idempotency-key: $2" "$B/api/invoices/$1/post"; }

# --- the refusals ---------------------------------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "idempotency-key: k" "$B/api/invoices/$FILLED/post")
[ "$GOT" = 401 ] || fail "posting with no bearer must be 401, got $GOT"
RO=$(post /test/token '{"subject":"reader","scopes":["invoices:read"]}' | field token)
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "authorization: Bearer $RO" -H "idempotency-key: k" "$B/api/invoices/$FILLED/post")
[ "$GOT" = 403 ] || fail "posting needs invoices:post — a read-only token must be 403, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" "$B/api/invoices/$FILLED/post")
[ "$GOT" = 400 ] || fail "posting with no Idempotency-Key must be 400 — a retry would charge twice, got $GOT"
GOT=$(post_code nope k1)
[ "$GOT" = 404 ] || fail "posting an unknown invoice must be 404, got $GOT"
GOT=$(post_code "$EMPTY" k2)
[ "$GOT" = 409 ] || fail "posting an invoice with no lines must be 409 nothing_to_post, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$B/api/invoices/$FILLED/entry")
[ "$GOT" = 404 ] || fail "an unposted invoice has no entry: must be 404 not_posted, got $GOT"

# --- posted once ----------------------------------------------------------------
FIRST=$(post_it "$FILLED" key-abc)
python3 - "$FIRST" "$FILLED" <<'PY' || fail "the first posting did not answer what the contract describes"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("total_units") == 10000, f"the posted total must be the invoice's: {d}"
assert str(d.get("posted_at", "")).endswith("Z"), f"posted_at must be RFC3339 UTC: {d}"
PY
python3 - "$(get "/test/invoice/$FILLED")" <<'PY' || fail "the posting was answered but the entry was not stored, or it does not balance"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("state") == "posted", f"a posted invoice is not a draft any more: {d}"
e = d.get("entry")
assert isinstance(e, dict), f"the invoice has no entry: {d}"
lines = e.get("lines") or []
assert len(lines) >= 2, f"double entry needs two sides: {e}"
debits = sum(l["amount"] for l in lines if l.get("side") == "debit")
credits = sum(l["amount"] for l in lines if l.get("side") == "credit")
assert debits == credits == 10000, (
    f"the two sides must be equal and must be the invoice total: debits {debits}, credits {credits}"
)
PY
python3 - "$(curl -s -H "$AUTH" "$B/api/invoices/$FILLED/entry")" <<'PY' || fail "GET /api/invoices/{id}/entry did not answer the stored entry"
import json, sys
e = json.loads(sys.argv[1] or "{}")
assert (e.get("lines") or e.get("entry")), f"the entry route answered nothing usable: {e}"
PY

# --- and the retry gets the same answer ----------------------------------------
AGAIN=$(post_it "$FILLED" key-abc)
CODE=$(post_code "$FILLED" key-abc)
python3 - "$FIRST" "$AGAIN" "$CODE" <<'PY' || fail "the same idempotency key did not get the same answer"
import json, sys
first, again, code = sys.argv[1], sys.argv[2], sys.argv[3]
assert json.loads(again or "{}") == json.loads(first or "{}"), (
    "a retry with the same Idempotency-Key must return the response the first call got, "
    f"verbatim. First: {first}\nAgain: {again}"
)
assert code == "201", (
    f"the retry answered {code}. A 409 tells a caller its request never happened when it "
    "did; the point of the key is that a retry is indistinguishable from the original."
)
PY

# A DIFFERENT key on an already-posted invoice is the one that must refuse: this is not a
# retry, it is a second posting, and it must not add a second entry.
BEFORE=$(get "/test/invoice/$FILLED")
GOT=$(post_code "$FILLED" key-xyz)
[ "$GOT" = 409 ] || fail "a new key on an already-posted invoice must be 409 already_posted, got $GOT — that is a second charge, not a retry"
AFTER=$(get "/test/invoice/$FILLED")
[ "$BEFORE" = "$AFTER" ] || fail "the refused second posting changed the invoice — the entry must be written exactly once"

echo "invoice:copilot — the posting part: passed"
