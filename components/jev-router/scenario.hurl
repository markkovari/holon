# jev-router's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.
# Mock script (see gate.sh): "printer" -> helpdesk-domain @ 0.94, "appointment"
# -> clinic-domain @ 0.90, anything else -> helpdesk-domain @ 0.20 flat.

GET {{base}}/health
HTTP 200

# A high-confidence match routes decisively.
POST {{base}}/api/route
{"query": "the printer on the third floor is jammed"}
HTTP 200
[Asserts]
jsonpath "$.domain" == "helpdesk-domain"
jsonpath "$.decision" == "route"
jsonpath "$.confidence" == 0.94

# A different high-confidence match routes to its own domain.
POST {{base}}/api/route
{"query": "I need to book an appointment"}
HTTP 200
[Asserts]
jsonpath "$.domain" == "clinic-domain"
jsonpath "$.decision" == "route"

# A flat, low-confidence match falls back to disambiguation rather than a route
# — this is the whole point of the confidence gate.
POST {{base}}/api/route
{"query": "something entirely unrelated to any domain"}
HTTP 200
[Asserts]
jsonpath "$.domain" == "unknown"
jsonpath "$.decision" == "disambiguate"
jsonpath "$.distribution" count == 4

# A missing `query` field is a 400, not a 503 — validation happens before any
# provider call.
POST {{base}}/api/route
{}
HTTP 400

# GET on the routing path is not a route at all.
GET {{base}}/api/route
HTTP 404
