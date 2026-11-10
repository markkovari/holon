///|
fn join_segments(segments : Array[Segment]) -> String {
  let sb = StringBuilder()
  for seg in segments {
    sb.write_string(seg.data_)
  }
  sb.to_string()
}

///|
fn candidate_modes(run : String) -> Array[Mode] {
  let mut numeric = true
  let mut alnum = true
  for c in run {
    if !accepts(Numeric, c) {
      numeric = false
    }
    if !accepts(Alphanumeric, c) {
      alnum = false
    }
  }
  if numeric {
    [Numeric, Alphanumeric, Byte]
  } else if alnum {
    [Alphanumeric, Byte]
  } else {
    [Byte]
  }
}

///|
fn brute_suffix_bits(
  runs : Array[String],
  index : Int32,
  version : Int32,
) -> Int32 {
  if index == runs.length() {
    0
  } else {
    let mut best = DP_INF
    for mode in candidate_modes(runs[index]) {
      let seg = Segment::new(mode, runs[index])
      let cost = 4 +
        char_count_bits(version, mode) +
        seg.data_bits() +
        brute_suffix_bits(runs, index + 1, version)
      if cost < best {
        best = cost
      }
    }
    best
  }
}

///|
fn brute_min_bits(text : String, version : Int32) -> Int32 {
  let chars = text.to_array()
  let n = chars.length()
  let mut best = DP_INF
  for mask in 0..<(1 << (n - 1)) {
    let runs : Array[String] = []
    let sb = StringBuilder()
    for i in 0..<n {
      sb.write_char(chars[i])
      if i == n - 1 || ((mask >> i) & 1) == 1 {
        runs.push(sb.to_string())
        sb.reset()
      }
    }
    let bits = brute_suffix_bits(runs, 0, version)
    if bits < best {
      best = bits
    }
  }
  best
}

///|
test "dynamic programming matches exhaustive search" {
  let cases = [
    "ABC123", "a1B2", "123abc", "HELLO1", "中A1", "12A45", "a1234567", "12345678",
  ]
  for text in cases {
    for version in [1, 10, 27] {
      let segments = optimal_segments(text, version)
      assert_eq(join_segments(segments), text)
      assert_eq(bits_needed(segments, version), brute_min_bits(text, version))
    }
  }
}

///|
test "optimal segmentation prefers one alphanumeric segment" {
  let segments = optimal_segments("ABC123", 1)
  assert_eq(segments.length(), 1)
  assert_eq(segments[0].mode(), Alphanumeric)
  assert_eq(segments[0].data(), "ABC123")
  assert_eq(bits_needed(segments, 1), 4 + 9 + 33)
  assert_true(
    bits_needed(segments, 1) < bits_needed(split_segments("ABC123"), 1),
  )
}

///|
test "optimal segmentation keeps pure runs in their best mode" {
  let numeric = optimal_segments("1234567890", 1)
  assert_eq(numeric.length(), 1)
  assert_eq(numeric[0].mode(), Numeric)
  let alnum = optimal_segments("HELLO WORLD", 1)
  assert_eq(alnum.length(), 1)
  assert_eq(alnum[0].mode(), Alphanumeric)
  let bytes = optimal_segments("hello", 1)
  assert_eq(bytes.length(), 1)
  assert_eq(bytes[0].mode(), Byte)
}

///|
test "optimal segmentation is stable within a version group" {
  for (lo, hi) in [(1, 9), (10, 26), (27, 40)] {
    for text in ["ABC123", "a1B2中", "1234567890", "https://a.cn/1"] {
      let seg_lo = optimal_segments(text, lo)
      let seg_hi = optimal_segments(text, hi)
      assert_eq(bits_needed(seg_lo, lo), bits_needed(seg_hi, hi))
    }
  }
}

///|
test "optimal segmentation reconstructs long mixed payloads" {
  let texts = [
    "订单1234567890-ABC/XYZ?x=1&y=2", "https://example.com/路径/to?q=QR1234567890",
    "A1B2C3D4E5F6G7H8I9J0", "0000011111222223333344444",
  ]
  for text in texts {
    for version in [1, 10, 27] {
      let segments = optimal_segments(text, version)
      assert_eq(join_segments(segments), text)
      for seg in segments {
        assert_true(seg.char_count() > 0)
      }
    }
  }
}

///|
test "optimal segmentation never costs more than greedy runs" {
  for
    text in [
      "ABC123", "a1B2c3", "HELLO123WORLD", "123456789a", "中文123ABC", "https://example.com/?q=1",
    ] {
    for version in [1, 10, 27] {
      let optimal = optimal_segments(text, version)
      let greedy = split_segments(text)
      assert_true(bits_needed(optimal, version) <= bits_needed(greedy, version))
    }
  }
}
