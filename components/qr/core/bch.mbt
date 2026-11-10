///|
/// BCH(15,5) format information, masked with 0x5412 as required by ISO 7.9.
///
/// BCH(15,5) 格式信息，按 ISO 7.9 要求与 0x5412 异或掩码。
pub fn format_info(ec : EcLevel, mask : Mask) -> Int32 {
  let ec_bits = match ec {
    L => 1
    M => 0
    Q => 3
    H => 2
  }
  let mask_bits = match mask {
    M0 => 0
    M1 => 1
    M2 => 2
    M3 => 3
    M4 => 4
    M5 => 5
    M6 => 6
    M7 => 7
  }
  let data = (ec_bits << 3) | mask_bits
  let mut rem = data << 10
  for i = 14; i >= 10; i = i - 1 {
    if ((rem >> i) & 1) == 1 {
      rem = rem ^ (0x537 << (i - 10))
    }
  }
  ((data << 10) | rem) ^ 0x5412
}

///|
/// BCH(18,6) version information for versions 7..40; `None` below 7.
///
/// BCH(18,6) 版本信息，适用于版本 7..40；低于 7 时返回 `None`。
pub fn version_info(version : Int32) -> Int32? {
  if version < 7 || version > 40 {
    None
  } else {
    let mut rem = version << 12
    for i = 17; i >= 12; i = i - 1 {
      if ((rem >> i) & 1) == 1 {
        rem = rem ^ (0x1F25 << (i - 12))
      }
    }
    Some((version << 12) | rem)
  }
}
