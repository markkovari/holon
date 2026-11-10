// Tests for pagination codec

test "a cursor tag binds the key and the payload" {
  let tag = checksum("k1", "offset:10")
  assert_eq(tag, checksum("k1", "offset:10"))
  assert_true(tag != checksum("k2", "offset:10"))
  assert_true(tag != checksum("k1", "offset:11"))
  assert_eq(tag.length(), 16)
  for i = 0; i < tag.length(); i = i + 1 {
    let c = tag[i]
    assert_true((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))
  }
}

test "verify accepts only the tag this key produces" {
  let tag = checksum("secret", "page:2")
  assert_true(verify("secret", "page:2", tag))
  assert_true(!verify("secret", "page:3", tag))
  assert_true(!verify("other", "page:2", tag))
}

test "a malformed tag is refused rather than trusted" {
  let tag = checksum("secret", "page:2")
  assert_true(!verify("secret", "page:2", ""))
  assert_true(!verify("secret", "page:2", "zz"))
  assert_true(!verify("secret", "page:2", tag[0:tag.length() - 1].to_owned()))
  assert_true(!verify("secret", "page:2", tag[0:tag.length() - 2].to_owned()))
  assert_true(!verify("secret", "page:2", tag + "00"))
}

test "encode and decode roundtrip" {
  let key = "test-secret-key"
  let cursor = encode_with_key("2026-11-10T12:00:00Z", "user_12345", true, key)
  assert_true(cursor.length() > 0)
  match decode_with_key(cursor, key) {
    Some((sort_key, last_id, forward)) => {
      assert_eq(sort_key, "2026-11-10T12:00:00Z")
      assert_eq(last_id, "user_12345")
      assert_eq(forward, true)
    }
    None => panic()
  }
}

test "tampered cursor is rejected" {
  let key = "test-secret-key"
  let cursor = encode_with_key("key1", "id1", false, key)
  match decode_with_key(cursor + "x", key) {
    None => ()
    Some(_) => panic()
  }
  match decode_with_key("malformed-cursor-without-dot", key) {
    None => ()
    Some(_) => panic()
  }
}

test "clamp limit" {
  match clamp_limit_with_max(0U, 100U) {
    None => ()
    Some(_) => panic()
  }
  match clamp_limit_with_max(50U, 100U) {
    Some(n) => assert_eq(n, 50U)
    None => panic()
  }
  match clamp_limit_with_max(200U, 100U) {
    Some(n) => assert_eq(n, 100U)
    None => panic()
  }
}
