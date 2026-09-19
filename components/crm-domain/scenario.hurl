# crm-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

# --- registration + login --------------------------------------------------

POST {{base}}/register
{"email": "rep-a@example.test", "password": "correcthorse1", "role": "rep"}
HTTP 201

POST {{base}}/register
{"email": "rep-b@example.test", "password": "correcthorse2", "role": "rep"}
HTTP 201

POST {{base}}/register
{"email": "boss@example.test", "password": "correcthorse3", "role": "admin"}
HTTP 201

# a duplicate registration is refused
POST {{base}}/register
{"email": "rep-a@example.test", "password": "correcthorse1", "role": "rep"}
HTTP 409

POST {{base}}/login
{"email": "rep-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
rep_a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "rep-b@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
rep_b_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "boss@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
boss_token: jsonpath "$.access_token"

# a wrong password never says which part was wrong
POST {{base}}/login
{"email": "rep-a@example.test", "password": "wrong"}
HTTP 401

GET {{base}}/me
Authorization: Bearer {{rep_a_token}}
HTTP 200
[Asserts]
jsonpath "$.roles" contains "rep"

# --- contacts + deals --------------------------------------------------------

POST {{base}}/api/contacts
Authorization: Bearer {{rep_a_token}}
{"name": "Acme Corp", "email": "buyer@acme.test", "company": "Acme"}
HTTP 201

POST {{base}}/api/deals
Authorization: Bearer {{rep_a_token}}
{"title": "Acme renewal", "amount": 5000}
HTTP 201
[Captures]
deal_id: jsonpath "$.id"

# rep-a sees their own deal
GET {{base}}/api/deals
Authorization: Bearer {{rep_a_token}}
HTTP 200
[Asserts]
jsonpath "$.deals" count == 1
jsonpath "$.deals[0].id" == "{{deal_id}}"

# rep-b does NOT see rep-a's deal — the row-level rule, not just RBAC
GET {{base}}/api/deals
Authorization: Bearer {{rep_b_token}}
HTTP 200
[Asserts]
jsonpath "$.deals" count == 0

# rep-b cannot move rep-a's deal
POST {{base}}/api/deals/{{deal_id}}/stage
Authorization: Bearer {{rep_b_token}}
{"stage": "qualified"}
HTTP 403

# rep-a can move their own deal
POST {{base}}/api/deals/{{deal_id}}/stage
Authorization: Bearer {{rep_a_token}}
{"stage": "qualified"}
HTTP 200

# an invalid stage is refused
POST {{base}}/api/deals/{{deal_id}}/stage
Authorization: Bearer {{rep_a_token}}
{"stage": "not-a-real-stage"}
HTTP 400

# rep-a can leave a note
POST {{base}}/api/deals/{{deal_id}}/notes
Authorization: Bearer {{rep_a_token}}
{"text": "Sent the renewal quote"}
HTTP 200

# admin sees every deal, not just their own
GET {{base}}/api/deals
Authorization: Bearer {{boss_token}}
HTTP 200
[Asserts]
jsonpath "$.deals" count == 1

# admin may move a deal they don't own
POST {{base}}/api/deals/{{deal_id}}/stage
Authorization: Bearer {{boss_token}}
{"stage": "won"}
HTTP 200

# an unauthenticated request is refused
GET {{base}}/api/deals
HTTP 401
