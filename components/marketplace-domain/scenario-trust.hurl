# PART 3 gate — trust.rs alone. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "buyer@example.test", "password": "correcthorse1", "role": "buyer"}
HTTP 201

POST {{base}}/register
{"email": "buyer2@example.test", "password": "correcthorse2", "role": "buyer"}
HTTP 201

POST {{base}}/register
{"email": "admin@example.test", "password": "correcthorse3", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "buyer@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
buyer_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "buyer2@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
buyer2_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "admin@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"

GET {{base}}/me
Authorization: Bearer {{buyer_token}}
HTTP 200
[Captures]
buyer_subject: jsonpath "$.subject"

POST {{base}}/test/seed
{"buyer": "{{buyer_subject}}"}
HTTP 201
[Captures]
order_placed: jsonpath "$.order_ids[0]"
order_delivered: jsonpath "$.order_ids[2]"

# only the order's own buyer may open a dispute
POST {{base}}/api/orders/{{order_placed}}/disputes
Authorization: Bearer {{buyer2_token}}
{"reason": "not mine to dispute"}
HTTP 403

POST {{base}}/api/orders/{{order_placed}}/disputes
Authorization: Bearer {{buyer_token}}
{"reason": "item never arrived"}
HTTP 201
[Captures]
dispute_id: jsonpath "$.id"
[Asserts]
jsonpath "$.jev_recommendation" exists
jsonpath "$.jev_confidence" exists

# already delivered — the buyer's only path from here is a return, not a dispute
POST {{base}}/api/orders/{{order_delivered}}/disputes
Authorization: Bearer {{buyer_token}}
{"reason": "too late now"}
HTTP 409

GET {{base}}/api/disputes
Authorization: Bearer {{buyer2_token}}
HTTP 200
[Asserts]
jsonpath "$.disputes" count == 0

GET {{base}}/api/disputes
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.disputes" count == 1

GET {{base}}/api/disputes
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.disputes" count >= 1

# only admin resolves
POST {{base}}/api/disputes/{{dispute_id}}/resolve
Authorization: Bearer {{buyer_token}}
{"resolution": "refund"}
HTTP 403

POST {{base}}/api/disputes/{{dispute_id}}/resolve
Authorization: Bearer {{admin_token}}
{"resolution": "not-a-real-resolution"}
HTTP 400

POST {{base}}/api/disputes/{{dispute_id}}/resolve
Authorization: Bearer {{admin_token}}
{"resolution": "refund"}
HTTP 200
[Asserts]
jsonpath "$.resolution" == "refund"

GET {{base}}/api/vendors/usr_vendor_a/reputation
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.vendor" == "usr_vendor_a"
jsonpath "$.dispute_rate" exists
jsonpath "$.order_count" exists
jsonpath "$.fraud_flags" exists
