test "ones" {
  assert_eq(@suffix.ordinal(1U), "1st")
  assert_eq(@suffix.ordinal(2U), "2nd")
  assert_eq(@suffix.ordinal(3U), "3rd")
  assert_eq(@suffix.ordinal(4U), "4th")
}

test "the teens are all th" {
  assert_eq(@suffix.ordinal(11U), "11th")
  assert_eq(@suffix.ordinal(12U), "12th")
  assert_eq(@suffix.ordinal(13U), "13th")
}

test "tens and hundreds" {
  assert_eq(@suffix.ordinal(21U), "21st")
  assert_eq(@suffix.ordinal(22U), "22nd")
  assert_eq(@suffix.ordinal(23U), "23rd")
  assert_eq(@suffix.ordinal(101U), "101st")
  assert_eq(@suffix.ordinal(111U), "111th")
  assert_eq(@suffix.ordinal(113U), "113th")
}
