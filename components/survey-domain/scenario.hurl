# survey-domain's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

GET {{base}}/health
HTTP 200

POST {{base}}/register
{"email": "admin@example.test", "password": "correcthorse1", "role": "admin"}
HTTP 201

POST {{base}}/register
{"email": "respondent-a@example.test", "password": "correcthorse2", "role": "respondent"}
HTTP 201

POST {{base}}/register
{"email": "respondent-b@example.test", "password": "correcthorse3", "role": "respondent"}
HTTP 201

POST {{base}}/login
{"email": "admin@example.test", "password": "correcthorse1"}
HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "respondent-a@example.test", "password": "correcthorse2"}
HTTP 200
[Captures]
a_token: jsonpath "$.access_token"

POST {{base}}/login
{"email": "respondent-b@example.test", "password": "correcthorse3"}
HTTP 200
[Captures]
b_token: jsonpath "$.access_token"

# a non-admin cannot create a survey
POST {{base}}/api/surveys
Authorization: Bearer {{a_token}}
{"title": "Team lunch", "questions": ["Where should we eat?", "Any dietary restrictions?"]}
HTTP 403

# admin creates a survey with two questions
POST {{base}}/api/surveys
Authorization: Bearer {{admin_token}}
{"title": "Team lunch", "questions": ["Where should we eat?", "Any dietary restrictions?"]}
HTTP 201
[Captures]
survey_id: jsonpath "$.id"

# any authenticated user sees the open survey
GET {{base}}/api/surveys
Authorization: Bearer {{a_token}}
HTTP 200
[Asserts]
jsonpath "$.surveys" count == 1

# respondent-a answers
POST {{base}}/api/surveys/{{survey_id}}/responses
Authorization: Bearer {{a_token}}
{"answers": ["Pizza place", "None"]}
HTTP 201

# respondent-a cannot answer the same survey twice
POST {{base}}/api/surveys/{{survey_id}}/responses
Authorization: Bearer {{a_token}}
{"answers": ["Sushi", "None"]}
HTTP 409

# the wrong number of answers is refused
POST {{base}}/api/surveys/{{survey_id}}/responses
Authorization: Bearer {{b_token}}
{"answers": ["Just one answer"]}
HTTP 400

# a non-admin cannot see results
GET {{base}}/api/surveys/{{survey_id}}/results
Authorization: Bearer {{b_token}}
HTTP 403

# respondent-b answers for real
POST {{base}}/api/surveys/{{survey_id}}/responses
Authorization: Bearer {{b_token}}
{"answers": ["Tacos", "Vegetarian"]}
HTTP 201

# admin sees both responses
GET {{base}}/api/surveys/{{survey_id}}/results
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.response_count" == 2
jsonpath "$.responses" count == 2

# a non-admin cannot close a survey
POST {{base}}/api/surveys/{{survey_id}}/close
Authorization: Bearer {{a_token}}
HTTP 403

# admin closes it
POST {{base}}/api/surveys/{{survey_id}}/close
Authorization: Bearer {{admin_token}}
HTTP 200

# closing an already-closed survey is refused
POST {{base}}/api/surveys/{{survey_id}}/close
Authorization: Bearer {{admin_token}}
HTTP 400

# a closed survey refuses new responses
POST {{base}}/api/surveys/{{survey_id}}/responses
Authorization: Bearer {{b_token}}
{"answers": ["Too late", "None"]}
HTTP 400

GET {{base}}/api/surveys
HTTP 401
