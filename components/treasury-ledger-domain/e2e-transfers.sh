#!/usr/bin/env bash
# treasury:ledger — the transfers part
#
# The hardest gate in this repository. Twelve requests, each trying to move an account's entire
# balance, all at once. Exactly one may succeed. The other eleven must be refused for the right
# reason, no account may go negative, and the sum across both accounts must be what it was
# before — because money that vanishes under contention is the failure this app exists to catch,
# and every implementation that checks the balance before taking the lock produces it.
set -uo pipefail
# shellcheck source=components/treasury-ledger-domain/gate-lib.sh
. components/treasury-ledger-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

treasury_requires_auth
gate_requires_capability "records:store/store" \
  "the store's revision is the only thing that makes a two-sided move safe — see CONTRACT.md on why a lease is not"
gate_requires_capability "money:amount/arithmetic" \
  "every amount comes from money:amount"
gate_requires_capability "ledger:doubleentry/ledger" \
  "the journal has two sides and something that knows what balanced means has to say so"
gate_requires_capability "fsm:workflow/engine" \
  "the transfer lifecycle is a definition, not a string assigned in three places"
gate_requires_capability "idempotency:guard/store" \
  "a retried transfer must not move money twice, and every client retries"

trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"treasurer"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
pair() { post /test/seed "{\"start\":\"$1\"}" | python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    sys.exit('the fixture answered nothing')
[print(i) for i in json.loads(raw).get('account_ids', [])]
"; }
xfer() { # xfer <key> <from> <to> <amount>
  curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -H "idempotency-key: $1" \
    -d "{\"from\":\"$2\",\"to\":\"$3\",\"amount\":\"$4\"}" "$B/api/transfers"
}
xfer_code() {
  curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
    -H "idempotency-key: $1" -d "{\"from\":\"$2\",\"to\":\"$3\",\"amount\":\"$4\"}" "$B/api/transfers"
}

IDS=$(pair "100.00")
L=$(printf '%s' "$IDS" | sed -n 1p)
R=$(printf '%s' "$IDS" | sed -n 2p)
[ -n "$L" ] && [ -n "$R" ] || fail "the fixture produced no accounts — the scaffold is broken, not the part"

# --- the refusals ---------------------------------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "idempotency-key: k" -d "{\"from\":\"$L\",\"to\":\"$R\",\"amount\":\"1.00\"}" "$B/api/transfers")
[ "$GOT" = 401 ] || fail "a transfer with no bearer must be 401, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d "{\"from\":\"$L\",\"to\":\"$R\",\"amount\":\"1.00\"}" "$B/api/transfers")
[ "$GOT" = 400 ] || fail "a transfer with no Idempotency-Key must be 400 — every client retries, got $GOT"
GOT=$(xfer_code k-same "$L" "$L" "1.00")
[ "$GOT" = 400 ] || fail "a transfer to the same account must be 400 same_account, got $GOT"
GOT=$(xfer_code k-nope "$L" nope "1.00")
[ "$GOT" = 404 ] || fail "an unknown destination must be 404, got $GOT"

# --- one transfer, both sides, and a journal line -------------------------------
OK=$(xfer k-one "$L" "$R" "25.00")
python3 - "$OK" <<'PY' || fail "a plain transfer did not answer both new balances"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the transfer route answered an empty body"
d = json.loads(raw)
assert "error" not in d, f"a 25.00 transfer between two accounts holding 100.00 was refused: {d}"
assert d.get("from_units") == 7500, f"the source must end at 7500: {d}"
assert d.get("to_units") == 12500, f"the destination must end at 12500: {d}"
assert d.get("transfer"), f"the answer must name the transfer it created: {d}"
PY
[ "$(units_of "$L")" = 7500 ] || fail "the source account's stored balance is $(units_of "$L"), not 7500"
[ "$(units_of "$R")" = 12500 ] || fail "the destination's stored balance is $(units_of "$R"), not 12500"

TID=$(printf '%s' "$OK" | field transfer)
python3 - "$(curl -s -H "$AUTH" "$B/api/transfers/$TID")" <<'PY' || fail "the transfer was not recorded as the contract describes"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "GET /api/transfers/{id} answered nothing"
d = json.loads(raw)
assert d.get("state") == "settled", f"a completed transfer is settled: {d}"
assert d.get("units") == 2500, f"the recorded amount must be the amount moved: {d}"
PY
python3 - "$(curl -s -H "$AUTH" "$B/api/journal")" "$L" "$R" <<'PY' || fail "a settled transfer left no journal line, so reconciliation can never see it"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "GET /api/journal answered nothing"
lines = json.loads(raw).get("lines")
assert isinstance(lines, list) and lines, f"the journal is empty after a settled transfer: {lines}"
mine = [l for l in lines if l.get("from") == sys.argv[2] and l.get("to") == sys.argv[3]]
assert mine, f"no journal line names this pair: {lines}"
assert mine[-1].get("units") == 2500, f"the journal line must carry the amount: {mine[-1]}"
PY

# --- the retry every client makes ----------------------------------------------
AGAIN=$(xfer k-one "$L" "$R" "25.00")
python3 - "$OK" "$AGAIN" <<'PY' || fail "the same idempotency key moved money twice or answered differently"
import json, sys
first, again = json.loads(sys.argv[1] or "{}"), json.loads(sys.argv[2] or "{}")
assert again == first, (
    f"a retry with the same key must answer exactly what the first call answered.\n  first: {first}\n  again: {again}"
)
PY
[ "$(units_of "$L")" = 7500 ] || fail "the retry moved money again — the source is now $(units_of "$L"), not 7500"

# --- and now, many at once, each for everything, three times over ---------------
#
# THREE ROUNDS, not one, and this is not belt-and-braces. A contention assertion is
# probabilistic in one direction: a correct implementation is safe under every interleaving, so
# this can never fail correct work — but a BROKEN one only loses money when the requests actually
# overlap, and sometimes they do not. Measured while building this gate: the same double-spending
# implementation was caught when run directly and slipped through a single round inside the
# rehearsal. One round is a coin flip that only ever lies in favour of a bug.
for round in 1 2 3; do
  IDS=$(pair "60.00")
  A=$(printf '%s' "$IDS" | sed -n 1p)
  Z=$(printf '%s' "$IDS" | sed -n 2p)
  [ -n "$A" ] && [ -n "$Z" ] || fail "the fixture stopped producing accounts in round $round"
  BEFORE=$(( $(units_of "$A") + $(units_of "$Z") ))
  CODES=$(seq 16 | xargs -P 16 -I{} curl -s -o /dev/null -w '%{http_code}\n' -X POST \
    -H 'content-type: application/json' -H "$AUTH" -H "idempotency-key: storm-$round-{}" \
    -d "{\"from\":\"$A\",\"to\":\"$Z\",\"amount\":\"60.00\"}" "$B/api/transfers" \
    | sort | uniq -c | tr -s ' ' | paste -sd' ' -)
  AFTER=$(( $(units_of "$A") + $(units_of "$Z") ))
  python3 - "$CODES" "$(units_of "$A")" "$(units_of "$Z")" "$BEFORE" "$AFTER" "$round" <<'PY' || fail "the contention test found money missing, duplicated, or an account overdrawn"
import re, sys
codes, a, z, before, after, rnd = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]), sys.argv[6]
counts = {code: int(n) for n, code in re.findall(r"(\d+) (\d{3})", codes)}
settled = counts.get("201", 0)
assert a >= 0 and z >= 0, f"round {rnd}: an account went negative: from={a} to={z}"
assert before == after, (
    f"round {rnd}: the two accounts held {before} minor units before sixteen simultaneous "
    f"transfers and {after} after. Money was created or destroyed. Codes: [{codes}]"
)
assert settled == 1, (
    f"round {rnd}: {settled} of sixteen transfers of the ENTIRE balance succeeded. Exactly one "
    f"can: the comparison and the write have to be one CAS on the same revision, or every "
    f"request decides against a balance that is already gone. Codes: [{codes}]"
)
assert a == 0 and z == 12000, f"round {rnd}: after the one settlement the source is empty and the destination holds both: from={a} to={z}"
refused = counts.get("409", 0)
assert refused >= 12, (
    f"round {rnd}: only {refused} of the fifteen losers were refused with 409 "
    f"insufficient_funds. The rest failed some other way, and a caller cannot tell 'no money' "
    f"from 'try again': [{codes}]"
)
PY
done

# A refusal is a state, not a silence: the refused transfers are on record.
python3 - "$(curl -s -H "$AUTH" "$B/api/journal?limit=500")" <<'PY' || fail "the journal grew by more than the one settled transfer"
import json, sys
lines = json.loads((sys.argv[1] or "{}")).get("lines") or []
storm = [l for l in lines if l.get("units") == 6000]
# Three rounds, one settlement each. Every other attempt was refused, and a refusal moved
# nothing — so it has no line here however carefully it was recorded as a transfer.
assert len(storm) == 3, (
    f"three rounds of sixteen simultaneous transfers of 60.00 settled once each and produced "
    f"{len(storm)} journal lines of 6000. Only a settlement is journalled; a refusal is not a "
    "movement, and an auditor reading these lines would find money that never went anywhere."
)
PY

echo "treasury:ledger — the transfers part: passed"
