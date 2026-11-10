///|
test "alphanumeric table accepts every ISO character" {
  let text = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:"
  let segments = @core.split_segments(text, mode=@core.Mode::Alphanumeric)
  assert_eq(segments.length(), 1)
  assert_true(segments[0].mode() == @core.Mode::Alphanumeric)
  assert_eq(segments[0].data(), text)
  assert_eq(segments[0].char_count(), 45)
  assert_eq(segments[0].data_bits(), 248)
  // Encoding exercises the 45-symbol value table on the write path.
  let qr = @core.encode(text, mode=@core.Mode::Alphanumeric)
  assert_true(qr.size() >= 21)
}

///|
test "segments merge adjacent runs of the same mode" {
  let segments = @core.split_segments("ABC123xyz")
  assert_eq(segments.length(), 3)
  assert_true(segments[0].mode() == @core.Mode::Alphanumeric)
  assert_true(segments[1].mode() == @core.Mode::Numeric)
  assert_true(segments[2].mode() == @core.Mode::Byte)
}

///|
test "emoji uses UTF-8 byte accounting" {
  let segments = @core.split_segments("😀")
  assert_eq(segments.length(), 1)
  assert_true(segments[0].mode() == @core.Mode::Byte)
  assert_eq(segments[0].char_count(), 4)
  assert_eq(segments[0].data_bits(), 32)
  let qr = @core.encode("😀", min_version=1, max_version=1)
  assert_eq(qr.size(), 21)
}

///|
test "short mixed runs are absorbed into byte segments" {
  // A one-character numeric run between byte runs is cheaper as byte data.
  let segments = @core.split_segments("a1b")
  assert_eq(segments.length(), 1)
  assert_true(segments[0].mode() == @core.Mode::Byte)
  assert_eq(segments[0].data(), "a1b")
}
