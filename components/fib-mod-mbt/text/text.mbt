///|
/// String helpers for a tool. A MoonBit `String` is UTF-16 and may hold an unpaired surrogate (half
/// of an emoji, from cutting by `length()`); such a string cannot leave the component: over wRPC the
/// host refuses it, and `@utf8.encode` aborts on it.
fn is_high(u : Int32) -> Bool {
  u >= 0xD800 && u <= 0xDBFF
}

///|
fn is_low(u : Int32) -> Bool {
  u >= 0xDC00 && u <= 0xDFFF
}

///|
pub fn is_well_formed(s : String) -> Bool {
  let n = s.length()
  let mut i = 0
  while i < n {
    let u = s.at(i).to_int()
    if is_high(u) && i + 1 < n && is_low(s.at(i + 1).to_int()) {
      i += 2
    } else if is_high(u) || is_low(u) {
      return false
    } else {
      i += 1
    }
  }
  true
}

///|
/// `s` with every unpaired surrogate replaced by U+FFFD.
pub fn well_formed(s : String) -> String {
  if is_well_formed(s) {
    return s
  }
  let buf = StringBuilder()
  let n = s.length()
  let mut i = 0
  while i < n {
    let u = s.at(i).to_int()
    if is_high(u) && i + 1 < n && is_low(s.at(i + 1).to_int()) {
      let lo = s.at(i + 1).to_int()
      buf.write_char(
        (0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00)).unsafe_to_char(),
      )
      i += 2
    } else if is_high(u) || is_low(u) {
      buf.write_char('\u{FFFD}')
      i += 1
    } else {
      buf.write_char(u.unsafe_to_char())
      i += 1
    }
  }
  buf.to_string()
}

///|
/// UTF-8 bytes of `s`, never aborting. `String::to_bytes` is UTF-16LE, not this.
pub fn utf8(s : String) -> Bytes {
  @utf8.encode(well_formed(s))
}

///|
/// The first `n` characters (code points) of `s`; `length()` counts UTF-16 units, and cutting by
/// it can split a character in two.
pub fn take_chars(s : String, n : Int32) -> String {
  let buf = StringBuilder()
  let mut k = 0
  for c in well_formed(s) {
    if k >= n {
      break
    }
    buf.write_char(c)
    k += 1
  }
  buf.to_string()
}
