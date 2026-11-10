///|
/// A run of input encoded with one mode.
///
/// 使用同一模式编码的一段输入。
///
/// Byte segments created with `Segment::from_bytes` keep the original bytes
/// untouched; `data()` then returns a lossy UTF-8 view.
///
/// 由 `Segment::from_bytes` 创建的 Byte 段会保留原始字节，
/// 此时 `data()` 返回有损 UTF-8 文本。
pub struct Segment {
  mode_ : Mode
  data_ : String
  raw_ : Bytes?
}

///|
pub fn Segment::new(mode : Mode, data : String) -> Segment {
  { mode_: mode, data_: data, raw_: None, }
}

///|
/// Creates a Byte segment from arbitrary bytes, including data that is not
/// valid UTF-8.
///
/// 从任意字节（包括非 UTF-8 数据）创建 Byte 段。
pub fn Segment::from_bytes(data : Bytes) -> Segment {
  { mode_: Byte, data_: @utf8.decode_lossy(data[:]), raw_: Some(data), }
}

///|
pub fn Segment::mode(self : Segment) -> Mode {
  self.mode_
}

///|
/// Text view of the segment. For raw byte segments this is a lossy UTF-8
/// decoding; use `bytes()` when the exact payload matters.
///
/// 段的文本视图。对原始字节段而言是有损 UTF-8 解码；
/// 需要精确载荷时请使用 `bytes()`。
pub fn Segment::data(self : Segment) -> String {
  self.data_
}

///|
/// Exact payload bytes of the segment. Text segments are UTF-8 encoded.
///
/// 段的精确载荷字节。文本段按 UTF-8 编码。
pub fn Segment::bytes(self : Segment) -> Bytes {
  match self.raw_ {
    Some(data) => data
    None => @utf8.encode(self.data_)
  }
}

///|
/// Value of an alphanumeric character (ISO Table 5), if it belongs to the set.
fn alnum_value(c : Char) -> Int32? {
  if c >= '0' && c <= '9' {
    Some(c.to_int() - '0'.to_int())
  } else if c >= 'A' && c <= 'Z' {
    Some(c.to_int() - 'A'.to_int() + 10)
  } else {
    match c {
      ' ' => Some(36)
      '$' => Some(37)
      '%' => Some(38)
      '*' => Some(39)
      '+' => Some(40)
      '-' => Some(41)
      '.' => Some(42)
      '/' => Some(43)
      ':' => Some(44)
      _ => None
    }
  }
}

///|
fn classify(c : Char) -> Mode {
  if c >= '0' && c <= '9' {
    Numeric
  } else if alnum_value(c) is Some(_) {
    Alphanumeric
  } else {
    Byte
  }
}

///|
fn validate(text : String, mode : Mode) -> Unit raise QrError {
  for c in text {
    let ok = match mode {
      Numeric => c >= '0' && c <= '9'
      Alphanumeric => alnum_value(c) is Some(_)
      Byte => true
    }
    if !ok {
      raise QrError::InvalidCharacter(char=c, mode~)
    }
  }
}

///|
/// Character count written into the segment header.
///
/// 写入段头的字符计数。
pub fn Segment::char_count(self : Segment) -> Int32 {
  match self.mode_ {
    Byte => self.bytes().length()
    _ => self.data_.length()
  }
}

///|
/// Payload bit length, excluding mode and character-count fields.
///
/// 载荷比特长度，不含模式和字符计数字段。
pub fn Segment::data_bits(self : Segment) -> Int32 {
  match self.mode_ {
    Numeric => {
      let n = self.data_.length()
      let rem = n % 3
      n / 3 * 10 + (if rem == 2 { 7 } else if rem == 1 { 4 } else { 0 })
    }
    Alphanumeric => {
      let n = self.data_.length()
      n / 2 * 11 + (if n % 2 == 1 { 6 } else { 0 })
    }
    Byte => self.bytes().length() * 8
  }
}

