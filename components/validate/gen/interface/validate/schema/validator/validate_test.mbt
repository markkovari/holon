test "validate required and type" {
  let rules : Array[@validator.Rule] = [
    {
      field: "email",
      kind: @validator.Kind::EMAIL,
      required: true,
      min_len: 0U,
      max_len: 0U,
      min_value: None,
      max_value: None,
      one_of: [],
    },
    {
      field: "age",
      kind: @validator.Kind::INTEGER,
      required: true,
      min_len: 0U,
      max_len: 0U,
      min_value: Some(18.0),
      max_value: Some(120.0),
      one_of: [],
    },
  ]

  let valid_json = "{\"email\":\"alice@example.com\",\"age\":25}"
  let res1 = @validator.validate(valid_json, rules)
  assert_eq(res1.length(), 0)

  let missing_json = "{\"age\":25}"
  let res2 = @validator.validate(missing_json, rules)
  assert_eq(res2.length(), 1)
  assert_eq(res2[0].field, "email")
  assert_eq(res2[0].code, "required")

  let bad_email = "{\"email\":\"not-an-email\",\"age\":25}"
  let res3 = @validator.validate(bad_email, rules)
  assert_eq(res3.length(), 1)
  assert_eq(res3[0].field, "email")
  assert_eq(res3[0].code, "format")
}

test "validate one_of and uuid" {
  let rules : Array[@validator.Rule] = [
    {
      field: "id",
      kind: @validator.Kind::UUID,
      required: true,
      min_len: 0U,
      max_len: 0U,
      min_value: None,
      max_value: None,
      one_of: [],
    },
    {
      field: "status",
      kind: @validator.Kind::TEXT,
      required: true,
      min_len: 0U,
      max_len: 0U,
      min_value: None,
      max_value: None,
      one_of: ["active", "pending", "inactive"],
    },
  ]

  let ok_json = "{\"id\":\"12345678-1234-1234-1234-123456789abc\",\"status\":\"active\"}"
  let r1 = @validator.validate(ok_json, rules)
  assert_eq(r1.length(), 0)

  let bad_status = "{\"id\":\"12345678-1234-1234-1234-123456789abc\",\"status\":\"deleted\"}"
  let r2 = @validator.validate(bad_status, rules)
  assert_eq(r2.length(), 1)
  assert_eq(r2[0].code, "one-of")
}
