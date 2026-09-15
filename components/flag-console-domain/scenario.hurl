# flag:console's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "admin@example.test", "roles": ["admin"], "scopes": ["services:write","services:read","toggle"]}
HTTP 201
[Captures]
admin_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "eng-b@example.test", "roles": ["engineer:billing"], "scopes": ["services:read","toggle"]}
HTTP 201
[Captures]
eng_billing_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "eng-s@example.test", "roles": ["engineer:search"], "scopes": ["services:read","toggle"]}
HTTP 201
[Captures]
eng_search_token: jsonpath "$.token"

POST {{base}}/api/services
Authorization: Bearer {{eng_billing_token}}
{"name": "billing"}
HTTP 403

POST {{base}}/api/services
Authorization: Bearer {{admin_token}}
{"name": "billing"}
HTTP 201

# wrong-service engineer cannot toggle (policy:guard row-level rule)
POST {{base}}/api/services/billing/flags/new-invoice-ui
Authorization: Bearer {{eng_search_token}}
{"enabled": true}
HTTP 403

# owning engineer toggles
POST {{base}}/api/services/billing/flags/new-invoice-ui
Authorization: Bearer {{eng_billing_token}}
{"enabled": true}
HTTP 200

# admin overrides any service
POST {{base}}/api/services/billing/flags/new-invoice-ui
Authorization: Bearer {{admin_token}}
{"enabled": false}
HTTP 200