///|
/// Merges short non-byte runs into an adjacent byte run when doing so saves
/// header bits, then merges adjacent byte runs. Purely numeric/alphanumeric
/// inputs are left alone because they have no byte neighbour.
fn merge_short_runs(segments : Array[Segment]) -> Array[Segment] {
  let result : Array[Segment] = []
  let n = segments.length()
  for i in 0..<n {
    let seg = segments[i]
    let prev_byte = i > 0 && segments[i - 1].mode_ == Byte
    let next_byte = i + 1 < n && segments[i + 1].mode_ == Byte
    let has_byte_neighbor = prev_byte || next_byte
    let short = seg.data_.length()
    let numeric_short = seg.mode_ == Numeric && short <= 2
    let alpha_short = seg.mode_ == Alphanumeric && short <= 4
    let absorb = has_byte_neighbor && (numeric_short || alpha_short)
    let as_byte = if seg.mode_ == Byte || absorb {
      Some(Segment::new(Byte, seg.data_))
    } else {
      None
    }
    match as_byte {
      Some(b) =>
        if result.length() > 0 && result[result.length() - 1].mode_ == Byte {
          match result.pop() {
            Some(prev) => result.push(Segment::new(Byte, prev.data_ + b.data_))
            None => result.push(b)
          }
        } else {
          result.push(b)
        }
      None => result.push(seg)
    }
  }
  result
}

///|
/// Splits text into greedy class runs. Forcing `mode` produces a single segment
/// and rejects characters outside that mode's repertoire.
///
/// 将文本拆分为贪心分类段。强制指定 `mode` 时生成单个段，
/// 并拒绝该模式字符集之外的字符。
pub fn split_segments(
  text : String,
  mode? : Mode,
) -> Array[Segment] raise QrError {
  if text.is_empty() {
    raise QrError::EmptyInput
  }
  match mode {
    Some(m) => {
      validate(text, m)
      [Segment::new(m, text)]
    }
    None => {
      let segments : Array[Segment] = []
      let sb = StringBuilder()
      let mut current = Mode::Byte
      let mut has_content = false
      for c in text {
        let m = classify(c)
        if !has_content {
          current = m
          has_content = true
          sb.write_char(c)
        } else if m == current {
          sb.write_char(c)
        } else {
          segments.push(Segment::new(current, sb.to_string()))
          sb.reset()
          current = m
          sb.write_char(c)
        }
      }
      segments.push(Segment::new(current, sb.to_string()))
      merge_short_runs(segments)
    }
  }
}

///|
const DP_STATE_COUNT : Int32 = 6

///|
const DP_INF : Int32 = 0x3FFFFFFF

///|
/// State 0..2: numeric, 3..4: alphanumeric, 5: byte. Numeric states carry the
/// character count modulo 3, alphanumeric states modulo 2.
fn state_mode(state : Int32) -> Mode {
  if state <= 2 {
    Numeric
  } else if state <= 4 {
    Alphanumeric
  } else {
    Byte
  }
}

///|
fn start_state(mode : Mode) -> Int32 {
  match mode {
    Numeric => 1
    Alphanumeric => 4
    Byte => 5
  }
}

///|
fn utf8_len(c : Char) -> Int32 {
  let code = c.to_int()
  if code < 0x80 {
    1
  } else if code < 0x800 {
    2
  } else if code < 0x10000 {
    3
  } else {
    4
  }
}

///|
fn first_payload_bits(mode : Mode, c : Char) -> Int32 {
  match mode {
    Numeric => 4
    Alphanumeric => 6
    Byte => 8 * utf8_len(c)
  }
}

///|
fn continue_bits(mode : Mode, state : Int32, c : Char) -> (Int32, Int32) {
  match mode {
    Numeric =>
      if state == 0 {
        (1, 4)
      } else if state == 1 {
        (2, 3)
      } else {
        (0, 3)
      }
    Alphanumeric => if state == 3 { (4, 6) } else { (3, 5) }
    Byte => (5, 8 * utf8_len(c))
  }
}

///|
fn accepts(mode : Mode, c : Char) -> Bool {
  match mode {
    Numeric => c >= '0' && c <= '9'
    Alphanumeric => alnum_value(c) is Some(_)
    Byte => true
  }
}

