# PART 5 gate — messaging.rs alone. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "vendor@example.test", "password": "correcthorse1", "role": "vendor"}
HTTP 201

POST {{base}}/register
{"email": "buyer@example.test", "password": "correcthorse2", "role": "buyer"}
HTTP 201

POST {{base}}/register
{"email": "buyer2@example.test", "password": "correcthorse3", "role": "buyer"}
HTTP 201

POST {{base}}/login
{"email": "vendor@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
vendor_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "buyer@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
buyer_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "buyer2@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
buyer2_token: jsonpath "$.access_token"

GET {{base}}/me
Authorization: Bearer {{vendor_token}}
HTTP 200
[Captures]
vendor_subject: jsonpath "$.subject"

POST {{base}}/test/seed
{"vendor_a": "{{vendor_subject}}"}
HTTP 201
[Captures]
listing_id: jsonpath "$.listing_ids[0]"

POST {{base}}/api/listings/does-not-exist/inquiries
Authorization: Bearer {{buyer_token}}
{"body": "hello?"}
HTTP 404

POST {{base}}/api/listings/{{listing_id}}/inquiries
Authorization: Bearer {{buyer_token}}
{"body": "Is this still available?"}
HTTP 201
[Captures]
inquiry_id: jsonpath "$.id"

GET {{base}}/api/inquiries
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.inquiries" count == 1

GET {{base}}/api/inquiries
Authorization: Bearer {{vendor_token}}
HTTP 200
[Asserts]
jsonpath "$.inquiries" count == 1

GET {{base}}/api/inquiries
Authorization: Bearer {{buyer2_token}}
HTTP 200
[Asserts]
jsonpath "$.inquiries" count == 0

GET {{base}}/api/inquiries/{{inquiry_id}}/messages
Authorization: Bearer {{buyer2_token}}
HTTP 403

GET {{base}}/api/inquiries/{{inquiry_id}}/messages
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.messages" count == 1

POST {{base}}/api/inquiries/{{inquiry_id}}/messages
Authorization: Bearer {{buyer2_token}}
{"body": "butting in"}
HTTP 403

POST {{base}}/api/inquiries/{{inquiry_id}}/messages
Authorization: Bearer {{vendor_token}}
{"body": "Yes, still available!"}
HTTP 201

GET {{base}}/api/inquiries/{{inquiry_id}}/messages
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.messages" count == 2
jsonpath "$.messages[0].body" == "Is this still available?"
jsonpath "$.messages[1].body" == "Yes, still available!"
