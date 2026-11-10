///|
test "Segment::from_bytes preserves arbitrary binary payloads" {
  let raw = b"\xff\x00\x80\xfe"
  let seg = @core.Segment::from_bytes(raw)
  assert_eq(seg.mode(), @core.Mode::Byte)
  assert_eq(seg.char_count(), 4)
  assert_eq(seg.data_bits(), 32)
  assert_eq(seg.bytes(), raw)
}

///|
test "encode_bytes matches byte-mode text encoding" {
  let text = "hello 世界"
  let bytes = @utf8.encode(text)
  let a = @core.encode(text, mode=@core.Mode::Byte, mask=@core.Mask::M3)
  let b = @core.encode_bytes(bytes, mask=@core.Mask::M3)
  assert_eq(a.version().value(), b.version().value())
  assert_eq(a.to_bytes(), b.to_bytes())
}

///|
test "encode_bytes accepts non-UTF-8 data" {
  let qr = @core.encode_bytes(
    b"\xff\x00\x80\xfe\x7f",
    ec_level=@core.EcLevel::L,
    mask=@core.Mask::M0,
  )
  assert_eq(qr.version().value(), 1)
  assert_eq(qr.mask(), @core.Mask::M0)
  assert_eq(qr.size(), 21)
}

///|
test "encode_segments preserves custom ordering and modes" {
  let segments = [
    @core.Segment::new(@core.Mode::Numeric, "123456"),
    @core.Segment::new(@core.Mode::Byte, "abc"),
    @core.Segment::from_bytes(b"\xff\x00"),
  ]
  assert_eq(@core.bits_needed(segments, 1), 98)
  let qr = @core.encode_segments(segments, mask=@core.Mask::M5)
  assert_eq(qr.version().value(), 1)
  assert_eq(qr.mask(), @core.Mask::M5)
}

///|
test "single custom alphanumeric segment matches forced-mode encode" {
  let segments = [@core.Segment::new(@core.Mode::Alphanumeric, "HELLO WORLD")]
  let custom = @core.encode_segments(segments, mask=@core.Mask::M1)
  let forced = @core.encode(
    "HELLO WORLD",
    mode=@core.Mode::Alphanumeric,
    mask=@core.Mask::M1,
  )
  assert_eq(custom.to_bytes(), forced.to_bytes())
}

///|
test "empty byte and segment payloads raise EmptyInput" {
  let cases : Array[() -> Unit raise @core.QrError] = [
    () => ignore(@core.encode_bytes(b"")),
    () => ignore(@core.encode_segments([])),
    () => {
      ignore(@core.encode_segments([@core.Segment::new(@core.Mode::Byte, "")]))
    },
  ]
  for run in cases {
    let mut caught = false
    try run() catch {
      @core.QrError::EmptyInput => caught = true
      _ => ()
    } noraise {
      _ => ()
    }
    assert_true(caught)
  }
}

///|
test "invalid numeric custom segments are rejected" {
  let mut char_seen = ' '
  let mut mode_seen = @core.Mode::Byte
  try
    ignore(
      @core.encode_segments([@core.Segment::new(@core.Mode::Numeric, "12a")]),
    )
  catch {
    @core.QrError::InvalidCharacter(char~, mode~) => {
      char_seen = char
      mode_seen = mode
    }
    _ => ()
  } noraise {
    _ => ()
  }
  assert_eq(char_seen, 'a')
  assert_eq(mode_seen, @core.Mode::Numeric)
}

///|
test "custom segments honour version bounds and overflow errors" {
  let segments = [@core.Segment::new(@core.Mode::Byte, "hi")]
  let qr = @core.encode_segments(segments, min_version=5, max_version=9)
  assert_eq(qr.version().value(), 5)
  let mut caught = false
  try
    ignore(
      @core.encode_segments(
        [@core.Segment::new(@core.Mode::Byte, "too long for v1")],
        ec_level=@core.EcLevel::H,
        max_version=1,
      ),
    )
  catch {
    @core.QrError::DataTooLong(_) => caught = true
    _ => ()
  } noraise {
    _ => ()
  }
  assert_true(caught)
}

///|
test "automatic mode still reconstructs mixed payloads" {
  for
    text in [
      "ABC123", "a1B2c3", "订单1234567890", "https://example.com/a?b=1", "中文测试",
    ] {
    let qr = @core.encode(text)
    assert_eq(qr.size(), qr.version().value() * 4 + 17)
  }
}
