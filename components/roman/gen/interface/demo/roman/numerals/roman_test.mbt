test "units" {
  assert_eq(@numerals.to_roman(1U), "I")
  assert_eq(@numerals.to_roman(3U), "III")
}

test "subtractive" {
  assert_eq(@numerals.to_roman(4U), "IV")
  assert_eq(@numerals.to_roman(9U), "IX")
  assert_eq(@numerals.to_roman(40U), "XL")
  assert_eq(@numerals.to_roman(90U), "XC")
  assert_eq(@numerals.to_roman(400U), "CD")
  assert_eq(@numerals.to_roman(900U), "CM")
}

test "composite" {
  assert_eq(@numerals.to_roman(2024U), "MMXXIV")
  assert_eq(@numerals.to_roman(3888U), "MMMDCCCLXXXVIII")
}

test "parse back" {
  assert_eq(@numerals.from_roman("IV"), 4U)
  assert_eq(@numerals.from_roman("MMXXIV"), 2024U)
  assert_eq(@numerals.from_roman("MMMDCCCLXXXVIII"), 3888U)
}

test "round trips" {
  let cases : Array[UInt] = [1U, 4U, 9U, 14U, 40U, 99U, 444U, 2024U, 3999U]
  for n in cases {
    assert_eq(@numerals.from_roman(@numerals.to_roman(n)), n)
  }
}
