# incident:response's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "admin@example.test", "roles": ["admin"], "scopes": ["incidents:write","incidents:read","incidents:assign","incidents:resolve"]}
HTTP 201
[Captures]
admin_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "alice@example.test", "roles": ["responder"], "scopes": ["incidents:write","incidents:read","incidents:resolve"]}
HTTP 201
[Captures]
alice_token: jsonpath "$.token"
alice_subject: jsonpath "$.subject"

POST {{base}}/test/token
{"subject": "bob@example.test", "roles": ["responder"], "scopes": ["incidents:write","incidents:read","incidents:resolve"]}
HTTP 201
[Captures]
bob_token: jsonpath "$.token"
bob_subject: jsonpath "$.subject"

POST {{base}}/api/incidents
Authorization: Bearer {{alice_token}}
{"title": "prod down", "severity": "high"}
HTTP 201
[Captures]
incident_id: jsonpath "$.id"

# unassigned — cannot resolve yet
POST {{base}}/api/incidents/{{incident_id}}/resolve
Authorization: Bearer {{alice_token}}
HTTP 409

# a non-admin cannot assign (RBAC)
POST {{base}}/api/incidents/{{incident_id}}/assign
Authorization: Bearer {{alice_token}}
{"responder": "irrelevant"}
HTTP 403

POST {{base}}/api/incidents/{{incident_id}}/assign
Authorization: Bearer {{admin_token}}
{"responder": "{{alice_subject}}"}
HTTP 200

# bob (not assigned) cannot resolve (policy:guard row-level rule)
POST {{base}}/api/incidents/{{incident_id}}/resolve
Authorization: Bearer {{bob_token}}
HTTP 403

# alice (assigned) resolves
POST {{base}}/api/incidents/{{incident_id}}/resolve
Authorization: Bearer {{alice_token}}
HTTP 200

# a second incident, admin overrides someone else's assignment
POST {{base}}/api/incidents
Authorization: Bearer {{bob_token}}
{"title": "leak", "severity": "low"}
HTTP 201
[Captures]
incident2_id: jsonpath "$.id"

POST {{base}}/api/incidents/{{incident2_id}}/assign
Authorization: Bearer {{admin_token}}
{"responder": "{{bob_subject}}"}
HTTP 200

POST {{base}}/api/incidents/{{incident2_id}}/resolve
Authorization: Bearer {{admin_token}}
HTTP 200
