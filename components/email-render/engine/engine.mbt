// Transactional email rendering engine

let std_table : Array[Char] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".to_array()

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

pub fn b64_encode(bytes : FixedArray[Byte]) -> String {
  let out = StringBuilder::new()
  let mut i = 0
  while i < bytes.length() {
    let remaining = bytes.length() - i
    let b0 = bytes[i].to_int() & 0xFF
    let b1 = if remaining > 1 { bytes[i + 1].to_int() & 0xFF } else { 0 }
    let b2 = if remaining > 2 { bytes[i + 2].to_int() & 0xFF } else { 0 }
    let n = (b0 << 16) | (b1 << 8) | b2
    out.write_char(std_table[(n >> 18) & 0x3F])
    out.write_char(std_table[(n >> 12) & 0x3F])
    if remaining > 2 {
      out.write_char(std_table[(n >> 6) & 0x3F])
      out.write_char(std_table[n & 0x3F])
    } else if remaining == 2 {
      out.write_char(std_table[(n >> 6) & 0x3F])
      out.write_char('=')
    } else {
      out.write_char('=')
      out.write_char('=')
    }
    i = i + 3
  }
  out.to_string()
}

fn std_char_val(code : Int) -> Int {
  let c = (code & 0xFF).unsafe_to_char()
  let n = c.to_int()
  if n >= 'A'.to_int() && n <= 'Z'.to_int() {
    n - 'A'.to_int()
  } else if n >= 'a'.to_int() && n <= 'z'.to_int() {
    n - 'a'.to_int() + 26
  } else if n >= '0'.to_int() && n <= '9'.to_int() {
    n - '0'.to_int() + 52
  } else if c == '+' {
    62
  } else if c == '/' {
    63
  } else {
    -1
  }
}

pub fn b64_decode(text : String) -> FixedArray[Byte]? {
  let len = text.length()
  if len == 0 {
    return Some(FixedArray::make(0, b'\x00'))
  }
  let mut pad_start = len
  for i = 0; i < len; i = i + 1 {
    if text[i] == '=' {
      pad_start = i
      break
    }
  }
  for i = pad_start; i < len; i = i + 1 {
    if text[i] != '=' {
      return None
    }
  }
  let values = []
  for i = 0; i < pad_start; i = i + 1 {
    let v = std_char_val(text[i].to_int())
    if v < 0 {
      return None
    }
    values.push(v)
  }
  if values.length() % 4 == 1 {
    return None
  }
  let out = []
  let mut i = 0
  while i + 4 <= values.length() {
    let n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6) | values[i + 3]
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
  Some(FixedArray::from_array(out))
}

let hex_upper : Array[Char] = "0123456789ABCDEF".to_array()

pub fn tmpl_key(name : String) -> String {
  let buf = StringBuilder::new()
  buf.write_string("tmpl_")
  for i = 0; i < name.length(); i = i + 1 {
    let code = name[i].to_int()
    let c = (code & 0xFF).unsafe_to_char()
    if (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '/' || c == '=' {
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

pub fn serialize_template(subject : String, text : String, html : String) -> String {
  b64_encode(str_to_bytes(subject)) + "\n" + b64_encode(str_to_bytes(text)) + "\n" + b64_encode(str_to_bytes(html))
}

pub fn parse_template(s : String) -> (String, String, String)? {
  let parts = []
  let mut start = 0
  for i = 0; i <= s.length(); i = i + 1 {
    if i == s.length() || s[i] == '\n' {
      parts.push(s[start:i].to_owned())
      start = i + 1
    }
  }
  if parts.length() < 3 {
    return None
  }
  let s_str = match b64_decode(parts[0]) {
    Some(b) => bytes_to_str(b)
    None => return None
  }
  let t_str = match b64_decode(parts[1]) {
    Some(b) => bytes_to_str(b)
    None => return None
  }
  let h_str = match b64_decode(parts[2]) {
    Some(b) => bytes_to_str(b)
    None => return None
  }
  Some((s_str, t_str, h_str))
}

pub fn html_escape(s : String) -> String {
  let buf = StringBuilder::new()
  for i = 0; i < s.length(); i = i + 1 {
    let c = (s[i].to_int() & 0xFF).unsafe_to_char()
    if c == '&' {
      buf.write_string("&amp;")
    } else if c == '<' {
      buf.write_string("&lt;")
    } else if c == '>' {
      buf.write_string("&gt;")
    } else if c == '"' {
      buf.write_string("&quot;")
    } else if c == '\'' {
      buf.write_string("&#x27;")
    } else {
      buf.write_char(c)
    }
  }
  buf.to_string()
}

fn is_name_char(code : Int) -> Bool {
  let c = (code & 0xFF).unsafe_to_char()
  (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-'
}

fn lookup(vars : Array[(String, String)], name : String) -> String? {
  for v in vars {
    if v.0 == name {
      return Some(v.1)
    }
  }
  None
}

pub fn render_field(field : String, vars : Array[(String, String)], escape : Bool) -> Result[String, String] {
  let buf = StringBuilder::new()
  let len = field.length()
  let mut i = 0
  while i < len {
    if field[i] == '{' {
      let mut j = i + 1
      while j < len && is_name_char(field[j].to_int()) {
        j = j + 1
      }
      if j > i + 1 && j < len && field[j] == '}' {
        let name = field[i + 1:j].to_owned()
        match lookup(vars, name) {
          Some(value) => {
            if escape {
              buf.write_string(html_escape(value))
            } else {
              buf.write_string(value)
            }
          }
          None => return Err(name)
        }
        i = j + 1
        continue
      }
      buf.write_char('{')
      i = i + 1
    } else {
      buf.write_char((field[i].to_int() & 0xFF).unsafe_to_char())
      i = i + 1
    }
  }
  Ok(buf.to_string())
}
