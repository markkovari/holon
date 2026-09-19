# ats-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "interviewer-a@example.test", "password": "correcthorse1", "role": "interviewer"}
HTTP 201
[Captures]
interviewer_a_subject: jsonpath "$.subject"

POST {{base}}/register
{"email": "interviewer-b@example.test", "password": "correcthorse2", "role": "interviewer"}
HTTP 201
[Captures]
interviewer_b_subject: jsonpath "$.subject"

POST {{base}}/register
{"email": "recruiter@example.test", "password": "correcthorse3", "role": "admin"}
HTTP 201

POST {{base}}/login
{"email": "interviewer-a@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
interviewer_a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "interviewer-b@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
interviewer_b_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "recruiter@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
recruiter_token: jsonpath "$.access_token"

# an interviewer cannot create a posting — admin only
POST {{base}}/api/postings
Authorization: Bearer {{interviewer_a_token}}
{"title": "Senior Engineer", "assigned_to": "{{interviewer_a_subject}}"}
HTTP 403

POST {{base}}/api/postings
Authorization: Bearer {{recruiter_token}}
{"title": "Senior Engineer", "assigned_to": "{{interviewer_a_subject}}"}
HTTP 201
[Captures]
posting_id: jsonpath "$.id"

# interviewer-a (assigned) sees the posting
GET {{base}}/api/postings
Authorization: Bearer {{interviewer_a_token}}
HTTP 200
[Asserts]
jsonpath "$.postings" count == 1

# interviewer-b (not assigned) does not
GET {{base}}/api/postings
Authorization: Bearer {{interviewer_b_token}}
HTTP 200
[Asserts]
jsonpath "$.postings" count == 0

# interviewer-b cannot add a candidate to a posting they are not assigned to
POST {{base}}/api/postings/{{posting_id}}/candidates
Authorization: Bearer {{interviewer_b_token}}
{"name": "Jamie Lee", "email": "jamie@example.test"}
HTTP 403

# interviewer-a (assigned) can
POST {{base}}/api/postings/{{posting_id}}/candidates
Authorization: Bearer {{interviewer_a_token}}
{"name": "Jamie Lee", "email": "jamie@example.test"}
HTTP 201
[Captures]
candidate_id: jsonpath "$.id"

GET {{base}}/api/postings/{{posting_id}}/candidates
Authorization: Bearer {{interviewer_a_token}}
HTTP 200
[Asserts]
jsonpath "$.candidates" count == 1
jsonpath "$.candidates[0].stage" == "applied"

# interviewer-a moves the candidate forward
POST {{base}}/api/candidates/{{candidate_id}}/stage
Authorization: Bearer {{interviewer_a_token}}
{"stage": "screening"}
HTTP 200

# an invalid stage is refused
POST {{base}}/api/candidates/{{candidate_id}}/stage
Authorization: Bearer {{interviewer_a_token}}
{"stage": "not-a-real-stage"}
HTTP 400

# admin may act on any posting despite not being assigned
POST {{base}}/api/candidates/{{candidate_id}}/stage
Authorization: Bearer {{recruiter_token}}
{"stage": "interview"}
HTTP 200

GET {{base}}/api/postings
HTTP 401
