// Implementation of case:conv@0.1.0 conv interface in MoonBit

fn is_whitespace(c : Char) -> Bool {
  c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

fn is_lowercase(c : Char) -> Bool {
  c >= 'a' && c <= 'z'
}

fn is_uppercase(c : Char) -> Bool {
  c >= 'A' && c <= 'Z'
}

fn is_alphabetic(c : Char) -> Bool {
  is_lowercase(c) || is_uppercase(c)
}

fn is_numeric(c : Char) -> Bool {
  c >= '0' && c <= '9'
}

fn to_lower_char(c : Char) -> Char {
  if is_uppercase(c) {
    (c.to_int() + 32).unsafe_to_char()
  } else {
    c
  }
}

fn to_upper_char(c : Char) -> Char {
  if is_lowercase(c) {
    (c.to_int() - 32).unsafe_to_char()
  } else {
    c
  }
}

fn to_lower(s : String) -> String {
  let buf = StringBuilder()
  for c in s {
    buf.write_char(to_lower_char(c))
  }
  buf.to_string()
}

fn capitalize(w : String) -> String {
  if w.is_empty() {
    return ""
  }
  let buf = StringBuilder()
  let mut first = true
  for c in w {
    if first {
      buf.write_char(to_upper_char(c))
      first = false
    } else {
      buf.write_char(to_lower_char(c))
    }
  }
  buf.to_string()
}

fn is_sep(c : Char) -> Bool {
  c == '_' || c == '-' || is_whitespace(c)
}

fn words(s : String) -> Array[String] {
  let chars : Array[Char] = []
  for c in s {
    chars.push(c)
  }
  let result : Array[String] = []
  let mut cur = StringBuilder()
  let mut cur_len = 0

  for i = 0; i < chars.length(); i = i + 1 {
    let c = chars[i]
    if is_sep(c) {
      if cur_len > 0 {
        result.push(cur.to_string())
        cur = StringBuilder()
        cur_len = 0
      }
      continue
    }

    if i > 0 {
      let prev = chars[i - 1]
      let prev_sep = is_sep(prev)
      let next_lower = if i + 1 < chars.length() {
        is_lowercase(chars[i + 1])
      } else {
        false
      }
      let boundary = !prev_sep &&
        (
          (is_lowercase(prev) && is_uppercase(c)) ||
          (is_uppercase(prev) && is_uppercase(c) && next_lower) ||
          (is_alphabetic(prev) && is_numeric(c)) ||
          (is_numeric(prev) && is_alphabetic(c))
        )
      if boundary && cur_len > 0 {
        result.push(cur.to_string())
        cur = StringBuilder()
        cur_len = 0
      }
    }
    cur.write_char(c)
    cur_len = cur_len + 1
  }

  if cur_len > 0 {
    result.push(cur.to_string())
  }
  result
}

///|
pub fn to_snake(s : String) -> String {
  let ws = words(s)
  let buf = StringBuilder()
  for i = 0; i < ws.length(); i = i + 1 {
    if i > 0 {
      buf.write_char('_')
    }
    buf.write_string(to_lower(ws[i]))
  }
  buf.to_string()
}

///|
pub fn to_kebab(s : String) -> String {
  let ws = words(s)
  let buf = StringBuilder()
  for i = 0; i < ws.length(); i = i + 1 {
    if i > 0 {
      buf.write_char('-')
    }
    buf.write_string(to_lower(ws[i]))
  }
  buf.to_string()
}

///|
pub fn to_camel(s : String) -> String {
  let ws = words(s)
  let buf = StringBuilder()
  for i = 0; i < ws.length(); i = i + 1 {
    if i == 0 {
      buf.write_string(to_lower(ws[i]))
    } else {
      buf.write_string(capitalize(ws[i]))
    }
  }
  buf.to_string()
}

///|
pub fn to_pascal(s : String) -> String {
  let ws = words(s)
  let buf = StringBuilder()
  for i = 0; i < ws.length(); i = i + 1 {
    buf.write_string(capitalize(ws[i]))
  }
  buf.to_string()
}
