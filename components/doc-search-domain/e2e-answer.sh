#!/usr/bin/env bash
# docsearch:agent — the answer part
#
# The one gate here that spends a model call, and it spends exactly one — which is only
# possible because the interesting properties are about the calls that must NOT happen:
#
#   * the same question twice costs one model call and one budget unit, not two;
#   * a question the library cannot support costs nothing at all;
#   * once the budget is gone, an answer already paid for is still served — which is
#     only true if the cache is consulted BEFORE the meter, and is the one check that
#     tells the contract's order from a plausible reordering of it.
#
# On a real model nothing is compared to an expected string: the shape is asserted, and
# two things a part cannot fake — that the answer is not a slice of the sources, and
# that it is about THIS question.
set -uo pipefail
# shellcheck source=components/doc-search-domain/gate-lib.sh
. components/doc-search-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

docs_requires_auth
gate_requires_capability "ai:inference/inference" \
  "the model is one interface away in this repository — not an HTTP client this part writes"
gate_requires_capability "quota:meter/meter" \
  "counting what a subject spent in a period is a solved problem here — a counter in the record store is how this part fails"
gate_requires_capability "cache:store/cache" \
  "the cache is a component, and a HashMap in this part's own memory dies with the instance"
gate_requires_capability "search:index/index" \
  "retrieval is the index's job — a model asked without sources is this app inventing things"

# A budget of ONE, so the whole cost story fits in a single model call.
GATE_CONFIG="--config answer-budget=1 --config answer-period-secs=3600 --config answer-cache-ttl-secs=300"
gate_shim_config
trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"}" | field token; }
gate_seed >/dev/null || true
T=$(token ada)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
ask() { curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $T" \
  -d "{\"question\":\"$1\"}" "$B/api/answer"; }
ask_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $T" -d "{\"question\":\"$1\"}" "$B/api/answer"; }

# --- no step-up, no answer ------------------------------------------------------
#
# Before the fixture marks anyone: the first check must be the one that costs nothing.
GOT=$(ask_code "How often does the reconciler poll inventory?")
[ "$GOT" = 403 ] || fail "a session that has not stepped up must be 403 step_up_required, got $GOT"

post /test/stepup '{"subject":"ada"}' >/dev/null

# --- the first real question ----------------------------------------------------
Q="How long does the reconciler wait between inventory polls?"
FIRST=$(ask "$Q")
python3 - "$FIRST" <<'PY' || fail "the first answer is not usable"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
a = (d.get("answer") or "").strip()
assert 5 <= len(a) <= 2000, f"no usable answer, got {len(a)} chars: {a!r}"
assert d.get("cached") is False, f"the first answer to a question cannot be cached: {d}"
srcs = d.get("sources")
assert isinstance(srcs, list) and srcs, f"an answer must name the documents it came from: {d}"
assert d.get("remaining") == 0, f"one answer out of a budget of one leaves 0 remaining, got {d.get('remaining')!r}"
# About THIS question, and not a slice of the source: the seeded document says the
# reconciler polls every three seconds, and a canned sentence mentions none of it.
low = a.lower()
assert any(w in low for w in ("three", "3 second", "3-second", "second")), \
    f"the answer says nothing about the interval it was asked for: {a!r}"
PY

# --- the same question again: free ---------------------------------------------
START=$(python3 -c "import time;print(time.time())")
SECOND=$(ask "$Q")
ELAPSED=$(python3 -c "import time,sys;print(round(time.time()-float(sys.argv[1]),1))" "$START")
python3 - "$SECOND" "$FIRST" "$ELAPSED" <<'PY' || fail "the same question a second time did not come from the cache"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
first = json.loads(sys.argv[2] or "{}")
elapsed = float(sys.argv[3])
assert d.get("cached") is True, f"the second identical question must be served from the cache: {d}"
assert d.get("answer") == first.get("answer"), "a cache hit must return the answer that was cached"
assert d.get("remaining") == 0, f"a cache hit spends nothing, so remaining is unchanged: {d}"
# A real model call through the shim takes seconds. A cache hit cannot.
assert elapsed < 4.0, f"the second answer took {elapsed}s — that is a model call, not a cache hit"
PY

# --- a question the library cannot support: also free --------------------------
GOT=$(ask_code "What temperature should I proof sourdough at?")
[ "$GOT" = 404 ] || fail "a question with no matching sources must be 404 no_sources, got $GOT"

# --- the budget is gone, and the paid-for answer is still served ---------------
NEW=$(ask "Why does raising the per-instance memory ceiling cost address space?")
python3 - "$NEW" <<'PY' || fail "past the budget the part must refuse and say when"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("error") == "budget_exhausted", f"a second distinct question on a budget of one must be refused: {d}"
assert isinstance(d.get("retry_after"), int) and d["retry_after"] > 0, f"a refusal must say how long to wait: {d}"
PY
GOT=$(ask_code "Why does raising the per-instance memory ceiling cost address space?")
[ "$GOT" = 429 ] || fail "a refused question must be 429, got $GOT"

# THE ordering check: an answer already paid for survives the budget running out. If the
# meter is consulted before the cache, this is a 429 and the part has the order wrong.
AGAIN=$(ask "$Q")
python3 - "$AGAIN" <<'PY' || fail "an answer already paid for stopped being served once the budget ran out — the cache must be checked BEFORE the meter"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("cached") is True and d.get("answer"), \
    f"the cached answer must still be served when the budget is exhausted: {d}"
PY

echo "docsearch:agent — the answer part: passed"
