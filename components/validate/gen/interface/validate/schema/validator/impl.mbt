///|
fn err(field : String, code : String, message : String) -> FieldError {
  { field, code, message }
}

///|
fn is_whitespace(c : Char) -> Bool {
  c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

///|
fn is_email_shape(s : String) -> Bool {
  for c in s {
    if is_whitespace(c) {
      return false
    }
  }
  let mut at_count = 0
  let mut at_idx = -1
  for i = 0; i < s.length(); i = i + 1 {
    if s[i].to_int() == 64 { // '@'
      at_count += 1
      at_idx = i
    }
  }
  if at_count != 1 {
    return false
  }
  let local_part = s[0:at_idx].to_owned()
  let domain_part = s[at_idx + 1:s.length()].to_owned()
  if local_part.length() == 0 || domain_part.length() == 0 {
    return false
  }
  let mut has_dot = false
  for c in domain_part {
    if c == '.' {
      has_dot = true
    }
  }
  has_dot
}

///|
fn is_hex_digit(u : UInt16) -> Bool {
  let c = u.to_int()
  (c >= 48 && c <= 57) || (c >= 97 && c <= 102) || (c >= 65 && c <= 70)
}

///|
fn is_uuid_shape(s : String) -> Bool {
  if s.length() != 36 {
    return false
  }
  for i = 0; i < 36; i = i + 1 {
    if i == 8 || i == 13 || i == 18 || i == 23 {
      if s[i].to_int() != 45 { // '-'
        return false
      }
    } else {
      if !is_hex_digit(s[i]) {
        return false
      }
    }
  }
  true
}

///|
fn one_of_repr(v : @json.Json) -> String? {
  match v {
    String(s) => Some(s)
    Number(n, repr~) =>
      match repr {
        Some(r) => Some(r)
        None =>
          if n.to_int64().to_double() == n {
            Some("\{n.to_int64()}")
          } else {
            Some("\{n}")
          }
      }
    True => Some("true")
    False => Some("false")
    _ => None
  }
}

///|
fn check_numeric_range(rule : Rule, n : Double, out : Array[FieldError]) -> Unit {
  match rule.min_value {
    Some(min) =>
      if n < min {
        out.push(err(rule.field, "min-value", "must be at least \{min}"))
      }
    None => ()
  }
  match rule.max_value {
    Some(max) =>
      if n > max {
        out.push(err(rule.field, "max-value", "must be at most \{max}"))
      }
    None => ()
  }
}

///|
fn join_strings(arr : Array[String], sep : String) -> String {
  let buf = StringBuilder()
  for i = 0; i < arr.length(); i = i + 1 {
    if i > 0 {
      buf.write_string(sep)
    }
    buf.write_string(arr[i])
  }
  buf.to_string()
}

///|
fn check_one_of(rule : Rule, value : @json.Json, out : Array[FieldError]) -> Unit {
  if rule.one_of.length() == 0 {
    return
  }
  let repr = one_of_repr(value)
  let ok = match repr {
    Some(r) => rule.one_of.contains(r)
    None => false
  }
  if !ok {
    out.push(err(rule.field, "one-of", "must be one of: " + join_strings(rule.one_of, ", ")))
  }
}

///|
fn check_present(rule : Rule, value : @json.Json, out : Array[FieldError]) -> Unit {
  let field = rule.field

  match rule.kind {
    TEXT | EMAIL | ALPHANUMERIC | UUID => {
      let s = match value {
        String(str) => str
        _ => {
          out.push(err(field, "type", "expected a string"))
          return
        }
      }

      let char_len = s.length()
      if rule.min_len != 0U && char_len < rule.min_len.reinterpret_as_int() {
        out.push(err(field, "min-len", "must be at least \{rule.min_len} characters"))
      }
      if rule.max_len != 0U && char_len > rule.max_len.reinterpret_as_int() {
        out.push(err(field, "max-len", "must be at most \{rule.max_len} characters"))
      }

      match rule.kind {
        EMAIL =>
          if !is_email_shape(s) {
            out.push(err(field, "format", "not a valid email address"))
          }
        ALPHANUMERIC => {
          let mut valid = true
          for c in s {
            let is_alnum = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
            if !is_alnum {
              valid = false
              break
            }
          }
          if !valid {
            out.push(err(field, "format", "must contain only ASCII letters and digits"))
          }
        }
        UUID =>
          if !is_uuid_shape(s) {
            out.push(err(field, "format", "not a valid UUID"))
          }
        _ => ()
      }
    }
    INTEGER => {
      let n = match value {
        Number(num, ..) =>
          if num.to_int64().to_double() == num {
            num
          } else {
            out.push(err(field, "type", "expected an integer"))
            return
          }
        _ => {
          out.push(err(field, "type", "expected an integer"))
          return
        }
      }
      check_numeric_range(rule, n, out)
      check_one_of(rule, value, out)
      return
    }
    NUMBER => {
      let n = match value {
        Number(num, ..) => num
        _ => {
          out.push(err(field, "type", "expected a number"))
          return
        }
      }
      check_numeric_range(rule, n, out)
      check_one_of(rule, value, out)
      return
    }
    BOOLEAN => {
      match value {
        True | False => ()
        _ => {
          out.push(err(field, "type", "expected a boolean"))
          return
        }
      }
      check_one_of(rule, value, out)
      return
    }
  }

  check_one_of(rule, value, out)
}

///|
pub fn validate(json : String, rules : Array[Rule]) -> Array[FieldError] {
  let root = @json.parse(json) catch {
    _ => return [err("", "format", "not a valid JSON object")]
  }
  let obj = match root {
    Object(map) => map
    _ => return [err("", "format", "not a valid JSON object")]
  }

  let out : Array[FieldError] = []
  for rule in rules {
    match obj.get(rule.field) {
      None | Some(Null) =>
        if rule.required {
          out.push(err(rule.field, "required", "field is required"))
        }
      Some(value) => check_present(rule, value, out)
    }
  }
  out
}
