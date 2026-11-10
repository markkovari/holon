///|
fn repeat_char(c : Char, n : Int32) -> String {
  let sb = StringBuilder()
  for _ in 0..<n {
    sb.write_char(c)
  }
  sb.to_string()
}

///|
test "default encode picks version 1, level M and a valid mask" {
  let qr = @core.encode("HELLO WORLD")
  assert_eq(qr.version().value(), 1)
  assert_eq(qr.ec_level(), @core.EcLevel::M)
  assert_true(mask_number(qr.mask()) >= 0 && mask_number(qr.mask()) <= 7)
  assert_eq(qr.size(), 21)
}

///|
test "automatic version selection chooses the smallest fitting version" {
  for length in [1, 10, 14, 15, 17, 18, 30, 100, 500, 2000] {
    let text = repeat_char('a', length)
    let qr = @core.encode(text, mode=@core.Mode::Byte)
    let expected = {
      let mut v = 1
      while v < 40 &&
            @core.character_capacity(v, @core.EcLevel::M, @core.Mode::Byte) <
            length {
        v = v + 1
      }
      v
    }
    assert_eq(qr.version().value(), expected)
  }
}

///|
test "minimum and maximum version bounds are honoured" {
  let qr = @core.encode("hi", min_version=5, max_version=9)
  assert_eq(qr.version().value(), 5)
  let exact = @core.encode("hi", min_version=40, max_version=40)
  assert_eq(exact.version().value(), 40)
  assert_eq(exact.size(), 177)
}

///|
test "overflowing the requested maximum version raises DataTooLong" {
  let text = repeat_char('a', 100)
  let mut caught = false
  try
    ignore(
      @core.encode(
        text,
        ec_level=@core.EcLevel::H,
        min_version=1,
        max_version=1,
      ),
    )
  catch {
    @core.QrError::DataTooLong(needed~, capacity~, version~, ec_level~) => {
      caught = true
      assert_true(needed > capacity)
      assert_eq(version, 1)
      assert_eq(ec_level, @core.EcLevel::H)
      assert_eq(capacity, 72)
    }
    _ => ()
  } noraise {
    _ => ()
  }
  assert_true(caught)
}

///|
test "empty input raises EmptyInput with a clear message" {
  let mut message = ""
  try ignore(@core.encode("")) catch {
    @core.QrError::EmptyInput => message = "empty"
    e => message = e.message()
  } noraise {
    _ => ()
  }
  assert_eq(message, "empty")
}

///|
test "invalid version ranges are rejected" {
  for (min_v, max_v) in [(0, 1), (41, 41), (5, 4), (1, 41)] {
    let mut caught = false
    try ignore(@core.encode("hi", min_version=min_v, max_version=max_v)) catch {
      @core.QrError::InvalidVersion(_) => caught = true
      _ => ()
    } noraise {
      _ => ()
    }
    assert_true(caught)
  }
}

///|
test "forced numeric mode rejects letters" {
  let mut char_seen = ' '
  let mut mode_seen = @core.Mode::Byte
  try ignore(@core.encode("12a34", mode=@core.Mode::Numeric)) catch {
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
test "segmentation reconstructs the input and distinguishes modes" {
  let cases : Array[(String, Array[@core.Mode])] = [
    ("1234567890", [@core.Mode::Numeric]),
    ("HELLO WORLD", [@core.Mode::Alphanumeric]),
    ("hello world", [@core.Mode::Byte]),
    ("中文测试", [@core.Mode::Byte]),
    ("ABC123", [@core.Mode::Alphanumeric, @core.Mode::Numeric]),
  ]
  for (text, modes) in cases {
    let segments = @core.split_segments(text)
    assert_eq(segments.length(), modes.length())
    let sb = StringBuilder()
    for i in 0..<segments.length() {
      assert_eq(segments[i].mode(), modes[i])
      sb.write_string(segments[i].data())
    }
    assert_eq(sb.to_string(), text)
  }
}

///|
test "bit counts match manual calculations" {
  let hello = @core.split_segments("HELLO WORLD")
  assert_eq(@core.bits_needed(hello, 1), 4 + 9 + 5 * 11 + 6)
  let number = @core.split_segments("01234567", mode=@core.Mode::Numeric)
  assert_eq(@core.bits_needed(number, 1), 4 + 10 + 10 + 10 + 7)
  let bytes = @core.split_segments("hi", mode=@core.Mode::Byte)
  assert_eq(@core.bits_needed(bytes, 1), 4 + 8 + 16)
  let mixed = @core.split_segments("abc123")
  assert_eq(mixed.length(), 2)
  assert_eq(@core.bits_needed(mixed, 1), 4 + 8 + 24 + (4 + 10 + 10))
}

///|
test "exact capacity succeeds and one more character fails" {
  let levels = [
    @core.EcLevel::L,
    @core.EcLevel::M,
    @core.EcLevel::Q,
    @core.EcLevel::H,
  ]
  for ec in levels {
    for
      (mode, c) in [
        (@core.Mode::Numeric, '1'),
        (@core.Mode::Alphanumeric, 'A'),
        (@core.Mode::Byte, 'a'),
      ] {
      let capacity = @core.character_capacity(1, ec, mode)
      let full = @core.encode(
        repeat_char(c, capacity),
        ec_level=ec,
        min_version=1,
        max_version=1,
        mode~,
      )
      assert_eq(full.version().value(), 1)
      let mut caught = false
      try
        ignore(
          @core.encode(
            repeat_char(c, capacity + 1),
            ec_level=ec,
            min_version=1,
            max_version=1,
            mode~,
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
  }
}

///|
test "encoding is deterministic and modules are immutable" {
  let a = @core.encode("deterministic payload", mask=@core.Mask::M3)
  let b = @core.encode("deterministic payload", mask=@core.Mask::M3)
  assert_eq(a.to_bytes(), b.to_bytes())
  let bytes = a.to_bytes()
  assert_eq(bytes.length(), a.size() * a.size())
  for byte in bytes {
    assert_true(byte == 0 || byte == 1)
  }
  let copy = a.modules()
  let original = a.get(0, 0)
  copy[0] = !copy[0]
  assert_eq(a.get(0, 0), original)
}

///|
test "DataTooLong message carries actionable diagnostics" {
  let text = repeat_char('a', 100)
  let mut message = ""
  try
    ignore(
      @core.encode(
        text,
        ec_level=@core.EcLevel::H,
        min_version=1,
        max_version=1,
      ),
    )
  catch {
    e => message = e.message()
  } noraise {
    _ => ()
  }
  assert_true(message.contains("data needs"))
  assert_true(message.contains("V1-H"))
  assert_true(message.contains("shorten"))
}
