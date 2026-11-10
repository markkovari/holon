test "svg is wellformed and scalable" {
  match svg("https://example.com", Ecc::MEDIUM, 4U) {
    Ok(s) => {
      assert_true(s.starts_with("<svg"))
      assert_true(s.contains("viewBox=\"0 0"))
      assert_true(s.contains("<path fill=\"#000\""))
      assert_true(s.ends_with("</svg>"))
    }
    Err(_) => panic()
  }
}

test "matrix is square and nonempty" {
  match matrix("hi", Ecc::LOW) {
    Ok(j) => {
      assert_true(j.starts_with("{\"size\":"))
      assert_true(j.contains("true"))
    }
    Err(_) => panic()
  }
}

test "unicode renders blocks" {
  match unicode("hi", Ecc::LOW) {
    Ok(u) => {
      assert_true(u.contains("█") || u.contains("▀") || u.contains("▄"))
    }
    Err(_) => panic()
  }
}

test "higher ecc is denser" {
  match (matrix("payload", Ecc::LOW), matrix("payload", Ecc::HIGH)) {
    (Ok(low), Ok(high)) => {
      assert_true(high.length() >= low.length())
    }
    _ => panic()
  }
}

test "too long input errors" {
  let buf = StringBuilder::new()
  for i = 0; i < 8000; i = i + 1 {
    buf.write_char('x')
  }
  let huge = buf.to_string()
  match svg(huge, Ecc::HIGH, 2U) {
    Err(QrError::TooLong(_)) => ()
    _ => panic()
  }
}
