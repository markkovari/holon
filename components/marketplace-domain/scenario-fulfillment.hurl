# PART 2 gate — fulfillment.rs alone. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "buyer@example.test", "password": "correcthorse1", "role": "buyer"}
HTTP 201

POST {{base}}/register
{"email": "admin@example.test", "password": "correcthorse2", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "buyer@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
buyer_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "admin@example.test", "password": "correcthorse2"}
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
order_paid: jsonpath "$.order_ids[1]"
order_delivered: jsonpath "$.order_ids[2]"

# only the order's own buyer may pay — admin is not an exception here
POST {{base}}/api/orders/{{order_placed}}/pay
Authorization: Bearer {{admin_token}}
HTTP 403

POST {{base}}/api/orders/{{order_placed}}/pay
Authorization: Bearer {{buyer_token}}
HTTP 200

# shipping is admin/vendor, gated on the order being paid
POST {{base}}/api/orders/{{order_placed}}/ship
Authorization: Bearer {{admin_token}}
{"carrier": "UPS", "tracking": "1Z999"}
HTTP 200
[Captures]
shipment_id: jsonpath "$.id"

# already shipped — the order is no longer "paid"
POST {{base}}/api/orders/{{order_placed}}/ship
Authorization: Bearer {{admin_token}}
{"carrier": "UPS", "tracking": "1Z999"}
HTTP 409

POST {{base}}/api/shipments/{{shipment_id}}/deliver
Authorization: Bearer {{admin_token}}
HTTP 200

# a refund larger than the order total is refused
POST {{base}}/api/orders/{{order_paid}}/refund
Authorization: Bearer {{admin_token}}
{"amount": 2000}
HTTP 400

POST {{base}}/api/orders/{{order_paid}}/refund
Authorization: Bearer {{admin_token}}
{"amount": 1000}
HTTP 200

# a return needs the order delivered — order_paid is not
POST {{base}}/api/orders/{{order_paid}}/returns
Authorization: Bearer {{buyer_token}}
{"reason": "changed my mind"}
HTTP 409

POST {{base}}/api/orders/{{order_delivered}}/returns
Authorization: Bearer {{buyer_token}}
{"reason": "wrong item"}
HTTP 201
[Captures]
return_id: jsonpath "$.id"

POST {{base}}/api/returns/{{return_id}}/approve
Authorization: Bearer {{admin_token}}
HTTP 200

# already approved — terminal
POST {{base}}/api/returns/{{return_id}}/reject
Authorization: Bearer {{admin_token}}
HTTP 409
