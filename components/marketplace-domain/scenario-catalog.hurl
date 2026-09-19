# PART 1 gate — catalog.rs alone. `base` via --variable base=http://127.0.0.1:<port>.

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

# a buyer cannot create a listing
POST {{base}}/api/listings
Authorization: Bearer {{buyer_token}}
{"title": "Keyboard", "description": "Mechanical, brown switches", "price": 4500, "stock": 10}
HTTP 403

# a vendor creates one — auto-labeled by Jev
POST {{base}}/api/listings
Authorization: Bearer {{vendor_token}}
{"title": "Keyboard", "description": "Mechanical, brown switches", "price": 4500, "stock": 10}
HTTP 201
[Captures]
listing_id: jsonpath "$.id"
[Asserts]
jsonpath "$.category" exists
jsonpath "$.category_confidence" exists

# empty title is refused
POST {{base}}/api/listings
Authorization: Bearer {{vendor_token}}
{"title": "", "description": "x", "price": 100, "stock": 1}
HTTP 400

GET {{base}}/api/listings
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.listings" count == 1

GET {{base}}/api/listings/{{listing_id}}
Authorization: Bearer {{buyer_token}}
HTTP 200

# only the owning vendor (or admin — untested here, no admin in this gate) may override the category
PATCH {{base}}/api/listings/{{listing_id}}/category
Authorization: Bearer {{buyer_token}}
{"category": "toys"}
HTTP 403

PATCH {{base}}/api/listings/{{listing_id}}/category
Authorization: Bearer {{vendor_token}}
{"category": "not-a-real-category"}
HTTP 400

PATCH {{base}}/api/listings/{{listing_id}}/category
Authorization: Bearer {{vendor_token}}
{"category": "toys"}
HTTP 200
[Asserts]
jsonpath "$.category" == "toys"
jsonpath "$.category_confidence" == 1000

POST {{base}}/api/listings/{{listing_id}}/delist
Authorization: Bearer {{vendor_token}}
HTTP 200

# delisted: 404 for a non-owner, still visible to the owning vendor
GET {{base}}/api/listings/{{listing_id}}
Authorization: Bearer {{buyer_token}}
HTTP 404

GET {{base}}/api/listings/{{listing_id}}
Authorization: Bearer {{vendor_token}}
HTTP 200

# a fresh listing for order placement
POST {{base}}/api/listings
Authorization: Bearer {{vendor_token}}
{"title": "Mouse", "description": "Wireless", "price": 1000, "stock": 3}
HTTP 201
[Captures]
mouse_id: jsonpath "$.id"

# oversell is refused
POST {{base}}/api/orders
Authorization: Bearer {{buyer_token}}
{"items": [{"listing_id": "{{mouse_id}}", "quantity": 10}]}
HTTP 409

# empty items is refused
POST {{base}}/api/orders
Authorization: Bearer {{buyer_token}}
{"items": []}
HTTP 400

POST {{base}}/api/orders
Authorization: Bearer {{buyer_token}}
{"items": [{"listing_id": "{{mouse_id}}", "quantity": 2}]}
HTTP 201
[Captures]
order_id: jsonpath "$.id"
[Asserts]
jsonpath "$.total" == 2000

GET {{base}}/api/orders
Authorization: Bearer {{buyer_token}}
HTTP 200
[Asserts]
jsonpath "$.orders" count == 1

GET {{base}}/api/orders/{{order_id}}
Authorization: Bearer {{buyer2_token}}
HTTP 403

GET {{base}}/api/orders/{{order_id}}
Authorization: Bearer {{buyer_token}}
HTTP 200
