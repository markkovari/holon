# invoice:approval's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "clerk@example.test", "roles": ["clerk"], "scopes": ["invoices:write","invoices:read"]}
HTTP 201
[Captures]
clerk_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "appr-eng@example.test", "roles": ["approver:engineering"], "scopes": ["invoices:read","invoices:approve"]}
HTTP 201
[Captures]
appr_eng_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "appr-sales@example.test", "roles": ["approver:sales"], "scopes": ["invoices:read","invoices:approve"]}
HTTP 201
[Captures]
appr_sales_token: jsonpath "$.token"

POST {{base}}/api/invoices
Authorization: Bearer {{clerk_token}}
{"vendor": "Acme Corp", "dept": "engineering", "amount": "1099.00", "currency": "USD"}
HTTP 201
[Captures]
invoice_id: jsonpath "$.id"

POST {{base}}/api/invoices
Authorization: Bearer {{clerk_token}}
{"vendor": "Bad", "dept": "engineering", "amount": "not-a-number", "currency": "USD"}
HTTP 400

# wrong-department approver (policy:guard row-level rule, no admin override)
POST {{base}}/api/invoices/{{invoice_id}}/approve
Authorization: Bearer {{appr_sales_token}}
HTTP 403

POST {{base}}/api/invoices/{{invoice_id}}/approve
Authorization: Bearer {{appr_eng_token}}
HTTP 200

POST {{base}}/api/invoices/{{invoice_id}}/approve
Authorization: Bearer {{appr_eng_token}}
HTTP 409
