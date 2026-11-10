// Tests for experiment-assign core

test "sanitize and unsanitize roundtrip" {
  let cases = [
    "simple",
    "with space",
    "colon:and,comma",
    "unicode-test",
    "under_score",
    "hyphen-dash=equals",
  ]
  for s in cases {
    let sanitized = sanitize(s)
    let unsanitized = unsanitize(sanitized)
    assert_eq(unsanitized, s)
  }
}

test "variants serialize and deserialize" {
  let variants : Array[(String, UInt)] = [
    ("control", 50U),
    ("variant-a", 25U),
    ("variant:b,special", 25U),
  ]
  let bytes = variants_to_bytes(variants)
  let parsed = variants_from_bytes(bytes)
  match parsed {
    Some(res) => {
      assert_eq(res.length(), variants.length())
      for i = 0; i < variants.length(); i = i + 1 {
        assert_eq(res[i].0, variants[i].0)
        assert_eq(res[i].1, variants[i].1)
      }
    }
    None => panic()
  }
}

test "pick is deterministic and sticky" {
  let variants : Array[(String, UInt)] = [
    ("control", 50U),
    ("test", 50U),
  ]
  let a1 = pick(variants, "user_123")
  let a2 = pick(variants, "user_123")
  assert_eq(a1, a2)

  let b1 = pick(variants, "user_456")
  let b2 = pick(variants, "user_456")
  assert_eq(b1, b2)
}

test "pick returns None when all weights zero" {
  let variants : Array[(String, UInt)] = [
    ("a", 0U),
    ("b", 0U),
  ]
  assert_eq(pick(variants, "user_1"), None)
}

test "exp_key format" {
  assert_eq(exp_key("", "signup-flow"), "ex_/signup-flow")
  assert_eq(exp_key("acme", "button-color"), "ex_acme/button-color")
  assert_eq(exp_key("", "signup_flow"), "ex_/signup_5Fflow")
}
