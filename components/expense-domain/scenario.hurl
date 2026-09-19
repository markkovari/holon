# expense-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "employee-a@example.test", "password": "correcthorse1", "role": "employee"}
HTTP 201

POST {{base}}/register
{"email": "employee-b@example.test", "password": "correcthorse2", "role": "employee"}
HTTP 201

POST {{base}}/register
{"email": "controller@example.test", "password": "correcthorse3", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "employee-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "employee-b@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
b_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "controller@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"

# employee-a files a report
POST {{base}}/api/reports
Authorization: Bearer {{a_token}}
{"amount": 4500, "note": "Client dinner"}
HTTP 201
[Captures]
report_id: jsonpath "$.id"

# employee-a sees their own report
GET {{base}}/api/reports
Authorization: Bearer {{a_token}}
HTTP 200
[Asserts]
jsonpath "$.reports" count == 1

# employee-b does not see employee-a's report — row-level, not just RBAC
GET {{base}}/api/reports
Authorization: Bearer {{b_token}}
HTTP 200
[Asserts]
jsonpath "$.reports" count == 0

# employee-b cannot fetch employee-a's report by id
GET {{base}}/api/reports/{{report_id}}
Authorization: Bearer {{b_token}}
HTTP 403

# employee-a can fetch their own report by id
GET {{base}}/api/reports/{{report_id}}
Authorization: Bearer {{a_token}}
HTTP 200

# employee-b cannot approve employee-a's report
POST {{base}}/api/reports/{{report_id}}/approve
Authorization: Bearer {{b_token}}
HTTP 403

# even the owner cannot approve their own report — only admin can
POST {{base}}/api/reports/{{report_id}}/approve
Authorization: Bearer {{a_token}}
HTTP 403

# admin sees every report, not just their own
GET {{base}}/api/reports
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.reports" count == 1

# admin approves
POST {{base}}/api/reports/{{report_id}}/approve
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.status" == "approved"

# an approved report cannot be rejected afterward
POST {{base}}/api/reports/{{report_id}}/reject
Authorization: Bearer {{admin_token}}
HTTP 400

GET {{base}}/api/reports
HTTP 401
