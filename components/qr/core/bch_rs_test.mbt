///|
fn ec_bits(ec : @core.EcLevel) -> Int32 {
  match ec {
    @core.EcLevel::L => 1
    @core.EcLevel::M => 0
    @core.EcLevel::Q => 3
    @core.EcLevel::H => 2
  }
}

///|
fn mask_of(n : Int32) -> @core.Mask {
  match n {
    0 => @core.Mask::M0
    1 => @core.Mask::M1
    2 => @core.Mask::M2
    3 => @core.Mask::M3
    4 => @core.Mask::M4
    5 => @core.Mask::M5
    6 => @core.Mask::M6
    _ => @core.Mask::M7
  }
}

///|
fn polynomial_degree(value : Int32) -> Int32 {
  let mut degree = 0
  let mut v = value
  while v > 1 {
    v = v >> 1
    degree = degree + 1
  }
  degree
}

///|
/// Remainder of binary polynomial division (addition is XOR).
fn polynomial_remainder(value : Int32, generator : Int32) -> Int32 {
  let generator_degree = polynomial_degree(generator)
  let mut remainder = value
  while remainder != 0 && polynomial_degree(remainder) >= generator_degree {
    remainder = remainder ^
      (generator << (polynomial_degree(remainder) - generator_degree))
  }
  remainder
}

///|
test "format information is a valid BCH(15,5) codeword" {
  for
    ec in [
      @core.EcLevel::L,
      @core.EcLevel::M,
      @core.EcLevel::Q,
      @core.EcLevel::H,
    ] {
    for n in 0..<8 {
      let bits = @core.format_info(ec, mask_of(n))
      assert_true(bits >= 0 && bits < 0x8000)
      assert_eq((bits ^ 0x5412) >> 10, (ec_bits(ec) << 3) | n)
      assert_eq(polynomial_remainder(bits ^ 0x5412, 0x537), 0)
    }
  }
}

///|
test "format information reference values" {
  assert_eq(@core.format_info(@core.EcLevel::L, @core.Mask::M0), 0x77C4)
  assert_eq(@core.format_info(@core.EcLevel::M, @core.Mask::M0), 0x5412)
  assert_eq(@core.format_info(@core.EcLevel::Q, @core.Mask::M0), 0x355F)
  assert_eq(@core.format_info(@core.EcLevel::H, @core.Mask::M0), 0x1689)
}

///|
test "all 32 format words are distinct" {
  let values : Array[Int32] = []
  for
    ec in [
      @core.EcLevel::L,
      @core.EcLevel::M,
      @core.EcLevel::Q,
      @core.EcLevel::H,
    ] {
    for n in 0..<8 {
      values.push(@core.format_info(ec, mask_of(n)))
    }
  }
  for i in 0..<values.length() {
    for j in (i + 1)..<values.length() {
      assert_true(values[i] != values[j])
    }
  }
}

///|
test "version information is a valid BCH(18,6) codeword" {
  for v in 7..<=40 {
    let bits = @core.version_info(v).unwrap()
    assert_true(bits >= 0 && bits < 0x40000)
    assert_eq(bits >> 12, v)
    assert_eq(polynomial_remainder(bits, 0x1F25), 0)
  }
  assert_true(@core.version_info(1) is None)
  assert_true(@core.version_info(6) is None)
  assert_true(@core.version_info(0) is None)
  assert_true(@core.version_info(41) is None)
}

///|
test "version information reference values" {
  assert_eq(@core.version_info(7).unwrap(), 0x07C94)
  assert_eq(@core.version_info(8).unwrap(), 0x085BC)
  assert_eq(@core.version_info(40).unwrap(), 0x28C69)
}

///|
test "GF(256) multiplication identities" {
  let samples : Array[Byte] = [0, 1, 2, 3, 7, 128, 200, 255]
  for a in samples {
    assert_eq(@core.gf_mul(a, 0), 0)
    assert_eq(@core.gf_mul(a, 1), a)
    for b in samples {
      assert_eq(@core.gf_mul(a, b), @core.gf_mul(b, a))
    }
  }
  assert_eq(@core.gf_mul(2, 128), 29)
  assert_eq(@core.gf_mul(2, 2), 4)
  assert_eq(@core.gf_mul(128, 128), 0x13)
}

///|
test "Reed-Solomon parity for HELLO WORLD V1-M" {
  let data : Array[Byte] = [
    32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17,
  ]
  let parity = @core.rs_encode(data, 10)
  assert_eq(parity.length(), 10)
  let expected : Array[Byte] = [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]
  for i in 0..<10 {
    assert_eq(parity[i], expected[i])
  }
}

///|
test "Reed-Solomon of the all-zero message is all zero" {
  let data : Array[Byte] = Array::make(16, (0 : Byte))
  for nsym in [1, 10, 17, 30] {
    let parity = @core.rs_encode(data, nsym)
    assert_eq(parity.length(), nsym)
    for b in parity {
      assert_eq(b, 0)
    }
  }
}

///|
test "Reed-Solomon output length equals requested parity length" {
  let data : Array[Byte] = [1, 2, 3, 4, 5]
  for nsym in 0..<=30 {
    assert_eq(@core.rs_encode(data, nsym).length(), nsym)
  }
}
