test "adds" {
  assert_eq(@arith.eval("1+2"), 3L)
}

test "precedence" {
  assert_eq(@arith.eval("2*3+4"), 10L)
  assert_eq(@arith.eval("2+3*4"), 14L)
}

test "parens" {
  assert_eq(@arith.eval("(2+3)*4"), 20L)
  assert_eq(@arith.eval("((1+2))*3"), 9L)
}

test "left assoc" {
  assert_eq(@arith.eval("10-2-3"), 5L)
  assert_eq(@arith.eval("100/5/2"), 10L)
}

test "unary minus" {
  assert_eq(@arith.eval("-5+3"), -2L)
  assert_eq(@arith.eval("2*-3"), -6L)
}

test "whitespace" {
  assert_eq(@arith.eval(" 7 * ( 6 + 1 ) "), 49L)
}

test "mixed" {
  assert_eq(@arith.eval("20/(2+3)"), 4L)
  assert_eq(@arith.eval("2*3*4"), 24L)
}
