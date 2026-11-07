test "small" {
  assert_eq(@bignum.add("1", "2"), "3")
  assert_eq(@bignum.add("5", "5"), "10")
}

test "carry ripples" {
  assert_eq(@bignum.add("999", "1"), "1000")
  assert_eq(@bignum.add("1", "9999"), "10000")
}

test "zero" {
  assert_eq(@bignum.add("0", "0"), "0")
}

test "different lengths" {
  assert_eq(@bignum.add("12", "3456"), "3468")
}

test "beyond u128" {
  assert_eq(
    @bignum.add("123456789012345678901234567890", "987654321098765432109876543210"),
    "1111111110111111111011111111100",
  )
}
