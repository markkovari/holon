# feedback-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "member-a@example.test", "password": "correcthorse1", "role": "member"}
HTTP 201

POST {{base}}/register
{"email": "member-b@example.test", "password": "correcthorse2", "role": "member"}
HTTP 201

POST {{base}}/register
{"email": "pm@example.test", "password": "correcthorse3", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "member-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
member_a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "member-b@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
member_b_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "pm@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
pm_token: jsonpath "$.access_token"

POST {{base}}/api/posts
Authorization: Bearer {{member_a_token}}
{"title": "Dark mode", "description": "Please add a dark theme"}
HTTP 201
[Captures]
post_id: jsonpath "$.id"

# member-b votes
POST {{base}}/api/posts/{{post_id}}/vote
Authorization: Bearer {{member_b_token}}
HTTP 200
[Asserts]
jsonpath "$.votes" == 1

# a second vote from the same member is refused, not double-counted
POST {{base}}/api/posts/{{post_id}}/vote
Authorization: Bearer {{member_b_token}}
HTTP 409

GET {{base}}/api/posts
Authorization: Bearer {{member_a_token}}
HTTP 200
[Asserts]
jsonpath "$.posts" count == 1
jsonpath "$.posts[0].votes" == 1
jsonpath "$.posts[0].status" == "open"

# a member cannot change a post's status — admin only
POST {{base}}/api/posts/{{post_id}}/status
Authorization: Bearer {{member_a_token}}
{"status": "planned"}
HTTP 403

POST {{base}}/api/posts/{{post_id}}/status
Authorization: Bearer {{pm_token}}
{"status": "planned"}
HTTP 200

# an invalid status is refused
POST {{base}}/api/posts/{{post_id}}/status
Authorization: Bearer {{pm_token}}
{"status": "not-a-real-status"}
HTTP 400

# member-b (not the author) cannot delete member-a's post
DELETE {{base}}/api/posts/{{post_id}}
Authorization: Bearer {{member_b_token}}
HTTP 403

# the author can
DELETE {{base}}/api/posts/{{post_id}}
Authorization: Bearer {{member_a_token}}
HTTP 204

GET {{base}}/api/posts
Authorization: Bearer {{member_a_token}}
HTTP 200
[Asserts]
jsonpath "$.posts" count == 0

GET {{base}}/api/posts
HTTP 401
