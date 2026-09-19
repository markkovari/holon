# billing-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "biller-a@example.test", "password": "correcthorse1", "role": "biller"}
HTTP 201

POST {{base}}/register
{"email": "biller-b@example.test", "password": "correcthorse2", "role": "biller"}
HTTP 201

POST {{base}}/register
{"email": "cfo@example.test", "password": "correcthorse3", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "biller-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
biller_a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "biller-b@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
biller_b_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "cfo@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
cfo_token: jsonpath "$.access_token"

POST {{base}}/api/clients
Authorization: Bearer {{biller_a_token}}
{"name": "Wayne Enterprises", "email": "ap@wayne.test"}
HTTP 201
[Captures]
client_id: jsonpath "$.id"

POST {{base}}/api/invoices
Authorization: Bearer {{biller_a_token}}
{"client_id": "{{client_id}}", "line_items": [{"description": "Consulting", "amount": 2500}]}
HTTP 201
[Captures]
invoice_id: jsonpath "$.id"
[Asserts]
jsonpath "$.total" == 2500

# biller-a sees their own invoice
GET {{base}}/api/invoices
Authorization: Bearer {{biller_a_token}}
HTTP 200
[Asserts]
jsonpath "$.invoices" count == 1

# biller-b does not see biller-a's invoice — row-level, not just RBAC
GET {{base}}/api/invoices
Authorization: Bearer {{biller_b_token}}
HTTP 200
[Asserts]
jsonpath "$.invoices" count == 0

# biller-b cannot send biller-a's invoice
POST {{base}}/api/invoices/{{invoice_id}}/send
Authorization: Bearer {{biller_b_token}}
HTTP 403

# biller-a can send their own
POST {{base}}/api/invoices/{{invoice_id}}/send
Authorization: Bearer {{biller_a_token}}
HTTP 200

# a sent invoice cannot be sent again
POST {{base}}/api/invoices/{{invoice_id}}/send
Authorization: Bearer {{biller_a_token}}
HTTP 400

# a biller — even the owner — cannot mark it paid; only admin can
POST {{base}}/api/invoices/{{invoice_id}}/pay
Authorization: Bearer {{biller_a_token}}
HTTP 403

# admin sees every invoice, not just their own
GET {{base}}/api/invoices
Authorization: Bearer {{cfo_token}}
HTTP 200
[Asserts]
jsonpath "$.invoices" count == 1

POST {{base}}/api/invoices/{{invoice_id}}/pay
Authorization: Bearer {{cfo_token}}
HTTP 200
[Asserts]
jsonpath "$.status" == "paid"

GET {{base}}/api/invoices
HTTP 401
