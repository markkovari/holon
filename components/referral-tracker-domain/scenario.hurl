# referral:tracker's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "rec-a@example.test", "roles": ["recruiter"], "scopes": ["postings:write","postings:read","referrals:write","postings:close"]}
HTTP 201
[Captures]
rec_a_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "rec-b@example.test", "roles": ["recruiter"], "scopes": ["postings:write","postings:read","referrals:write","postings:close"]}
HTTP 201
[Captures]
rec_b_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "emp@example.test", "roles": ["employee"], "scopes": ["postings:read","referrals:write"]}
HTTP 201
[Captures]
emp_token: jsonpath "$.token"

POST {{base}}/api/postings
Authorization: Bearer {{emp_token}}
{"title": "Senior Engineer"}
HTTP 403

POST {{base}}/api/postings
Authorization: Bearer {{rec_a_token}}
{"title": "Senior Engineer"}
HTTP 201
[Captures]
posting_id: jsonpath "$.id"

POST {{base}}/api/postings/{{posting_id}}/referrals
Authorization: Bearer {{emp_token}}
{"candidate_email": "cand@example.test"}
HTTP 201

# a DIFFERENT recruiter can never close it — no override rule, on purpose
POST {{base}}/api/postings/{{posting_id}}/close
Authorization: Bearer {{rec_b_token}}
HTTP 403

POST {{base}}/api/postings/{{posting_id}}/close
Authorization: Bearer {{rec_a_token}}
HTTP 200
