test "ulid length and Crockford charset" {
  let u = encode_ulid(1700000000000UL, 0x1234UL, 0x56789ABCDEF01234UL)
  assert_eq(u.length(), 26)
  for i = 0; i < 26; i = i + 1 {
    let c = u[i].to_int().unsafe_to_char()
    assert_true(crockford.contains(c.to_string()))
  }
}

test "ulid monotonic ordering within same millisecond" {
  let u1 = next_ulid(1000UL, 0UL, 10UL)
  let u2 = next_ulid(1000UL, 0UL, 10UL)
  let u3 = next_ulid(1000UL, 0UL, 10UL)
  assert_true(u1 < u2)
  assert_true(u2 < u3)
}

test "uuid_v4 formatting" {
  let b = FixedArray::make(16, b'\x00')
  for i = 0; i < 16; i = i + 1 {
    b[i] = i.to_byte()
  }
  let uuid = format_uuid_v4(b)
  assert_eq(uuid.length(), 36)
  assert_eq(uuid[8].to_int().unsafe_to_char(), '-')
  assert_eq(uuid[13].to_int().unsafe_to_char(), '-')
  assert_eq(uuid[18].to_int().unsafe_to_char(), '-')
  assert_eq(uuid[23].to_int().unsafe_to_char(), '-')
  // version 4 check: byte 6 high nibble is 4 -> character at index 14 is '4'
  assert_eq(uuid[14].to_int().unsafe_to_char(), '4')
  // variant 1 check: byte 8 high nibble has 0x80 set -> 8, 9, a, or b
  let c = uuid[19].to_int().unsafe_to_char()
  assert_true(c == '8' || c == '9' || c == 'a' || c == 'b')
}

test "nanoid length and alphabet" {
  let b = FixedArray::make(32, b'\x00')
  for i = 0; i < 32; i = i + 1 {
    b[i] = (i * 7).to_byte()
  }
  let nano = format_nanoid(b, (21).to_byte())
  assert_eq(nano.length(), 21)
}

test "short_code length and unambiguous alphabet" {
  let b = FixedArray::make(16, b'\x00')
  for i = 0; i < 16; i = i + 1 {
    b[i] = (i * 13).to_byte()
  }
  let code = format_short_code(b, (8).to_byte())
  assert_eq(code.length(), 8)
  assert_true(!code.contains("0"))
  assert_true(!code.contains("O"))
  assert_true(!code.contains("1"))
  assert_true(!code.contains("I"))
  assert_true(!code.contains("L"))
}
