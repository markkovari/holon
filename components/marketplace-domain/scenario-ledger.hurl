# PART 4 gate — ledger.rs alone. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "vendor@example.test", "password": "correcthorse1", "role": "vendor"}
HTTP 201

POST {{base}}/register
{"email": "admin@example.test", "password": "correcthorse2", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "vendor@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
vendor_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "admin@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"

GET {{base}}/me
Authorization: Bearer {{vendor_token}}
HTTP 200
[Captures]
vendor_subject: jsonpath "$.subject"

POST {{base}}/test/seed
{"vendor_a": "{{vendor_subject}}"}
HTTP 201

# a vendor with no postings at all has a zero balance, not an error
GET {{base}}/api/vendors/nobody-ever-heard-of/balance
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.balance" == 0

# only the vendor themself, or admin, may read their balance
GET {{base}}/api/vendors/{{vendor_subject}}/balance
Authorization: Bearer {{vendor_token}}
HTTP 200
[Asserts]
jsonpath "$.balance" == 900

GET {{base}}/api/vendors/{{vendor_subject}}/balance
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.balance" == 900

# platform ledger is admin-only
GET {{base}}/api/ledger/platform
Authorization: Bearer {{vendor_token}}
HTTP 403

GET {{base}}/api/ledger/platform
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.cash" == 1000
jsonpath "$.fees" == 100

# a vendor with nothing owed cannot be paid out
POST {{base}}/api/vendors/nobody-ever-heard-of/payout
Authorization: Bearer {{admin_token}}
HTTP 400

POST {{base}}/api/vendors/{{vendor_subject}}/payout
Authorization: Bearer {{admin_token}}
HTTP 200

# the payout zeroed it — a second one has nothing owed
POST {{base}}/api/vendors/{{vendor_subject}}/payout
Authorization: Bearer {{admin_token}}
HTTP 400

GET {{base}}/api/vendors/{{vendor_subject}}/balance
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.balance" == 0
