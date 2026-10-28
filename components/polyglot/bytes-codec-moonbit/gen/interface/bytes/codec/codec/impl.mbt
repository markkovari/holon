// `bytes:codec`, in MoonBit — the same WIT the Rust component exports.
//
// This exists to answer one question: can a capability in this pool be written in a
// language nothing else here uses, and be judged by the specification that already
// exists? `components/bytes-codec/gate.sh` drives the contract over HTTP, so it does
// not care what compiled the thing it is judging — that is the whole point of moving
// the gate to the WIT boundary before writing this.
//
// Deliberately not a base64 library. The point is not that one exists; it is that
// this implementation and the Rust one agree, character for character, on both
// alphabets and on padding.

// `Array[Char]`, not `String`: a MoonBit string indexes to UTF-16 code units, and
// `write_char` wants a Char. The tables are ASCII either way.
let std_table : Array[Char] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".to_array()

let url_table : Array[Char] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".to_array()

///|
/// The value of one character in `alphabet`, or -1.
///
/// The two alphabets are NOT interchangeable: a character belonging to the other
/// table is refused here rather than accepted because some table has it. Decoding
/// with the wrong one otherwise returns different bytes and reports nothing.
fn char_value(c : Char, alphabet : Alphabet) -> Int {
  let n = c.to_int()
  if n >= 'A'.to_int() && n <= 'Z'.to_int() {
    n - 'A'.to_int()
  } else if n >= 'a'.to_int() && n <= 'z'.to_int() {
    n - 'a'.to_int() + 26
  } else if n >= '0'.to_int() && n <= '9'.to_int() {
    n - '0'.to_int() + 52
  } else {
    match alphabet {
      STANDARD =>
        if c == '+' {
          62
        } else if c == '/' {
          63
        } else {
          -1
        }
      URL_SAFE =>
        if c == '-' {
          62
        } else if c == '_' {
          63
        } else {
          -1
        }
    }
  }
}

///|
pub fn encode(bytes : FixedArray[Byte], alphabet : Alphabet) -> String {
  let table = match alphabet {
    STANDARD => std_table
    URL_SAFE => url_table
  }
  // Only the standard alphabet pads; the specifications that use url-safe forbid it.
  let padded = match alphabet {
    STANDARD => true
    URL_SAFE => false
  }
  let out = StringBuilder::new()
  let mut i = 0
  while i < bytes.length() {
    let remaining = bytes.length() - i
    let b0 = bytes[i].to_int()
    let b1 = if remaining > 1 { bytes[i + 1].to_int() } else { 0 }
    let b2 = if remaining > 2 { bytes[i + 2].to_int() } else { 0 }
    let n = (b0 << 16) | (b1 << 8) | b2
    out.write_char(table[(n >> 18) & 0x3F])
    out.write_char(table[(n >> 12) & 0x3F])
    if remaining > 2 {
      out.write_char(table[(n >> 6) & 0x3F])
      out.write_char(table[n & 0x3F])
    } else if remaining == 2 {
      out.write_char(table[(n >> 6) & 0x3F])
      if padded {
        out.write_char('=')
      }
    } else if padded {
      out.write_char('=')
      out.write_char('=')
    }
    i = i + 3
  }
  out.to_string()
}

///|
pub fn decode(
  text : String,
  alphabet : Alphabet,
) -> Result[FixedArray[Byte], DecodeError] {
  let chars = text.to_array()
  if chars.length() == 0 {
    return Ok(FixedArray::make(0, b'\x00'))
  }

  // Padding belongs at the end and nowhere else.
  let mut pad_start = chars.length()
  for i = 0; i < chars.length(); i = i + 1 {
    if chars[i] == '=' {
      pad_start = i
      break
    }
  }
  for i = pad_start; i < chars.length(); i = i + 1 {
    if chars[i] != '=' {
      return Err(MisplacedPadding(pad_start.reinterpret_as_uint()))
    }
  }
  if chars.length() - pad_start > 2 {
    return Err(MisplacedPadding(pad_start.reinterpret_as_uint()))
  }

  let values = Array::new()
  for i = 0; i < pad_start; i = i + 1 {
    let v = char_value(chars[i], alphabet)
    if v < 0 {
      // The character itself, so a caller can say which one without printing the
      // payload it came from.
      return Err(
        NotInAlphabet(
          (i.reinterpret_as_uint(), String::from_array([chars[i]])),
        ),
      )
    }
    values.push(v)
  }
  // One leftover character encodes no byte.
  if values.length() % 4 == 1 {
    return Err(TruncatedGroup(chars.length().reinterpret_as_uint()))
  }

  let out = Array::new()
  let mut i = 0
  while i + 4 <= values.length() {
    let n = (values[i] << 18) |
      (values[i + 1] << 12) |
      (values[i + 2] << 6) |
      values[i + 3]
    out.push(((n >> 16) & 0xFF).to_byte())
    out.push(((n >> 8) & 0xFF).to_byte())
    out.push((n & 0xFF).to_byte())
    i = i + 4
  }
  let rest = values.length() - i
  if rest == 2 {
    let n = (values[i] << 18) | (values[i + 1] << 12)
    out.push(((n >> 16) & 0xFF).to_byte())
  } else if rest == 3 {
    let n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6)
    out.push(((n >> 16) & 0xFF).to_byte())
    out.push(((n >> 8) & 0xFF).to_byte())
  }
  Ok(FixedArray::from_array(out))
}

///|
pub fn to_hex(bytes : FixedArray[Byte]) -> String {
  let digits = "0123456789abcdef".to_array()
  let out = StringBuilder::new()
  for i = 0; i < bytes.length(); i = i + 1 {
    let b = bytes[i].to_int()
    out.write_char(digits[(b >> 4) & 0x0F])
    out.write_char(digits[b & 0x0F])
  }
  out.to_string()
}

///|
pub fn from_hex(text : String) -> Result[FixedArray[Byte], DecodeError] {
  let chars = text.to_array()
  let values = Array::new()
  for i = 0; i < chars.length(); i = i + 1 {
    let c = chars[i]
    let n = c.to_int()
    let v = if n >= '0'.to_int() && n <= '9'.to_int() {
      n - '0'.to_int()
    } else if n >= 'a'.to_int() && n <= 'f'.to_int() {
      n - 'a'.to_int() + 10
    } else if n >= 'A'.to_int() && n <= 'F'.to_int() {
      n - 'A'.to_int() + 10
    } else {
      -1
    }
    if v < 0 {
      return Err(
        NotInAlphabet((i.reinterpret_as_uint(), String::from_array([c]))),
      )
    }
    values.push(v)
  }
  if values.length() % 2 != 0 {
    return Err(TruncatedGroup(values.length().reinterpret_as_uint()))
  }
  let out = Array::new()
  let mut i = 0
  while i < values.length() {
    out.push((((values[i] << 4) | values[i + 1]) & 0xFF).to_byte())
    i = i + 2
  }
  Ok(FixedArray::from_array(out))
}
