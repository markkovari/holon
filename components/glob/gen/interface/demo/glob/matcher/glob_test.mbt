test "literal" {
  assert_eq(@matcher.matches("abc", "abc"), true)
  assert_eq(@matcher.matches("abc", "abd"), false)
}

test "question" {
  assert_eq(@matcher.matches("a?c", "abc"), true)
  assert_eq(@matcher.matches("a?c", "ac"), false)
}

test "star" {
  assert_eq(@matcher.matches("a*c", "abbbc"), true)
  assert_eq(@matcher.matches("a*c", "ac"), true)
  assert_eq(@matcher.matches("*", "anything"), true)
}

test "suffix" {
  assert_eq(@matcher.matches("*.txt", "file.txt"), true)
  assert_eq(@matcher.matches("*.txt", "file.md"), false)
}

test "class" {
  assert_eq(@matcher.matches("[abc]d", "bd"), true)
  assert_eq(@matcher.matches("[abc]d", "dd"), false)
}

test "range" {
  assert_eq(@matcher.matches("[a-z]", "m"), true)
  assert_eq(@matcher.matches("[a-z]", "M"), false)
}

test "many stars" {
  assert_eq(@matcher.matches("a*b*c", "axxbyyc"), true)
  assert_eq(@matcher.matches("a*b*c", "axxc"), false)
}

test "empties" {
  assert_eq(@matcher.matches("", ""), true)
  assert_eq(@matcher.matches("a", ""), false)
  assert_eq(@matcher.matches("*", ""), true)
}
