# timesheet-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "manager-a@example.test", "password": "correcthorse1", "role": "manager"}
HTTP 201
[Captures]
manager_a_subject: jsonpath "$.subject"

POST {{base}}/register
{"email": "manager-b@example.test", "password": "correcthorse2", "role": "manager"}
HTTP 201
[Captures]
manager_b_subject: jsonpath "$.subject"

POST {{base}}/register
{"email": "member-x@example.test", "password": "correcthorse3", "role": "member"}
HTTP 201

POST {{base}}/login
{"email": "manager-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
manager_a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "manager-b@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
manager_b_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "member-x@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
member_x_token: jsonpath "$.access_token"

POST {{base}}/api/projects
Authorization: Bearer {{member_x_token}}
{"name": "Website redesign"}
HTTP 201
[Captures]
project_id: jsonpath "$.id"

POST {{base}}/api/entries
Authorization: Bearer {{member_x_token}}
{"project_id": "{{project_id}}", "date": "2026-01-05", "hours": 6.5, "note": "hero section", "manager": "{{manager_a_subject}}"}
HTTP 201
[Captures]
entry_id: jsonpath "$.id"

# a request with no hours is rejected
POST {{base}}/api/entries
Authorization: Bearer {{member_x_token}}
{"project_id": "{{project_id}}", "date": "2026-01-05", "hours": 0, "manager": "{{manager_a_subject}}"}
HTTP 400

# the member sees their own entry
GET {{base}}/api/entries
Authorization: Bearer {{member_x_token}}
HTTP 200
[Asserts]
jsonpath "$.entries" count == 1
jsonpath "$.entries[0].status" == "pending"

# manager-b (not routed to) sees nothing
GET {{base}}/api/entries
Authorization: Bearer {{manager_b_token}}
HTTP 200
[Asserts]
jsonpath "$.entries" count == 0

# manager-b cannot approve an entry routed to manager-a
POST {{base}}/api/entries/{{entry_id}}/approve
Authorization: Bearer {{manager_b_token}}
HTTP 403

# the member who logged it cannot self-approve either
POST {{base}}/api/entries/{{entry_id}}/approve
Authorization: Bearer {{member_x_token}}
HTTP 403

# manager-a (routed to) sees it and can approve it
GET {{base}}/api/entries
Authorization: Bearer {{manager_a_token}}
HTTP 200
[Asserts]
jsonpath "$.entries" count == 1

POST {{base}}/api/entries/{{entry_id}}/approve
Authorization: Bearer {{manager_a_token}}
HTTP 200
[Asserts]
jsonpath "$.status" == "approved"

GET {{base}}/api/entries
HTTP 401
