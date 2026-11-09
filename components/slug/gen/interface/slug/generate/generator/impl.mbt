///|
fn translit(c : Char) -> String? {
  match c {
    'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => Some("a")
    'é' | 'è' | 'ê' | 'ë' => Some("e")
    'î' | 'ï' | 'í' | 'ì' => Some("i")
    'ô' | 'ö' | 'ó' | 'ò' | 'õ' => Some("o")
    'ù' | 'û' | 'ü' | 'ú' => Some("u")
    'ç' => Some("c")
    'ñ' => Some("n")
    'ß' => Some("ss")
    _ => None
  }
}

///|
fn slugify_sep(text : String, sep : String) -> String {
  let tokens : Array[Char] = []
  let lower = text.to_lower()
  for c in lower {
    if (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') {
      tokens.push(c)
    } else {
      match translit(c) {
        Some(rep) => for rc in rep { tokens.push(rc) }
        None => tokens.push(' ')
      }
    }
  }

  let out = StringBuilder()
  let mut pending_sep = false
  let mut started = false
  for c in tokens {
    if c == ' ' {
      pending_sep = true
    } else {
      if started && pending_sep {
        out.write_string(sep)
      }
      out.write_char(c)
      started = true
      pending_sep = false
    }
  }
  out.to_string()
}

///|
fn rfind(s : String, sep : String) -> Int? {
  if sep.length() == 0 || s.length() < sep.length() {
    return None
  }
  let mut i = s.length() - sep.length()
  while i >= 0 {
    if s[i:i + sep.length()].to_owned() == sep {
      return Some(i)
    }
    i -= 1
  }
  None
}

///|
fn ends_with(s : String, sep : String) -> Bool {
  if sep.length() == 0 || s.length() < sep.length() {
    return false
  }
  s[s.length() - sep.length():s.length()].to_owned() == sep
}

///|
fn bound_length(s : String, sep : String, max : Int) -> String {
  if max == 0 || s.length() <= max {
    return s
  }
  let mut sub = s[0:max].to_owned()
  if sep != "" {
    match rfind(sub, sep) {
      Some(idx) => sub = sub[0:idx].to_owned()
      None => ()
    }
    while ends_with(sub, sep) {
      let n = sub.length() - sep.length()
      sub = sub[0:n].to_owned()
    }
  }
  sub
}

///|
pub fn slugify(text : String) -> String {
  slugify_with(text, { separator: "", max_length: 0 })
}

///|
pub fn slugify_with(text : String, opts : Options) -> String {
  let sep = if opts.separator.length() == 0 { "-" } else { opts.separator }
  let slug = slugify_sep(text, sep)
  bound_length(slug, sep, opts.max_length.reinterpret_as_int())
}

///|
pub fn uniquify(desired : String, taken : Array[String]) -> String {
  if !taken.contains(desired) {
    return desired
  }
  let mut n = 2
  while true {
    let candidate = "\{desired}-\{n}"
    if !taken.contains(candidate) {
      return candidate
    }
    n += 1
  }
  desired
}
