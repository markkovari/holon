# complaint:escalation's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "sup@example.test", "roles": ["supervisor"], "scopes": ["complaints:write","complaints:read","complaints:assign","complaints:escalate","complaints:resolve"]}
HTTP 201
[Captures]
sup_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "alice@example.test", "roles": ["agent"], "scopes": ["complaints:write","complaints:read","complaints:escalate","complaints:resolve"]}
HTTP 201
[Captures]
alice_token: jsonpath "$.token"
alice_subject: jsonpath "$.subject"

POST {{base}}/test/token
{"subject": "bob@example.test", "roles": ["agent"], "scopes": ["complaints:write","complaints:read","complaints:escalate","complaints:resolve"]}
HTTP 201
[Captures]
bob_token: jsonpath "$.token"

POST {{base}}/api/complaints
Authorization: Bearer {{alice_token}}
{"summary": "billing error"}
HTTP 201
[Captures]
complaint_id: jsonpath "$.id"

POST {{base}}/api/complaints/{{complaint_id}}/escalate
Authorization: Bearer {{alice_token}}
HTTP 409

POST {{base}}/api/complaints/{{complaint_id}}/assign
Authorization: Bearer {{alice_token}}
{"agent": "x"}
HTTP 403

POST {{base}}/api/complaints/{{complaint_id}}/assign
Authorization: Bearer {{sup_token}}
{"agent": "{{alice_subject}}"}
HTTP 200

POST {{base}}/api/complaints/{{complaint_id}}/escalate
Authorization: Bearer {{bob_token}}
HTTP 403

POST {{base}}/api/complaints/{{complaint_id}}/escalate
Authorization: Bearer {{alice_token}}
HTTP 200

POST {{base}}/api/complaints/{{complaint_id}}/resolve
Authorization: Bearer {{sup_token}}
HTTP 200
