// RFC 4226 test vectors

test "rfc 4226 hotp test vectors" {
  // Key: "12345678901234567890" in ASCII
  let key_str = "12345678901234567890"
  let key = FixedArray::make(20, b'\x00')
  for i = 0; i < 20; i = i + 1 {
    key[i] = key_str[i].to_int().to_byte()
  }

  let expected = [
    "755224",
    "287082",
    "359152",
    "969429",
    "338314",
    "254676",
    "287922",
    "162583",
    "399871",
    "520489",
  ]

  for i = 0; i < 10; i = i + 1 {
    match hotp(key, i.to_uint64(), 6) {
      Some(code) => assert_eq(code, expected[i])
      None => panic()
    }
  }
}

test "base32 encode and decode roundtrip" {
  let raw = FixedArray::make(10, b'\x00')
  for i = 0; i < 10; i = i + 1 {
    raw[i] = (i * 17 + 5).to_byte()
  }
  let encoded = base32_encode(raw)
  assert_eq(encoded.length(), 16)
  match base32_decode(encoded) {
    Some(decoded) => {
      assert_eq(decoded.length(), 10)
      for i = 0; i < 10; i = i + 1 {
        assert_eq(decoded[i], raw[i])
      }
    }
    None => panic()
  }
}

test "percent encode" {
  assert_eq(percent_encode("Hello World!"), "Hello%20World%21")
  assert_eq(percent_encode("user@example.com"), "user%40example.com")
  assert_eq(percent_encode("ABC-123_test.~"), "ABC-123_test.~")
}

test "constant time equality" {
  assert_true(constant_time_eq("123456", "123456"))
  assert_true(!constant_time_eq("123456", "123457"))
  assert_true(!constant_time_eq("123456", "12345"))
}
