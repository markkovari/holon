# inspection-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "admin-a@example.test", "password": "correcthorse1", "role": "admin"}
HTTP 201

POST {{base}}/register
{"email": "inspector-x@example.test", "password": "correcthorse2", "role": "inspector"}
HTTP 201
[Captures]
inspector_x_subject: jsonpath "$.subject"

POST {{base}}/register
{"email": "inspector-y@example.test", "password": "correcthorse3", "role": "inspector"}
HTTP 201

POST {{base}}/login
{"email": "admin-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
admin_a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "inspector-x@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
inspector_x_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "inspector-y@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
inspector_y_token: jsonpath "$.access_token"

# inspector-x files a passing inspection
POST {{base}}/api/inspections
Authorization: Bearer {{inspector_x_token}}
{"site_name": "Warehouse 4", "items": [{"label": "Fire extinguisher present", "pass": true, "note": ""}, {"label": "Exit signs lit", "pass": true, "note": ""}]}
HTTP 201
[Captures]
pass_id: jsonpath "$.id"
[Asserts]
jsonpath "$.overall_pass" == true
jsonpath "$.status" == "submitted"

# inspector-x files a failing inspection
POST {{base}}/api/inspections
Authorization: Bearer {{inspector_x_token}}
{"site_name": "Warehouse 5", "items": [{"label": "Fire extinguisher present", "pass": false, "note": "missing from bay 3"}, {"label": "Exit signs lit", "pass": true, "note": ""}]}
HTTP 201
[Captures]
fail_id: jsonpath "$.id"
[Asserts]
jsonpath "$.overall_pass" == false

# an inspection with no items is rejected
POST {{base}}/api/inspections
Authorization: Bearer {{inspector_x_token}}
{"site_name": "Empty site", "items": []}
HTTP 400

# inspector-x sees only their own two inspections
GET {{base}}/api/inspections
Authorization: Bearer {{inspector_x_token}}
HTTP 200
[Asserts]
jsonpath "$.inspections" count == 2

# inspector-y (filed nothing) sees none of inspector-x's inspections
GET {{base}}/api/inspections
Authorization: Bearer {{inspector_y_token}}
HTTP 200
[Asserts]
jsonpath "$.inspections" count == 0

# inspector-y cannot read inspector-x's inspection by id
GET {{base}}/api/inspections/{{pass_id}}
Authorization: Bearer {{inspector_y_token}}
HTTP 403

# inspector-y cannot download inspector-x's PDF either
GET {{base}}/api/inspections/{{pass_id}}/pdf
Authorization: Bearer {{inspector_y_token}}
HTTP 403

# the admin sees every inspection, not just their own (there are none of
# the admin's own — this is the row-level rule's OTHER half)
GET {{base}}/api/inspections
Authorization: Bearer {{admin_a_token}}
HTTP 200
[Asserts]
jsonpath "$.inspections" count == 2

# the admin can read inspector-x's inspection by id
GET {{base}}/api/inspections/{{fail_id}}
Authorization: Bearer {{admin_a_token}}
HTTP 200
[Asserts]
jsonpath "$.overall_pass" == false
jsonpath "$.site_name" == "Warehouse 5"

# inspector-x downloads their own inspection as a real PDF
GET {{base}}/api/inspections/{{pass_id}}/pdf
Authorization: Bearer {{inspector_x_token}}
HTTP 200
[Asserts]
header "Content-Type" == "application/pdf"
bytes count > 200
bytes startsWith hex,255044462d31; # "%PDF-1" — a real PDF file header, not a JSON error dressed up

# a nonexistent inspection is a 404, not a 403 or a 500
GET {{base}}/api/inspections/nonexistent-id
Authorization: Bearer {{inspector_x_token}}
HTTP 404

GET {{base}}/api/inspections
HTTP 401
