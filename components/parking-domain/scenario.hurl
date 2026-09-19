# parking-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "admin@example.test", "password": "correcthorse1", "role": "admin"}
HTTP 201

POST {{base}}/register
{"email": "member-a@example.test", "password": "correcthorse2", "role": "member"}
HTTP 201

POST {{base}}/register
{"email": "member-b@example.test", "password": "correcthorse3", "role": "member"}
HTTP 201

POST {{base}}/login
{"email": "admin@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "member-a@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "member-b@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
b_token: jsonpath "$.access_token"

# a non-admin cannot create a spot
POST {{base}}/api/spots
Authorization: Bearer {{a_token}}
{"label": "A1"}
HTTP 403

# admin creates a spot
POST {{base}}/api/spots
Authorization: Bearer {{admin_token}}
{"label": "A1"}
HTTP 201
[Captures]
spot_id: jsonpath "$.id"

# any authenticated user sees the spot list
GET {{base}}/api/spots
Authorization: Bearer {{a_token}}
HTTP 200
[Asserts]
jsonpath "$.spots" count == 1

# member-a reserves the spot from 1000 to 2000
POST {{base}}/api/spots/{{spot_id}}/reservations
Authorization: Bearer {{a_token}}
{"start": 1000, "end": 2000}
HTTP 201
[Captures]
reservation_id: jsonpath "$.id"

# a reservation where end <= start is refused
POST {{base}}/api/spots/{{spot_id}}/reservations
Authorization: Bearer {{b_token}}
{"start": 5000, "end": 4000}
HTTP 400

# an ADJACENT reservation (2000-3000) does not overlap [1000,2000) and is allowed
POST {{base}}/api/spots/{{spot_id}}/reservations
Authorization: Bearer {{a_token}}
{"start": 2000, "end": 3000}
HTTP 201

# an OVERLAPPING reservation (1500-2500) conflicts with the first (1000-2000) and is refused
POST {{base}}/api/spots/{{spot_id}}/reservations
Authorization: Bearer {{b_token}}
{"start": 1500, "end": 2500}
HTTP 409

# member-b has made no successful reservation yet
GET {{base}}/api/reservations
Authorization: Bearer {{b_token}}
HTTP 200
[Asserts]
jsonpath "$.reservations" count == 0

# member-b cannot cancel member-a's reservation
POST {{base}}/api/reservations/{{reservation_id}}/cancel
Authorization: Bearer {{b_token}}
HTTP 403

# member-a cancels their own reservation
POST {{base}}/api/reservations/{{reservation_id}}/cancel
Authorization: Bearer {{a_token}}
HTTP 200

# cancelling an already-cancelled reservation is refused
POST {{base}}/api/reservations/{{reservation_id}}/cancel
Authorization: Bearer {{a_token}}
HTTP 400

# admin sees every reservation, including the cancelled one and the second active one
GET {{base}}/api/reservations
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.reservations" count == 2

GET {{base}}/api/spots
HTTP 401

# --- the lot visualization: 3 floors, 50 spots each, live occupancy, history ---

# only admin may seed the lot
POST {{base}}/api/lot/seed
Authorization: Bearer {{a_token}}
HTTP 403

# admin seeds it: exactly 150 spots, 3 floors of 50
POST {{base}}/api/lot/seed
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.total" == 150

# any authenticated user can view the lot
GET {{base}}/api/lot
Authorization: Bearer {{a_token}}
HTTP 200
[Asserts]
jsonpath "$.floors" count == 3
jsonpath "$.floors[0].floor" == 1
jsonpath "$.floors[0].spots" count == 50
jsonpath "$.floors[1].floor" == 2
jsonpath "$.floors[2].floor" == 3
# freshly seeded: nothing in the lot is taken yet
jsonpath "$.floors[0].spots[0].taken" == false
[Captures]
lot_spot_id: jsonpath "$.floors[0].spots[0].id"

# member-b reserves that lot spot for a window guaranteed to cover "now"
POST {{base}}/api/spots/{{lot_spot_id}}/reservations
Authorization: Bearer {{b_token}}
{"start": 0, "end": 9999999999}
HTTP 201
[Captures]
lot_reservation_id: jsonpath "$.id"

# the lot view now shows that exact spot as taken, and nothing else
GET {{base}}/api/lot
Authorization: Bearer {{a_token}}
HTTP 200
[Asserts]
jsonpath "$.floors[0].spots[0].taken" == true
jsonpath "$.floors[0].spots[1].taken" == false

# a non-admin cannot see a spot's history
GET {{base}}/api/spots/{{lot_spot_id}}/history
Authorization: Bearer {{a_token}}
HTTP 403

# admin sees the history: one reservation, the one just made
GET {{base}}/api/spots/{{lot_spot_id}}/history
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.reservations" count == 1
jsonpath "$.reservations[0].id" == "{{lot_reservation_id}}"
jsonpath "$.reservations[0].status" == "active"

# seeding again is a no-op — still exactly 150 spots, the reservation still stands
POST {{base}}/api/lot/seed
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.total" == 150

GET {{base}}/api/lot
Authorization: Bearer {{a_token}}
HTTP 200
[Asserts]
jsonpath "$.floors[0].spots" count == 50
jsonpath "$.floors[1].spots" count == 50
jsonpath "$.floors[2].spots" count == 50
jsonpath "$.floors[0].spots[0].taken" == true
