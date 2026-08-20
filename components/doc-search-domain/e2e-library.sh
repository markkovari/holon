#!/usr/bin/env bash
# docsearch:agent — the library part
#
# Two things fail invisibly here and both are checked. A document STORED but not
# INDEXED answers `GET /api/docs/{id}` perfectly and is unfindable, which is a library
# that lies. And a search that matches titles by hand answers the one question a
# developer tries and nothing else — so the gate asks with a word from the body, which
# a title match cannot reach, and reads the artifact's imports for the index itself.
set -uo pipefail
# shellcheck source=components/doc-search-domain/gate-lib.sh
. components/doc-search-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

docs_requires_auth
gate_requires_capability "search:index/index" \
  "the index is a component in this repository — \`index-doc\` and \`query\`, not a scan over the store and not a substring match on titles"

trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
W=$(token ada)
[ -n "$W" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
DOC='{"title":"Rotating the signing key","text":"The webhook signer keeps two keys so an in-flight request signed with the old one still verifies during the overlap window.","tag":"security"}'

# --- the refusals ---------------------------------------------------------------
expect_post 401 /api/docs "$DOC" "filing a document with no bearer must be 401"
RO=$(token reader '["docs:read"]')
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $RO" -d "$DOC" "$B/api/docs")
[ "$GOT" = 403 ] || fail "a token with only docs:read must be 403 on a write, not $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $W" -d '{"title":"","text":"x","tag":"y"}' "$B/api/docs")
[ "$GOT" = 400 ] || fail "an empty title must be 400 invalid_doc, got $GOT"

# --- a document goes in, and is findable by its BODY ---------------------------
ID=$(curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $W" \
  -d "$DOC" "$B/api/docs" | field id)
[ -n "$ID" ] || fail "POST /api/docs returned no id"

python3 - "$(get "/test/doc/$ID")" <<'PY' || fail "the stored document is not what was filed"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("title") == "Rotating the signing key", d
assert d.get("tag") == "security", d
assert "overlap window" in d.get("text", ""), d
PY

# "overlap" appears only in the body. A title match cannot find this.
HITS=$(curl -s -H "authorization: Bearer $W" "$B/api/search?q=overlap")
python3 - "$HITS" "$ID" <<'PY' || fail "searching for a word from the document's BODY did not find it"
import json, sys
d = json.loads(sys.argv[1] or "{}")
hits = d.get("hits")
assert isinstance(hits, list) and hits, f"no hits for a word that is in the indexed text: {d}"
ids = [h.get("id") for h in hits]
assert sys.argv[2] in ids, f"the document just filed is not among the hits: {ids}"
h = next(h for h in hits if h.get("id") == sys.argv[2])
assert h.get("title") == "Rotating the signing key", \
    f"a hit must carry the title from the store — a caller cannot use a list of ULIDs: {h}"
assert isinstance(h.get("score"), (int, float)), f"a hit must carry the index's score: {h}"
scores = [x.get("score") for x in hits]
assert scores == sorted(scores, reverse=True), f"hits must be ordered by descending score: {scores}"
PY

# The tag filter is the index's, not a filter applied afterwards to everything.
python3 - "$(curl -s -H "authorization: Bearer $W" "$B/api/search?q=overlap&tag=ops")" <<'PY' \
  || fail "the tag filter did not exclude a document tagged something else"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("hits") == [], f"tag=ops must not match a security document: {d}"
PY

# A question the library cannot answer is an empty list, not an error: an empty library
# and a bad question are the same shape to a caller.
python3 - "$(curl -s -H "authorization: Bearer $W" "$B/api/search?q=sourdough")" <<'PY' \
  || fail "a query matching nothing must be 200 with an empty list"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("hits") == [], "a query matching nothing answered hits"
PY
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $W" "$B/api/docs/nope")
[ "$GOT" = 404 ] || fail "an unknown document id must be 404, got $GOT"

echo "docsearch:agent — the library part: passed"
