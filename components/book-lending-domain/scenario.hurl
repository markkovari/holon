# book:lending's held-out-spec gate. `base` via --variable base=http://127.0.0.1:<port>.

POST {{base}}/test/token
{"subject": "lib@example.test", "roles": ["librarian"], "scopes": ["books:write","books:read","loans:write","loans:return"]}
HTTP 201
[Captures]
lib_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "alice@example.test", "roles": ["patron"], "scopes": ["books:read","loans:write","loans:return"]}
HTTP 201
[Captures]
alice_token: jsonpath "$.token"

POST {{base}}/test/token
{"subject": "bob@example.test", "roles": ["patron"], "scopes": ["books:read","loans:write","loans:return"]}
HTTP 201
[Captures]
bob_token: jsonpath "$.token"

POST {{base}}/api/books
Authorization: Bearer {{alice_token}}
{"title": "The Left Hand of Darkness"}
HTTP 403

POST {{base}}/api/books
Authorization: Bearer {{lib_token}}
{"title": "The Left Hand of Darkness"}
HTTP 201
[Captures]
book_id: jsonpath "$.id"
[Asserts]
jsonpath "$.call_number" startsWith "BK-"

POST {{base}}/api/books/{{book_id}}/borrow
Authorization: Bearer {{alice_token}}
HTTP 201
[Captures]
loan_id: jsonpath "$.id"

POST {{base}}/api/books/{{book_id}}/borrow
Authorization: Bearer {{bob_token}}
HTTP 409

# bob cannot return alice's loan (policy:guard row-level rule)
POST {{base}}/api/loans/{{loan_id}}/return
Authorization: Bearer {{bob_token}}
HTTP 403

POST {{base}}/api/loans/{{loan_id}}/return
Authorization: Bearer {{alice_token}}
HTTP 200

POST {{base}}/api/books/{{book_id}}/borrow
Authorization: Bearer {{bob_token}}
HTTP 201
[Captures]
loan2_id: jsonpath "$.id"

# librarian override returns bob's loan
POST {{base}}/api/loans/{{loan2_id}}/return
Authorization: Bearer {{lib_token}}
HTTP 200
