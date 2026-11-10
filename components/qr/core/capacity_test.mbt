///|
fn raw_data_modules(version : Int32) -> Int32 {
  let mut result = (16 * version + 128) * version + 64
  if version >= 2 {
    let num_align = version / 7 + 2
    result = result - ((25 * num_align - 10) * num_align - 55)
    if version >= 7 {
      result = result - 36
    }
  }
  result
}

///|
test "total codewords follow the symbol size formula" {
  assert_eq(@core.total_codewords(1), 26)
  assert_eq(@core.total_codewords(2), 44)
  assert_eq(@core.total_codewords(7), 196)
  assert_eq(@core.total_codewords(40), 3706)
  for v in 1..<=40 {
    assert_eq(@core.total_codewords(v), raw_data_modules(v) / 8)
  }
}

///|
test "version 1 and version 40 capacity facts" {
  assert_eq(@core.data_codewords(1, @core.EcLevel::L), 19)
  assert_eq(@core.data_codewords(1, @core.EcLevel::M), 16)
  assert_eq(@core.data_codewords(1, @core.EcLevel::Q), 13)
  assert_eq(@core.data_codewords(1, @core.EcLevel::H), 9)
  assert_eq(@core.ecc_codewords(1, @core.EcLevel::L), 7)
  assert_eq(@core.ecc_codewords(1, @core.EcLevel::M), 10)
  assert_eq(@core.ecc_codewords(1, @core.EcLevel::Q), 13)
  assert_eq(@core.ecc_codewords(1, @core.EcLevel::H), 17)
  assert_eq(@core.block_count(1, @core.EcLevel::H), 1)
  assert_eq(
    @core.character_capacity(1, @core.EcLevel::L, @core.Mode::Numeric),
    41,
  )
  assert_eq(
    @core.character_capacity(1, @core.EcLevel::L, @core.Mode::Alphanumeric),
    25,
  )
  assert_eq(@core.character_capacity(1, @core.EcLevel::L, @core.Mode::Byte), 17)
  assert_eq(
    @core.character_capacity(1, @core.EcLevel::H, @core.Mode::Numeric),
    17,
  )
  assert_eq(
    @core.character_capacity(1, @core.EcLevel::H, @core.Mode::Alphanumeric),
    10,
  )
  assert_eq(@core.character_capacity(1, @core.EcLevel::H, @core.Mode::Byte), 7)
  assert_eq(
    @core.character_capacity(40, @core.EcLevel::L, @core.Mode::Numeric),
    7089,
  )
  assert_eq(
    @core.character_capacity(40, @core.EcLevel::L, @core.Mode::Alphanumeric),
    4296,
  )
  assert_eq(
    @core.character_capacity(40, @core.EcLevel::L, @core.Mode::Byte),
    2953,
  )
  assert_eq(
    @core.character_capacity(40, @core.EcLevel::H, @core.Mode::Byte),
    1273,
  )
}

///|
test "every version has more capacity than its predecessor" {
  for
    e in [
      @core.EcLevel::L,
      @core.EcLevel::M,
      @core.EcLevel::Q,
      @core.EcLevel::H,
    ] {
    for m in [@core.Mode::Numeric, @core.Mode::Alphanumeric, @core.Mode::Byte] {
      for v in 1..<40 {
        assert_true(
          @core.character_capacity(v, e, m) <
          @core.character_capacity(v + 1, e, m),
        )
        assert_true(
          @core.data_capacity_bits(v, e) < @core.data_capacity_bits(v + 1, e),
        )
      }
    }
  }
}

///|
test "stronger EC levels hold less data" {
  let levels = [
    @core.EcLevel::L,
    @core.EcLevel::M,
    @core.EcLevel::Q,
    @core.EcLevel::H,
  ]
  for v in 1..<=40 {
    for i in 0..<3 {
      assert_true(
        @core.data_capacity_bits(v, levels[i]) >
        @core.data_capacity_bits(v, levels[i + 1]),
      )
    }
  }
}

///|
test "data codewords and ECC codewords add up to total codewords" {
  for v in 1..<=40 {
    for
      e in [
        @core.EcLevel::L,
        @core.EcLevel::M,
        @core.EcLevel::Q,
        @core.EcLevel::H,
      ] {
      let accounted = @core.data_codewords(v, e) +
        @core.ecc_codewords(v, e) * @core.block_count(v, e)
      assert_eq(accounted, @core.total_codewords(v))
      assert_eq(@core.data_capacity_bits(v, e), @core.data_codewords(v, e) * 8)
      let byte_bits = 4 + @core.char_count_bits(v, @core.Mode::Byte)
      assert_eq(
        @core.character_capacity(v, e, @core.Mode::Byte),
        (@core.data_capacity_bits(v, e) - byte_bits) / 8,
      )
    }
  }
}

///|
test "character count field widths" {
  assert_eq(@core.char_count_bits(1, @core.Mode::Numeric), 10)
  assert_eq(@core.char_count_bits(10, @core.Mode::Numeric), 12)
  assert_eq(@core.char_count_bits(27, @core.Mode::Numeric), 14)
  assert_eq(@core.char_count_bits(1, @core.Mode::Alphanumeric), 9)
  assert_eq(@core.char_count_bits(10, @core.Mode::Alphanumeric), 11)
  assert_eq(@core.char_count_bits(27, @core.Mode::Alphanumeric), 13)
  assert_eq(@core.char_count_bits(1, @core.Mode::Byte), 8)
  assert_eq(@core.char_count_bits(10, @core.Mode::Byte), 16)
}

///|
test "version constructor rejects out-of-range values" {
  assert_true(@core.Version::new(1) is Some(_))
  assert_true(@core.Version::new(40) is Some(_))
  assert_true(@core.Version::new(0) is None)
  assert_true(@core.Version::new(41) is None)
  assert_true(@core.Version::new(-1) is None)
  let v = @core.Version::new(40).unwrap()
  assert_eq(v.size(), 177)
  assert_eq(v.total_codewords(), 3706)
}

///|
test "Version methods forward to table helpers" {
  let v = @core.Version::new(10).unwrap()
  let ec = @core.EcLevel::Q
  assert_eq(v.total_codewords(), @core.total_codewords(10))
  assert_eq(v.data_codewords(ec), @core.data_codewords(10, ec))
  assert_eq(v.data_capacity_bits(ec), @core.data_capacity_bits(10, ec))
  assert_eq(
    v.char_count_bits(@core.Mode::Byte),
    @core.char_count_bits(10, @core.Mode::Byte),
  )
  assert_eq(
    v.character_capacity(ec, @core.Mode::Alphanumeric),
    @core.character_capacity(10, ec, @core.Mode::Alphanumeric),
  )
}
