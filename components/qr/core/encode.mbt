///|
/// Splits data codewords into RS blocks, appends parity, and interleaves both.
///
/// 将数据码字拆分为 RS 块，追加校验字节，并交错排列。
fn interleave_blocks(
  data : Array[Byte],
  version : Int32,
  ec : EcLevel,
) -> Array[Byte] {
  let nblocks = block_count(version, ec)
  let ecc_len = ecc_codewords(version, ec)
  let raw_len = data.length()
  let short_len = raw_len / nblocks
  let num_short = nblocks - raw_len % nblocks
  let blocks : Array[Array[Byte]] = []
  let eccs : Array[Array[Byte]] = []
  let mut offset = 0
  for i in 0..<nblocks {
    let len = short_len + (if i < num_short { 0 } else { 1 })
    let block = Array::make(len, (0 : Byte))
    for j in 0..<len {
      block[j] = data[offset + j]
    }
    offset += len
    blocks.push(block)
    eccs.push(rs_encode(block, ecc_len))
  }
  let result : Array[Byte] = []
  let max_len = short_len + (if num_short < nblocks { 1 } else { 0 })
  for i in 0..<max_len {
    for block in blocks {
      if i < block.length() {
        result.push(block[i])
      }
    }
  }
  for i in 0..<ecc_len {
    for ecc in eccs {
      result.push(ecc[i])
    }
  }
  result
}

///|
fn make_qr(version : Int32, ec : EcLevel, mask : Mask, m : Matrix) -> QrCode {
  {
    version_: Version(version),
    ec_level_: ec,
    mask_: mask,
    size_: m.size,
    modules_: m.modules.copy(),
  }
}

///|
fn encode_at_version(
  segments : Array[Segment],
  version : Int32,
  ec : EcLevel,
  mask? : Mask,
) -> QrCode raise QrError {
  let capacity_bytes = data_codewords(version, ec)
  let buf = BitBuffer::new()
  for seg in segments {
    append_segment(buf, seg, version)
  }
  buf.pad_to(capacity_bytes)
  let codewords = interleave_blocks(buf.to_bytes(), version, ec)
  let base = build_matrix(version, codewords)
  match mask {
    Some(m) => make_qr(version, ec, m, apply_mask(base, m, ec))
    None => {
      let (best, best_mask) = choose_best_mask(base, ec)
      make_qr(version, ec, best_mask, best)
    }
  }
}

///|
/// Encodes `text` as a QR Model 2 symbol.
///
/// 将 `text` 编码为 QR Model 2 符号。
///
/// `ec_level` defaults to M, version range to 1..40, mode is chosen
/// automatically, and the mask is selected by minimizing the Annex J penalty.
///
/// `ec_level` 默认为 M，版本范围为 1..40，模式自动选择，
/// 掩码通过最小化 Annex J 惩罚分选出。
///
/// # Example
/// ```mbt check
/// test "encode hello world" {
///   let qr = @core.encode("HELLO WORLD") // V1-M
///   assert_eq(qr.size(), 21)
///   assert_eq(qr.version().value(), 1)
/// }
/// ```
pub fn encode(
  text : String,
  ec_level~ : EcLevel = EcLevel::M,
  min_version~ : Int32 = 1,
  max_version~ : Int32 = 40,
  mode? : Mode,
  mask? : Mask,
) -> QrCode raise QrError {
  if text.is_empty() {
    raise QrError::EmptyInput
  }
  match mode {
    Some(m) => {
      let segments = split_segments(text, mode=m)
      let version = choose_version(segments, ec_level, min_version, max_version)
      encode_at_version(segments, version, ec_level, mask?)
    }
    None => {
      let (version, segments) = choose_version_optimal(
        text, ec_level, min_version, max_version,
      )
      encode_at_version(segments, version, ec_level, mask?)
    }
  }
}

///|
/// Encodes arbitrary bytes as one Byte segment. Unlike `encode`, no UTF-8
/// interpretation is applied, so binary payloads are preserved exactly.
///
/// 将任意字节编码为单个 Byte 段。与 `encode` 不同，这里不做 UTF-8 解释，
/// 因此二进制载荷会被精确保留。
///
/// # Example
/// ```mbt check
/// test "encode raw bytes" {
///   let qr = @core.encode_bytes(b"\xff\x00\x80")
///   assert_eq(qr.version().value(), 1)
/// }
/// ```
pub fn encode_bytes(
  data : Bytes,
  ec_level~ : EcLevel = EcLevel::M,
  min_version~ : Int32 = 1,
  max_version~ : Int32 = 40,
  mask? : Mask,
) -> QrCode raise QrError {
  if data.is_empty() {
    raise QrError::EmptyInput
  }
  encode_segments(
    [Segment::from_bytes(data)],
    ec_level~,
    min_version~,
    max_version~,
    mask?,
  )
}

///|
/// Encodes an explicit sequence of segments, preserving the caller's mode and
/// ordering choices. Empty segments are dropped. Numeric and alphanumeric
/// segments are validated against their repertoires.
///
/// 编码显式给出的段序列，保留调用方的模式与顺序选择。空段会被丢弃；
/// 数字与字母数字段会按其字符集校验。
///
/// # Example
/// ```mbt check
/// test "encode custom segments" {
///   let segments = [
///     @core.Segment::new(@core.Mode::Numeric, "123"),
///     @core.Segment::new(@core.Mode::Byte, "abc"),
///   ]
///   let qr = @core.encode_segments(segments)
///   assert_eq(qr.version().value(), 1)
/// }
/// ```
pub fn encode_segments(
  segments : Array[Segment],
  ec_level~ : EcLevel = EcLevel::M,
  min_version~ : Int32 = 1,
  max_version~ : Int32 = 40,
  mask? : Mask,
) -> QrCode raise QrError {
  let normalized : Array[Segment] = []
  for seg in segments {
    if seg.char_count() > 0 {
      match seg.mode_ {
        Numeric => validate(seg.data_, Numeric)
        Alphanumeric => validate(seg.data_, Alphanumeric)
        Byte => ()
      }
      normalized.push(seg)
    }
  }
  if normalized.length() == 0 {
    raise QrError::EmptyInput
  }
  let version = choose_version(normalized, ec_level, min_version, max_version)
  encode_at_version(normalized, version, ec_level, mask?)
}
