test "arithmetic precedence" {
  assert_eq(@language.eval("1+2*3"), 7L)
  assert_eq(@language.eval("(1+2)*3"), 9L)
}

test "power is right associative" {
  assert_eq(@language.eval("2**3**2"), 512L)
  assert_eq(@language.eval("2**0"), 1L)
}

test "power binds tighter than unary minus" {
  assert_eq(@language.eval("-2**2"), -4L)
}

test "div mod left assoc" {
  assert_eq(@language.eval("6/2*3"), 9L)
  assert_eq(@language.eval("2*3%4"), 2L)
  assert_eq(@language.eval("10%3"), 1L)
}

test "subtraction left assoc" {
  assert_eq(@language.eval("100-10-5"), 85L)
}

test "comparisons below arithmetic" {
  assert_eq(@language.eval("2+3==5"), 1L)
  assert_eq(@language.eval("5>3"), 1L)
  assert_eq(@language.eval("3>5"), 0L)
}

test "comparison ops" {
  assert_eq(@language.eval("2<=2"), 1L)
  assert_eq(@language.eval("2!=3"), 1L)
  assert_eq(@language.eval("4>=5"), 0L)
}

test "logical" {
  assert_eq(@language.eval("1&&0"), 0L)
  assert_eq(@language.eval("1||0"), 1L)
  assert_eq(@language.eval("0||0||1"), 1L)
}

test "not is unary" {
  assert_eq(@language.eval("!0"), 1L)
  assert_eq(@language.eval("!5"), 0L)
  assert_eq(@language.eval("!1||1"), 1L)
}

test "precedence across levels" {
  assert_eq(@language.eval("1+2>2 && 3<4"), 1L)
}

test "unary and parens" {
  assert_eq(@language.eval("-(3+4)"), -7L)
}
