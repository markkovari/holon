///|
/// Character mode used by capacity helpers. Mirrors `@core.Mode` without
/// creating a dependency cycle.
///
/// 容量辅助函数使用的字符模式，与 `@core.Mode` 对应以避免循环依赖。
pub(all) enum CharMode {
  Numeric
  Alphanumeric
  Byte
}

///|
/// Number of data + ECC modules available for codewords, divided by 8.
/// Derived from ISO Table 1; remainder bits are not counted.
///
/// 可用于码字的数据 + ECC 模块数除以 8。
/// 源自 ISO Table 1；不计余留位。
pub fn total_codewords(
  version : Int32,
) -> Int32 where {
  proof_require: 1 <= version && version <= 40,
  proof_ensure: result => 26 <= result && result <= 3706,
} {
  let mut result = (16 * version + 128) * version + 64
  if version >= 2 {
    let num_align = version / 7 + 2
    result = result - ((25 * num_align - 10) * num_align - 55)
    if version >= 7 {
      result = result - 36
    }
  }
  result / 8
}

///|
/// Bits used by the character-count indicator for a version and mode.
///
/// 指定版本与模式下字符计数指示符所占的位数。
pub fn char_count_bits(
  version : Int32,
  mode : CharMode,
) -> Int32 where {
  proof_require: 1 <= version && version <= 40,
  proof_ensure: result => 8 <= result && result <= 16,
} {
  match mode {
    Numeric => if version <= 9 { 10 } else if version <= 26 { 12 } else { 14 }
    Alphanumeric =>
      if version <= 9 {
        9
      } else if version <= 26 {
        11
      } else {
        13
      }
    Byte => if version <= 9 { 8 } else { 16 }
  }
}

///|
pub fn numeric_capacity(
  bits : Int32,
) -> Int32 where {
  proof_require: 0 <= bits && bits <= 65536,
  proof_ensure: result => 0 <= result && result * 10 <= bits * 3,
} {
  if bits <= 0 {
    0
  } else {
    bits / 10 * 3 +
    ({
      let rem = bits % 10
      if rem >= 7 {
        2
      } else if rem >= 4 {
        1
      } else {
        0
      }
    })
  }
}

///|
pub fn alphanumeric_capacity(bits : Int32) -> Int32 {
  if bits <= 0 {
    0
  } else {
    bits / 11 * 2 + (if bits % 11 >= 6 { 1 } else { 0 })
  }
}

///|
pub fn byte_capacity(
  bits : Int32,
) -> Int32 where {
  proof_require: 0 <= bits && bits <= 65536,
  proof_ensure: result => result == bits / 8 && result >= 0,
} {
  if bits <= 0 {
    0
  } else {
    bits / 8
  }
}
