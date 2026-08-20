#!/usr/bin/env bash
# triage:assist — the assist part
#
# The one gate in this app that spends a real model call, and it spends exactly one
# (CONTRACT.md is why: a check per attempt per branch multiplies, and the loop's
# wall-clock is the model's latency, not this component's).
#
# ASSERTING ON A REAL MODEL. Nothing here compares the summary to an expected string:
# `claude -p` behind the shim answers differently every time and a gate that wanted
# one sentence would fail correct work. What is asserted instead is the SHAPE (fields,
# types, the label menu), and two things a part cannot fake:
#
#   * the summary is not a copy of the input — an extractive `body[..80]` passes a
#     "non-empty summary" check and never calls anything;
#   * the summary is about THIS report — a canned string passes the copy check and
#     mentions nothing from the report.
#
# Together they are the proof the model was called. Without them "use the real
# provider" is unenforced, and the cheapest passing branch is the one that stubs the
# call away.
#
# DEGRADATION, in the same run and for free: a second host with the provider pointed
# at a closed port. A provider that is down must leave the report untouched, which is
# the failure mode a mocked-out gate never sees.
set -uo pipefail
# shellcheck source=components/triage-assist-domain/gate-lib.sh
. components/triage-assist-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

assist_requires_auth
gate_requires_capability "ai:inference/inference" \
  "the model is one interface away in this repository — \`classify\` and \`summarize\` over ai:inference, \
not an HTTP client this part writes itself"

trap gate_cleanup EXIT

token() { post /test/token "{\"subject\":\"$1\"}" | field token; }
seed_one() { gate_seed | sed -n "$1p"; }

# --- with the model reachable --------------------------------------------------
gate_shim_config
gate_serve
T=$(token ada)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"

ID=$(seed_one 1)
[ -n "$ID" ] || fail "the fixture produced no reports — the scaffold is broken, not the part"
BODY_BEFORE=$(get "/test/report/$ID")

ASSIST=$(curl -s -X POST -H "authorization: Bearer $T" "$B/api/reports/$ID/assist")
python3 - "$ASSIST" "$BODY_BEFORE" <<'PY' || fail "POST /api/reports/{id}/assist did not answer a usable assist"
import json, sys
a = json.loads(sys.argv[1] or "{}")
report = json.loads(sys.argv[2] or "{}")

sev = a.get("severity")
assert sev in ("critical", "major", "minor"), \
    f"severity must be one of the three labels the model was given, got {sev!r} (whole answer: {a})"
conf = a.get("confidence")
assert isinstance(conf, int) and 0 <= conf <= 1000, \
    f"confidence is classify's 0..=1000 milli-units, passed through as-is, got {conf!r}"

s = (a.get("summary") or "").strip()
assert 20 <= len(s) <= 600, f"a brief summary of a report is a sentence or two, got {len(s)} chars: {s!r}"

# Not a copy: an extractive slice of the input needs no model at all.
title, body = report.get("title", ""), report.get("body", "")
haystack = f"{title}\n{body}"
assert s not in haystack, "the summary is a verbatim slice of the report — that is extraction, not a model call"
assert s != title, "the summary is the title again"

# About THIS report: a canned sentence passes every check above.
words = ("safari", "button", "white", "checkout", "banner", "invisible", "render")
assert any(w in s.lower() for w in words), \
    f"the summary mentions nothing from the report — it is not about this report: {s!r}"
PY

# The same four fields, on the document, which is where the next reader looks.
STORED=$(get "/test/report/$ID")
python3 - "$STORED" "$ASSIST" <<'PY' || fail "the assist was answered but not written to the report"
import json, sys
d = json.loads(sys.argv[1] or "{}")
answered = json.loads(sys.argv[2] or "{}")
a = d.get("assist")
assert isinstance(a, dict), f"the report has no assist block: {d}"
assert a.get("severity") == answered.get("severity"), f"the stored severity differs from the answer: {a} vs {answered}"
assert a.get("summary") == answered.get("summary"), "the stored summary differs from the answer"
assert str(a.get("assisted_at", "")).endswith("Z"), f"assisted_at must be RFC3339 UTC: {a.get('assisted_at')!r}"
PY

# Reading it back through the part's own route.
GOT=$(curl -s -H "authorization: Bearer $T" "$B/api/reports/$ID/assist")
python3 - "$GOT" <<'PY' || fail "GET /api/reports/{id}/assist did not answer the stored assist"
import json, sys
a = json.loads(sys.argv[1] or "{}")
assert a.get("severity") in ("critical", "major", "minor"), a
PY

# Twice is a conflict, not a second call. A part that re-assists on every POST spends
# a model call per request forever, and this is the cheapest possible place to notice.
AGAIN=$(curl -s -X POST -H "authorization: Bearer $T" "$B/api/reports/$ID/assist")
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "authorization: Bearer $T" "$B/api/reports/$ID/assist")
[ "$CODE" = 409 ] || fail "assisting an already-assisted report must be 409, got $CODE"
python3 - "$AGAIN" <<'PY' || fail "a 409 must name the severity already on record"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("error") == "already_assisted", d
assert d.get("severity") in ("critical", "major", "minor"), f"the 409 must carry the stored severity: {d}"
PY

# The refusals that cost nothing.
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "authorization: Bearer $T" "$B/api/reports/nope/assist")
[ "$CODE" = 404 ] || fail "assisting an unknown report must be 404, got $CODE"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$B/api/reports/$ID/assist")
[ "$CODE" = 401 ] || fail "assisting with no bearer must be 401, got $CODE"

# --- with the model unreachable ------------------------------------------------
#
# A second host, provider pointed at a closed port. Port 1 refuses immediately, so
# this costs a connection attempt rather than a timeout.
gate_cleanup
GATE_CONFIG="--config anthropic:base-url=http://127.0.0.1:1 --config anthropic:timeout=5"
GATE_EGRESS="--egress 127.0.0.1:1"
GATE_PRIVATE_EGRESS=--allow-private-egress
gate_serve
T=$(token ada)
DOWN_ID=$(seed_one 1)
[ -n "$DOWN_ID" ] || fail "the fixture produced no reports on the second host"

CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "authorization: Bearer $T" \
  "$B/api/reports/$DOWN_ID/assist")
[ "$CODE" = 503 ] || fail "a provider that cannot be reached must be 503 assist_unavailable, got $CODE"

python3 - "$(get "/test/report/$DOWN_ID")" <<'PY' || fail "a failed model call left something behind on the report"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert "assist" not in d, \
    f"the provider was down and the report was written anyway — a report with an empty opinion attached: {d}"
PY

echo "triage:assist — the assist part: passed"
