# COMPOSITION gate — every part together, the real flow end to end.
# `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "vendor@example.test", "password": "correcthorse1", "role": "vendor"}
HTTP 201

POST {{base}}/register
{"email": "buyer@example.test", "password": "correcthorse2", "role": "buyer"}
HTTP 201

POST {{base}}/register
{"email": "admin@example.test", "password": "correcthorse3", "role": "admin"}
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
{"email": "admin@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"

GET {{base}}/me
Authorization: Bearer {{vendor_token}}
HTTP 200
[Captures]
vendor_subject: jsonpath "$.subject"

# a real listing, auto-labeled by Jev
POST {{base}}/api/listings
Authorization: Bearer {{vendor_token}}
{"title": "Desk lamp", "description": "Adjustable LED desk lamp", "price": 3000, "stock": 10}
HTTP 201
[Captures]
listing_id: jsonpath "$.id"

# a buyer asks about it before buying
POST {{base}}/api/listings/{{listing_id}}/inquiries
Authorization: Bearer {{buyer_token}}
{"body": "Does it dim?"}
HTTP 201
[Captures]
inquiry_id: jsonpath "$.id"

POST {{base}}/api/inquiries/{{inquiry_id}}/messages
Authorization: Bearer {{vendor_token}}
{"body": "Yes, three brightness levels."}
HTTP 201

# a real multi-item order (two units of the one listing)
POST {{base}}/api/orders
Authorization: Bearer {{buyer_token}}
{"items": [{"listing_id": "{{listing_id}}", "quantity": 2}]}
HTTP 201
[Captures]
order_id: jsonpath "$.id"
[Asserts]
jsonpath "$.total" == 6000

POST {{base}}/api/orders/{{order_id}}/pay
Authorization: Bearer {{buyer_token}}
HTTP 200

POST {{base}}/api/orders/{{order_id}}/ship
Authorization: Bearer {{admin_token}}
{"carrier": "UPS", "tracking": "1Z000"}
HTTP 200
[Captures]
shipment_id: jsonpath "$.id"

POST {{base}}/api/shipments/{{shipment_id}}/deliver
Authorization: Bearer {{admin_token}}
HTTP 200

GET {{base}}/api/orders/{{order_id}}
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.status" == "delivered"

# the sale is on the ledger: 6000 total, vendor gets 90%, platform keeps 10%
GET {{base}}/api/vendors/{{vendor_subject}}/balance
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.balance" == 5400

GET {{base}}/api/ledger/platform
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.fees" == 600

# now delivered, the buyer can request a return but not a dispute
POST {{base}}/api/orders/{{order_id}}/disputes
Authorization: Bearer {{buyer_token}}
{"reason": "too late, already delivered"}
HTTP 409

POST {{base}}/api/orders/{{order_id}}/returns
Authorization: Bearer {{buyer_token}}
{"reason": "changed my mind"}
HTTP 201
[Captures]
return_id: jsonpath "$.id"

POST {{base}}/api/returns/{{return_id}}/approve
Authorization: Bearer {{admin_token}}
HTTP 200

# the vendor can be paid out what the ledger says they're owed
POST {{base}}/api/vendors/{{vendor_subject}}/payout
Authorization: Bearer {{admin_token}}
HTTP 200

GET {{base}}/api/vendors/{{vendor_subject}}/balance
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.balance" == 0
