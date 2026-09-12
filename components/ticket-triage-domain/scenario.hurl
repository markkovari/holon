# ticket:triage's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "lead@example.test", "roles": ["lead"], "scopes": ["tickets:write","tickets:read","tickets:resolve"]}
HTTP 201
[Captures]
lead_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "agent-b@example.test", "roles": ["agent:billing"], "scopes": ["tickets:write","tickets:read","tickets:resolve"]}
HTTP 201
[Captures]
agent_billing_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "agent-s@example.test", "roles": ["agent:search"], "scopes": ["tickets:write","tickets:read","tickets:resolve"]}
HTTP 201
[Captures]
agent_search_token: jsonpath "$.token"

POST {{base}}/api/tickets
Authorization: Bearer {{agent_billing_token}}
{"subject": "double charge", "body": "charged twice for the same order", "queue": "billing"}
HTTP 201
[Captures]
ticket_id: jsonpath "$.id"

# the fresh KV this gate run gets has exactly this one ticket, so "found at
# all" and "found and is this one" are the same assertion here.
GET {{base}}/api/tickets/search?q=charge&queue=billing
Authorization: Bearer {{agent_billing_token}}
HTTP 200
[Asserts]
jsonpath "$.results" count == 1
jsonpath "$.results[0].id" == "{{ticket_id}}"

# wrong-queue agent cannot resolve (policy:guard row-level rule)
POST {{base}}/api/tickets/{{ticket_id}}/resolve
Authorization: Bearer {{agent_search_token}}
HTTP 403

POST {{base}}/api/tickets/{{ticket_id}}/resolve
Authorization: Bearer {{agent_billing_token}}
HTTP 200

# a resolved ticket drops out of search
GET {{base}}/api/tickets/search?q=charge&queue=billing
Authorization: Bearer {{agent_billing_token}}
HTTP 200
[Asserts]
jsonpath "$.results" count == 0
