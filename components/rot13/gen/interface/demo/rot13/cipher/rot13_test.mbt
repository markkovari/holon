test "shifts lowercase by 13" {
  assert_eq(@cipher.rot13("abc"), "nop")
}

test "wraps past z" {
  assert_eq(@cipher.rot13("nop"), "abc")
}

test "preserves case" {
  assert_eq(@cipher.rot13("Hello"), "Uryyb")
}

test "leaves non letters alone" {
  assert_eq(@cipher.rot13("a1 b!z"), "n1 o!m")
}

test "is its own inverse" {
  assert_eq(@cipher.rot13(@cipher.rot13("The Quick Brown Fox 42!")), "The Quick Brown Fox 42!")
}