///|
/// Finds the segmentation with the fewest payload bits for `version` using a
/// six-state dynamic program over the string. Headers count towards the cost,
/// so switching modes is only chosen when it actually pays off.
///
/// 使用六状态动态规划求 `version` 下比特数最少的切分；
/// 段头计入代价，因此只有在真正划算时才会切换模式。
fn optimal_segments(text : String, version : Int32) -> Array[Segment] {
  let chars = text.to_array()
  let n = chars.length()
  if n == 0 {
    return []
  }
  let parents : Array[Int32] = Array::make(n * DP_STATE_COUNT, -1)
  let starts : Array[Bool] = Array::make(n * DP_STATE_COUNT, false)
  let mut current : Array[Int32] = Array::make(DP_STATE_COUNT, DP_INF)
  for i in 0..<n {
    let c = chars[i]
    let next : Array[Int32] = Array::make(DP_STATE_COUNT, DP_INF)
    if i > 0 {
      for s in 0..<DP_STATE_COUNT {
        let base = current[s]
        if base < DP_INF {
          let sm = state_mode(s)
          if accepts(sm, c) {
            let (target, add) = continue_bits(sm, s, c)
            let candidate = base + add
            if candidate < next[target] {
              next[target] = candidate
              parents[i * DP_STATE_COUNT + target] = s
              starts[i * DP_STATE_COUNT + target] = false
            }
          }
        }
      }
    }
    for m in [Numeric, Alphanumeric, Byte] {
      if accepts(m, c) {
        let target = start_state(m)
        let cost = 4 + char_count_bits(version, m) + first_payload_bits(m, c)
        if i == 0 {
          next[target] = cost
          parents[target] = -1
          starts[target] = true
        } else {
          for s in 0..<DP_STATE_COUNT {
            let base = current[s]
            if base < DP_INF && state_mode(s) != m {
              let candidate = base + cost
              let idx = i * DP_STATE_COUNT + target
              if candidate < next[target] {
                next[target] = candidate
                parents[idx] = s
                starts[idx] = true
              }
            }
          }
        }
      }
    }
    current = next
  }
  let mut best_state = 0
  let mut best_cost = DP_INF
  for s in 0..<DP_STATE_COUNT {
    if current[s] < best_cost {
      best_cost = current[s]
      best_state = s
    }
  }
  let reversed : Array[Segment] = []
  let mut chars_rev : Array[Char] = []
  let mut state = best_state
  let mut i = n - 1
  while i >= 0 {
    chars_rev.push(chars[i])
    let idx = i * DP_STATE_COUNT + state
    if starts[idx] || i == 0 {
      let sb = StringBuilder()
      for ch in chars_rev.rev() {
        sb.write_char(ch)
      }
      reversed.push(Segment::new(state_mode(state), sb.to_string()))
      chars_rev = []
    }
    let parent = parents[idx]
    if parent >= 0 {
      state = parent
    }
    i -= 1
  }
  reversed.rev()
}

///|
/// Groups versions that share character-count field widths.
fn version_group(version : Int32) -> Int32 {
  if version <= 9 {
    0
  } else if version <= 26 {
    1
  } else {
    2
  }
}

///|
fn group_representative(group : Int32) -> Int32 {
  if group == 0 {
    1
  } else if group == 1 {
    10
  } else {
    27
  }
}

///|
fn validate_versions(
  min_version : Int32,
  max_version : Int32,
) -> Unit raise QrError {
  if min_version < 1 || min_version > 40 {
    raise QrError::InvalidVersion(min_version)
  }
  if max_version < min_version || max_version > 40 {
    raise QrError::InvalidVersion(max_version)
  }
}

