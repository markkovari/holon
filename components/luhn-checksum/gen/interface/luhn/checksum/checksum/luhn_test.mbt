test "known valid numbers pass" {
  assert_true(@checksum.is_valid("79927398713"))
  assert_true(@checksum.is_valid("4111111111111111"))
  assert_true(@checksum.is_valid("5500005555555559"))
  assert_true(@checksum.is_valid("0"))
}

test "known invalid numbers fail" {
  assert_false(@checksum.is_valid("79927398710"))
  assert_false(@checksum.is_valid("79927398711"))
  assert_false(@checksum.is_valid("79927398712"))
  assert_false(@checksum.is_valid("4111111111111112"))
}

test "empty string is not valid" {
  assert_false(@checksum.is_valid(""))
  assert_eq(@checksum.checksum_digit(""), None)
  assert_eq(@checksum.append_checksum(""), None)
}

test "non-digit character is not valid" {
  assert_false(@checksum.is_valid("799273987a3"))
  assert_false(@checksum.is_valid("abc"))
  assert_false(@checksum.is_valid("12 34"))
  assert_eq(@checksum.checksum_digit("abc"), None)
  assert_eq(@checksum.append_checksum("abc"), None)
}

test "checksum digit makes the number valid once appended" {
  assert_eq(@checksum.checksum_digit("7992739871"), Some(b'\x03'))
  assert_eq(@checksum.append_checksum("7992739871"), Some("79927398713"))
  assert_true(@checksum.is_valid(@checksum.append_checksum("7992739871").unwrap()))
}

test "parity is counted from the right not the left" {
  assert_eq(@checksum.checksum_digit("07992739871"), @checksum.checksum_digit("7992739871"))
}

test "doubling that overflows nine folds by subtracting nine" {
  assert_eq(@checksum.checksum_digit("9"), Some(b'\x01'))
  assert_true(@checksum.is_valid("91"))
}

test "zero is trivially valid" {
  assert_true(@checksum.is_valid("0"))
}
