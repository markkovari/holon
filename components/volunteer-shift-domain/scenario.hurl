# volunteer:shift's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "coord@example.test", "roles": ["coordinator"], "scopes": ["shifts:write","shifts:read","signups:write"]}
HTTP 201
[Captures]
coord_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "vol-a@example.test", "roles": ["volunteer"], "scopes": ["shifts:read","signups:write"]}
HTTP 201
[Captures]
vol_a_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "vol-b@example.test", "roles": ["volunteer"], "scopes": ["shifts:read","signups:write"]}
HTTP 201
[Captures]
vol_b_token: jsonpath "$.token"

# a volunteer cannot create a shift (RBAC, coordinator-only)
POST {{base}}/api/shifts
Authorization: Bearer {{vol_a_token}}
{"title": "Saturday setup", "slots": 1}
HTTP 403

POST {{base}}/api/shifts
Authorization: Bearer {{coord_token}}
{"title": "Saturday setup", "slots": 1}
HTTP 201
[Captures]
shift_id: jsonpath "$.id"

POST {{base}}/api/shifts/{{shift_id}}/signup
Authorization: Bearer {{vol_a_token}}
HTTP 201
[Captures]
signup_id: jsonpath "$.signup_id"

# the one slot is taken
POST {{base}}/api/shifts/{{shift_id}}/signup
Authorization: Bearer {{vol_b_token}}
HTTP 409

# vol_b cannot cancel vol_a's signup (policy:guard row-level rule)
POST {{base}}/api/shifts/{{shift_id}}/signups/{{signup_id}}/cancel
Authorization: Bearer {{vol_b_token}}
HTTP 403

# vol_a cancels her own signup
POST {{base}}/api/shifts/{{shift_id}}/signups/{{signup_id}}/cancel
Authorization: Bearer {{vol_a_token}}
HTTP 200

# the freed slot is takeable
POST {{base}}/api/shifts/{{shift_id}}/signup
Authorization: Bearer {{vol_b_token}}
HTTP 201

# vol_a spends her weekly quota (3/week; 1 used above, cancelled but still
# counted since quota:meter tracks cumulative reservations, not net-of-cancel)
POST {{base}}/api/shifts
Authorization: Bearer {{coord_token}}
{"title": "extra 1", "slots": 5}
HTTP 201
[Captures]
extra1_id: jsonpath "$.id"

POST {{base}}/api/shifts/{{extra1_id}}/signup
Authorization: Bearer {{vol_a_token}}
HTTP 201

POST {{base}}/api/shifts
Authorization: Bearer {{coord_token}}
{"title": "extra 2", "slots": 5}
HTTP 201
[Captures]
extra2_id: jsonpath "$.id"

POST {{base}}/api/shifts/{{extra2_id}}/signup
Authorization: Bearer {{vol_a_token}}
HTTP 201

POST {{base}}/api/shifts
Authorization: Bearer {{coord_token}}
{"title": "one too many", "slots": 5}
HTTP 201
[Captures]
extra3_id: jsonpath "$.id"

POST {{base}}/api/shifts/{{extra3_id}}/signup
Authorization: Bearer {{vol_a_token}}
HTTP 429