///|
/// Picks the smallest version whose capacity fits the fixed segments.
fn choose_version(
  segments : Array[Segment],
  ec : EcLevel,
  min_version : Int32,
  max_version : Int32,
) -> Int32 raise QrError {
  validate_versions(min_version, max_version)
  let mut chosen = 0
  for v in min_version..<=max_version {
    let bits = bits_needed(segments, v)
    if chosen == 0 && bits <= data_capacity_bits(v, ec) {
      chosen = v
    }
  }
  if chosen == 0 {
    raise QrError::DataTooLong(
      needed=bits_needed(segments, max_version),
      capacity=data_capacity_bits(max_version, ec),
      version=max_version,
      ec_level=ec,
    )
  }
  chosen
}

///|
/// Picks the smallest fitting version together with the optimal segmentation
/// for that version group. Segment costs are constant inside a group, so one
/// dynamic-program run per group is enough.
///
/// 选出能容纳数据的最小版本，并返回该版本组的最优切分。
/// 同一版本组内段代价不变，因此每组只需运行一次动态规划。
fn choose_version_optimal(
  text : String,
  ec : EcLevel,
  min_version : Int32,
  max_version : Int32,
) -> (Int32, Array[Segment]) raise QrError {
  validate_versions(min_version, max_version)
  let cache : Array[Array[Segment]?] = [None, None, None]
  let mut chosen = 0
  let mut chosen_segments : Array[Segment] = []
  let mut v = min_version
  while v <= max_version && chosen == 0 {
    let group = version_group(v)
    let segments = match cache[group] {
      Some(segments) => segments
      None => {
        let computed = optimal_segments(text, group_representative(group))
        cache[group] = Some(computed)
        computed
      }
    }
    if bits_needed(segments, v) <= data_capacity_bits(v, ec) {
      chosen = v
      chosen_segments = segments
    }
    v = v + 1
  }
  if chosen == 0 {
    let group = version_group(max_version)
    let segments = match cache[group] {
      Some(segments) => segments
      None => optimal_segments(text, group_representative(group))
    }
    raise QrError::DataTooLong(
      needed=bits_needed(segments, max_version),
      capacity=data_capacity_bits(max_version, ec),
      version=max_version,
      ec_level=ec,
    )
  }
  (chosen, chosen_segments)
}

///|
/// Mode indicator, ISO Table 4 (Kanji omitted in v0.x).
fn mode_indicator(mode : Mode) -> Int32 {
  match mode {
    Numeric => 1
    Alphanumeric => 2
    Byte => 4
  }
}

///|
/// Total bits needed for the segments at a given version, headers included.
///
/// 给定版本下所有段的总比特数，包含段头。
pub fn bits_needed(segments : Array[Segment], version : Int32) -> Int32 {
  let mut total = 0
  for seg in segments {
    total += 4 + char_count_bits(version, seg.mode_) + seg.data_bits()
  }
  total
}

///|
fn append_segment(buf : BitBuffer, seg : Segment, version : Int32) -> Unit {
  buf.push_bits(mode_indicator(seg.mode_), 4)
  buf.push_bits(seg.char_count(), char_count_bits(version, seg.mode_))
  match seg.mode_ {
    Numeric => {
      let digits : Array[Int32] = []
      for c in seg.data_ {
        digits.push(c.to_int() - '0'.to_int())
      }
      let mut i = 0
      while i < digits.length() {
        let remaining = digits.length() - i
        if remaining >= 3 {
          buf.push_bits(
            digits[i] * 100 + digits[i + 1] * 10 + digits[i + 2],
            10,
          )
          i += 3
        } else if remaining == 2 {
          buf.push_bits(digits[i] * 10 + digits[i + 1], 7)
          i += 2
        } else {
          buf.push_bits(digits[i], 4)
          i += 1
        }
      }
    }
    Alphanumeric => {
      let mut pending : Int32? = None
      for c in seg.data_ {
        if alnum_value(c) is Some(v) {
          match pending {
            None => pending = Some(v)
            Some(first) => {
              buf.push_bits(first * 45 + v, 11)
              pending = None
            }
          }
        }
      }
      if pending is Some(first) {
        buf.push_bits(first, 6)
      }
    }
    Byte =>
      for b in seg.bytes() {
        buf.push_bits(b.to_int(), 8)
      }
  }
}
