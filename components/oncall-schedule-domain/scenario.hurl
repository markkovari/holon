# oncall:schedule's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "lead@example.test", "roles": ["lead"], "scopes": ["shifts:write","shifts:read","shifts:reassign"]}
HTTP 201
[Captures]
lead_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "alice@example.test", "roles": ["engineer"], "scopes": ["shifts:write","shifts:read","shifts:reassign"]}
HTTP 201
[Captures]
alice_token: jsonpath "$.token"
alice_subject: jsonpath "$.subject"

POST {{base}}/test/token
{"subject": "bob@example.test", "roles": ["engineer"], "scopes": ["shifts:write","shifts:read","shifts:reassign"]}
HTTP 201
[Captures]
bob_token: jsonpath "$.token"
bob_subject: jsonpath "$.subject"

POST {{base}}/api/shifts
Authorization: Bearer {{lead_token}}
{"engineer": "{{alice_subject}}", "starts_at": 1234567890}
HTTP 201
[Captures]
shift_id: jsonpath "$.id"

# bob cannot reassign alice's shift (policy:guard row-level rule)
POST {{base}}/api/shifts/{{shift_id}}/reassign
Authorization: Bearer {{bob_token}}
{"engineer": "{{bob_subject}}"}
HTTP 403

# alice (owner) reassigns her own shift to bob
POST {{base}}/api/shifts/{{shift_id}}/reassign
Authorization: Bearer {{alice_token}}
{"engineer": "{{bob_subject}}"}
HTTP 200

# lead overrides and reassigns back to alice
POST {{base}}/api/shifts/{{shift_id}}/reassign
Authorization: Bearer {{lead_token}}
{"engineer": "{{alice_subject}}"}
HTTP 200
