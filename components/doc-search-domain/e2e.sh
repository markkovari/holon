#!/usr/bin/env bash
# docsearch:agent — the whole API, all three parts at once
#
# The JOIN gate. Each part's own gate judges it with the other two stubbed, which is what
# makes three agents possible — and is exactly why something has to judge them together.
#
# What only this gate can see: `stepup` writes a mark and `answer` reads it. Both parts
# pass alone against the ROUTER's fixture, which writes that mark in the contract's shape
# — so a part that invented its own shape passes its own gate and fails here. This gate
# never touches /test/stepup: the step-up is earned through the real routes, with a real
# TOTP code, and then a real question is asked over a document filed through the real
# library route. Nothing in the path is scaffold.
set -uo pipefail
# shellcheck source=components/doc-search-domain/gate-lib.sh
. components/doc-search-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

docs_requires_auth
gate_requires_capability "otp:totp/authenticator" "the composed API must still be checking codes through otp"
gate_requires_capability "quota:meter/meter" "the composed API must still be metering through quota"
gate_requires_capability "cache:store/cache" "the composed API must still be caching through the cache component"
gate_requires_capability "search:index/index" "the composed API must still be retrieving through the index"
gate_requires_capability "ai:inference/inference" "the composed API must still be reaching the model through ai-inference"

GATE_CONFIG="--config answer-budget=2 --config answer-period-secs=3600 --config answer-cache-ttl-secs=300"
gate_shim_config
trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"ada"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the parts"
AUTH="authorization: Bearer $T"

# --- a document filed through the library part ---------------------------------
ID=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d \
  '{"title":"The quiet hours window","text":"A digest scheduled inside the quiet hours window is held until 08:00 in the recipient tenant timezone, and never merged with the next one.","tag":"product"}' \
  "$B/api/docs" | field id)
[ -n "$ID" ] || fail "the library part did not accept a document, so nothing else can be judged"

# --- asking before stepping up: refused ----------------------------------------
Q="What happens to a digest scheduled inside the quiet hours window?"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "$AUTH" -d "{\"question\":\"$Q\"}" "$B/api/answer")
[ "$GOT" = 403 ] || fail "the composed API answered a question for a session that never stepped up (got $GOT)"

# --- stepping up for real, through the step-up part ----------------------------
SECRET=$(curl -s -X POST -H "$AUTH" "$B/api/mfa/enroll" | field secret)
[ -n "$SECRET" ] || fail "the step-up part did not provision a secret"
CODE=$(totp_now "$SECRET")
python3 - "$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d "{\"code\":\"$CODE\"}" "$B/api/mfa/verify")" \
  <<'PY' || fail "a correct TOTP code was refused by the composed API"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("verified") is True, "the real second factor did not verify"
PY

# --- and now the question, over the document that was filed -------------------
#
# This single request is the join: `answer` found the mark `stepup` wrote, retrieved the
# document `library` indexed, and asked the model about it. Any one of the three
# disagreeing about a shape makes this a 403, a 404 or a wrong answer.
ANS=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d "{\"question\":\"$Q\"}" "$B/api/answer")
python3 - "$ANS" "$ID" <<'PY' || fail "the three parts do not agree — see which of the three claims below failed"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
doc_id = sys.argv[2]
assert "error" not in d, (
    "the composed API refused a question from a session that verified through the real "
    f"routes. If this is step_up_required, `stepup` (src/stepup.rs) and `answer` "
    f"(src/answer.rs) disagree about the mark's shape — the contract's `stepups` "
    f"collection, indexed on `subject`, with `verified_at`. Got: {d}"
)
assert doc_id in (d.get("sources") or []), (
    "the answer does not cite the document filed through /api/docs. If sources is empty, "
    f"`library` (src/library.rs) indexed something `answer` (src/answer.rs) cannot find. Got: {d}"
)
a = (d.get("answer") or "").lower()
assert len(a) >= 20, f"no usable answer: {d}"
assert any(w in a for w in ("08:00", "8:00", "eight", "quiet", "held", "morning")), \
    f"the answer is not about the document it cited: {d.get('answer')!r}"
assert d.get("cached") is False, f"the first answer to a question is not a cache hit: {d}"
assert d.get("remaining") == 1, f"one answer out of a budget of two leaves 1: {d}"
PY

# The step-up part's own read of the state agrees with what the answer part did.
python3 - "$(curl -s -H "$AUTH" "$B/api/mfa")" <<'PY' || fail "the step-up part reports a state the answer part did not act on"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("verified") is True, \
    f"the answer part accepted the session but the step-up part reports it unverified: {d} — the two parts read the same state differently"
PY

echo "docsearch:agent — the whole API: passed"
