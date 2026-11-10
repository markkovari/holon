// Core algorithms for weighted sticky experiment assignment

let hex_upper : Array[Char] = "0123456789ABCDEF".to_array()

pub fn sanitize(s : String) -> String {
  let buf = StringBuilder::new()
  for i = 0; i < s.length(); i = i + 1 {
    let code = s[i].to_int()
    let c = (code & 0xFF).unsafe_to_char()
    if (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '=' {
      buf.write_char(c)
    } else {
      let b = code & 0xFF
      buf.write_char('_')
      buf.write_char(hex_upper[(b >> 4) & 0x0F])
      buf.write_char(hex_upper[b & 0x0F])
    }
  }
  buf.to_string()
}

fn hex_val(code : Int) -> Int? {
  let c = (code & 0xFF).unsafe_to_char()
  let n = c.to_int()
  if n >= '0'.to_int() && n <= '9'.to_int() {
    Some(n - '0'.to_int())
  } else if n >= 'A'.to_int() && n <= 'F'.to_int() {
    Some(n - 'A'.to_int() + 10)
  } else if n >= 'a'.to_int() && n <= 'f'.to_int() {
    Some(n - 'a'.to_int() + 10)
  } else {
    None
  }
}

pub fn unsanitize(s : String) -> String {
  let buf = StringBuilder::new()
  let len = s.length()
  let mut i = 0
  while i < len {
    if s[i] == '_' && i + 2 < len {
      match (hex_val(s[i + 1].to_int()), hex_val(s[i + 2].to_int())) {
        (Some(hi), Some(lo)) => {
          let b = (hi << 4) | lo
          buf.write_char((b & 0xFF).unsafe_to_char())
          i = i + 3
          continue
        }
        _ => ()
      }
    }
    buf.write_char((s[i].to_int() & 0xFF).unsafe_to_char())
    i = i + 1
  }
  buf.to_string()
}

pub fn exp_key(tenant : String, name : String) -> String {
  "ex_" + sanitize(tenant) + "/" + sanitize(name)
}

pub fn parse_uint(s : String) -> UInt? {
  if s.length() == 0 {
    return None
  }
  let mut res = 0U
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c >= '0' && c <= '9' {
      res = res * 10U + (c.to_int() - '0'.to_int()).reinterpret_as_uint()
    } else {
      return None
    }
  }
  Some(res)
}

pub fn str_to_bytes(s : String) -> FixedArray[Byte] {
  let len = s.length()
  let arr = FixedArray::make(len, b'\x00')
  for i = 0; i < len; i = i + 1 {
    arr[i] = (s[i].to_int() & 0xFF).to_byte()
  }
  arr
}

pub fn bytes_to_str(bytes : FixedArray[Byte]) -> String {
  let buf = StringBuilder::new()
  for i = 0; i < bytes.length(); i = i + 1 {
    buf.write_char((bytes[i].to_int() & 0xFF).unsafe_to_char())
  }
  buf.to_string()
}

pub fn variants_to_bytes(variants : Array[(String, UInt)]) -> FixedArray[Byte] {
  let buf = StringBuilder::new()
  for i = 0; i < variants.length(); i = i + 1 {
    if i > 0 {
      buf.write_char(',')
    }
    buf.write_string(sanitize(variants[i].0))
    buf.write_char(':')
    buf.write_string(variants[i].1.to_string())
  }
  str_to_bytes(buf.to_string())
}

pub fn variants_from_bytes(bytes : FixedArray[Byte]) -> Array[(String, UInt)]? {
  let s = bytes_to_str(bytes)
  if s.length() == 0 {
    return Some([])
  }
  let out = []
  let mut start = 0
  for i = 0; i <= s.length(); i = i + 1 {
    if i == s.length() || s[i] == ',' {
      let part = s[start:i].to_owned()
      start = i + 1
      let mut colon_idx = -1
      for j = part.length() - 1; j >= 0; j = j - 1 {
        if part[j] == ':' {
          colon_idx = j
          break
        }
      }
      if colon_idx < 0 {
        return None
      }
      let enc = part[0:colon_idx].to_owned()
      let w_str = part[colon_idx + 1:].to_owned()
      let w = match parse_uint(w_str) {
        Some(v) => v
        None => return None
      }
      out.push((unsanitize(enc), w))
    }
  }
  Some(out)
}

pub fn fnv1a(s : String) -> UInt64 {
  let mut h = 0xcbf29ce484222325UL
  for i = 0; i < s.length(); i = i + 1 {
    let b = (s[i].to_int() & 0xFF).to_uint64()
    h = h ^ b
    h = h * 0x00000100000001b3UL
  }
  h
}

pub fn pick(variants : Array[(String, UInt)], subject : String) -> String? {
  let mut total = 0UL
  for i = 0; i < variants.length(); i = i + 1 {
    total = total + variants[i].1.to_uint64()
  }
  if total == 0UL {
    return None
  }
  let point = fnv1a(subject) % 1000UL
  let scaled = point * total / 1000UL
  let mut acc = 0UL
  for i = 0; i < variants.length(); i = i + 1 {
    acc = acc + variants[i].1.to_uint64()
    if scaled < acc {
      return Some(variants[i].0)
    }
  }
  for i = variants.length() - 1; i >= 0; i = i - 1 {
    if variants[i].1 > 0U {
      return Some(variants[i].0)
    }
  }
  None
}
