///|
test "error messages name the violated constraint" {
  assert_eq(@core.QrError::EmptyInput.message(), "input is empty")
  let version = @core.QrError::InvalidVersion(0).message()
  assert_true(version.contains("invalid QR version 0"))
  assert_true(version.contains("1..40"))
  let char = @core.QrError::InvalidCharacter(char='x', mode=@core.Mode::Numeric).message()
  assert_true(char.contains("'x'"))
  assert_true(char.contains("Numeric"))
  let color = @core.QrError::InvalidColor("#fff\n").message()
  assert_true(color.contains("#fff"))
  let internal = @core.QrError::Internal("invariant").message()
  assert_true(internal.contains("invariant"))
}

///|
test "DataTooLong message contains all diagnostics" {
  let err = @core.QrError::DataTooLong(
    needed=312,
    capacity=232,
    version=5,
    ec_level=@core.EcLevel::H,
  )
  let text = err.message()
  assert_true(text.contains("312"))
  assert_true(text.contains("232"))
  assert_true(text.contains("V5-H"))
  assert_true(text.contains("shorten"))
}
