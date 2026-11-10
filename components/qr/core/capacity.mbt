///|
/// ECC codewords per block, indexed by [ec][version]. Index 0 of each row is unused.
let ecc_codewords_per_block : Array[Array[Int32]] = [
  // L
  [
    -1, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28,
    28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    30,
  ],
  // M
  [
    -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26,
    26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28,
  ],
  // Q
  [
    -1, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26,
    30, 28, 30, 30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    30,
  ],
  // H
  [
    -1, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26,
    28, 30, 24, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    30,
  ],
]

///|
/// Number of error correction blocks, indexed by [ec][version].
let num_error_correction_blocks : Array[Array[Int32]] = [
  // L
  [
    -1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10,
    12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25,
  ],
  // M
  [
    -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17,
    18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
  ],
  // Q
  [
    -1, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23,
    23, 25, 27, 29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68,
  ],
  // H
  [
    -1, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25,
    34, 30, 32, 35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81,
  ],
]

///|
fn ec_index(ec : EcLevel) -> Int32 {
  match ec {
    L => 0
    M => 1
    Q => 2
    H => 3
  }
}

///|
/// Number of data + ECC modules available for codewords, divided by 8.
/// Derived from ISO Table 1; remainder bits are not counted.
///
/// 可用于码字的数据 + ECC 模块数除以 8。
/// 源自 ISO Table 1；不计余留位。
pub fn total_codewords(version : Int32) -> Int32 {
  @model.total_codewords(version)
}

///|
pub fn ecc_codewords(version : Int32, ec : EcLevel) -> Int32 {
  ecc_codewords_per_block[ec_index(ec)][version]
}

///|
pub fn block_count(version : Int32, ec : EcLevel) -> Int32 {
  num_error_correction_blocks[ec_index(ec)][version]
}

///|
pub fn data_codewords(version : Int32, ec : EcLevel) -> Int32 {
  total_codewords(version) -
  ecc_codewords(version, ec) * block_count(version, ec)
}

///|
pub fn data_capacity_bits(version : Int32, ec : EcLevel) -> Int32 {
  data_codewords(version, ec) * 8
}

///|
pub fn char_count_bits(version : Int32, mode : Mode) -> Int32 {
  @model.char_count_bits(version, char_mode(mode))
}

///|
fn char_mode(mode : Mode) -> @model.CharMode {
  match mode {
    Numeric => @model.CharMode::Numeric
    Alphanumeric => @model.CharMode::Alphanumeric
    Byte => @model.CharMode::Byte
  }
}

///|
/// Maximum number of characters of the given mode that fit in a version/EC pair.
///
/// 给定版本/纠错等级下，指定模式可容纳的最大字符数。
pub fn character_capacity(version : Int32, ec : EcLevel, mode : Mode) -> Int32 {
  let payload = data_capacity_bits(version, ec) -
    4 -
    char_count_bits(version, mode)
  match mode {
    Numeric => @model.numeric_capacity(payload)
    Alphanumeric => @model.alphanumeric_capacity(payload)
    Byte => @model.byte_capacity(payload)
  }
}

///|
pub fn Version::total_codewords(self : Version) -> Int32 {
  total_codewords(self.0)
}

///|
pub fn Version::data_codewords(self : Version, ec : EcLevel) -> Int32 {
  data_codewords(self.0, ec)
}

///|
pub fn Version::data_capacity_bits(self : Version, ec : EcLevel) -> Int32 {
  data_capacity_bits(self.0, ec)
}

///|
pub fn Version::char_count_bits(self : Version, mode : Mode) -> Int32 {
  char_count_bits(self.0, mode)
}

///|
pub fn Version::character_capacity(
  self : Version,
  ec : EcLevel,
  mode : Mode,
) -> Int32 {
  character_capacity(self.0, ec, mode)
}
