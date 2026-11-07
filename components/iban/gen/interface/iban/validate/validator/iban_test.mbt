test "valid dutch iban" {
  match @validator.validate("NL91 ABNA 0417 1643 00") {
    Ok(info) => {
      assert_eq(info.country, "NL")
      assert_eq(info.check_digits, "91")
      assert_eq(info.bban, "ABNA0417164300")
      assert_eq(info.formatted, "NL91 ABNA 0417 1643 00")
      assert_eq(info.length, 18U)
    }
    Err(_) => fail("expected valid IBAN")
  }
}

test "lowercase input normalizes and succeeds" {
  match @validator.validate("nl91 abna 0417 1643 00") {
    Ok(info) => {
      assert_eq(info.country, "NL")
      assert_eq(info.check_digits, "91")
      assert_eq(info.formatted, "NL91 ABNA 0417 1643 00")
    }
    Err(_) => fail("expected valid IBAN")
  }
}

test "checksum mismatch" {
  match @validator.validate("NL91 ABNA 0417 1643 01") {
    Err(BadCheck) => ()
    _ => fail("expected BadCheck")
  }
}

test "short length error" {
  match @validator.validate("DE8937040044") {
    Err(BadLength((12U, 22U))) => ()
    _ => fail("expected BadLength")
  }
}

test "bad country digits" {
  match @validator.validate("12 3456 7890") {
    Err(BadCountry("12")) => ()
    _ => fail("expected BadCountry")
  }
}

test "too short" {
  match @validator.validate("NL") {
    Err(TooShort) => ()
    _ => fail("expected TooShort")
  }
}

test "bad character" {
  match @validator.validate("NL91 ABNA 0417 1643 0!") {
    Err(BadChar("!")) => ()
    _ => fail("expected BadChar")
  }
}
