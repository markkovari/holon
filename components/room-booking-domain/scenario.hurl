# room:booking's held-out-spec gate — real HTTP requests against a running,
# composed instance. `base` is passed in via `--variable base=http://127.0.0.1:<port>`.

# ---- mint tokens (the router's fixture, never the goal's own code) ----
POST {{base}}/test/token
{"subject": "admin@example.test", "roles": ["admin"], "scopes": ["rooms:write","rooms:read","bookings:write"]}
HTTP 201
[Captures]
admin_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "alice@example.test", "roles": ["member"], "scopes": ["rooms:read","bookings:write"]}
HTTP 201
[Captures]
alice_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "bob@example.test", "roles": ["member"], "scopes": ["rooms:read","bookings:write"]}
HTTP 201
[Captures]
bob_token: jsonpath "$.token"

# ---- a member cannot create a room (RBAC, admin-only) ----
POST {{base}}/api/rooms
Authorization: Bearer {{alice_token}}
{"name": "Ada"}
HTTP 403

# ---- admin creates the room ----
POST {{base}}/api/rooms
Authorization: Bearer {{admin_token}}
{"name": "Ada"}
HTTP 201
[Captures]
room_id: jsonpath "$.id"

# ---- alice books it ----
POST {{base}}/api/rooms/{{room_id}}/book
Authorization: Bearer {{alice_token}}
{"date": "2030-01-01"}
HTTP 201
[Captures]
booking_id: jsonpath "$.id"

# ---- bob double-books the same room/date ----
POST {{base}}/api/rooms/{{room_id}}/book
Authorization: Bearer {{bob_token}}
{"date": "2030-01-01"}
HTTP 409

# ---- bob cannot cancel alice's booking (policy:guard row-level rule) ----
POST {{base}}/api/bookings/{{booking_id}}/cancel
Authorization: Bearer {{bob_token}}
HTTP 403

# ---- alice cancels her own booking ----
POST {{base}}/api/bookings/{{booking_id}}/cancel
Authorization: Bearer {{alice_token}}
HTTP 200

# ---- bob books the now-free slot ----
POST {{base}}/api/rooms/{{room_id}}/book
Authorization: Bearer {{bob_token}}
{"date": "2030-01-01"}
HTTP 201

# ---- alice exceeds her daily booking quota (quota:meter, 2/day) ----
POST {{base}}/api/rooms/{{room_id}}/book
Authorization: Bearer {{alice_token}}
{"date": "2030-01-02"}
HTTP 201

POST {{base}}/api/rooms/{{room_id}}/book
Authorization: Bearer {{alice_token}}
{"date": "2030-01-03"}
HTTP 429
