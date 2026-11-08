// Implementation of iban:validate@0.1.0 validator interface in MoonBit

let lengths : Array[(String, UInt)] = [
  ("AD", 24U),
  ("AE", 23U),
  ("AT", 20U),
  ("BE", 16U),
  ("BG", 22U),
  ("BH", 22U),
  ("BR", 29U),
  ("CH", 21U),
  ("CY", 28U),
  ("CZ", 24U),
  ("DE", 22U),
  ("DK", 18U),
  ("EE", 20U),
  ("ES", 24U),
  ("FI", 18U),
  ("FO", 18U),
  ("FR", 27U),
  ("GB", 22U),
  ("GE", 22U),
  ("GI", 23U),
  ("GL", 18U),
  ("GR", 27U),
  ("HR", 21U),
  ("HU", 28U),
  ("IE", 22U),
  ("IL", 23U),
  ("IS", 26U),
  ("IT", 27U),
  ("KW", 30U),
  ("LB", 28U),
  ("LI", 21U),
  ("LT", 20U),
  ("LU", 20U),
  ("LV", 21U),
  ("MC", 27U),
  ("MD", 24U),
  ("ME", 22U),
  ("MK", 19U),
  ("MT", 31U),
  ("MU", 30U),
  ("NL", 18U),
  ("NO", 15U),
  ("PL", 28U),
  ("PT", 25U),
  ("QA", 29U),
  ("RO", 24U),
  ("RS", 22U),
  ("SA", 24U),
  ("SE", 24U),
  ("SI", 19U),
  ("SK", 24U),
  ("SM", 27U),
  ("TN", 24U),
  ("TR", 26U),
  ("UA", 29U),
  ("VA", 22U),
]

fn expected_length(country : String) -> UInt? {
  for item in lengths {
    let (c, len) = item
    if c == country {
      return Some(len)
    }
  }
  None
}

fn mod97_step(rem : UInt, digit : UInt) -> UInt {
  (rem * 10U + digit) % 97U
}

///|
pub fn validate(iban : String) -> Result[IbanInfo, IbanError] {
  let norm_buf = StringBuilder()
  for c in iban {
    if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
      continue
    }
    if c >= 'a' && c <= 'z' {
      norm_buf.write_char((c.to_int() - 32).unsafe_to_char())
    } else {
      norm_buf.write_char(c)
    }
  }
  let norm = norm_buf.to_string()
  if norm.length() < 5 {
    return Err(IbanError::TooShort)
  }

  let chars : Array[Char] = []
  for c in norm {
    chars.push(c)
  }

  let country_buf = StringBuilder()
  country_buf.write_char(chars[0])
  country_buf.write_char(chars[1])
  let country = country_buf.to_string()

  if !(chars[0] >= 'A' && chars[0] <= 'Z' && chars[1] >= 'A' && chars[1] <= 'Z') {
    return Err(IbanError::BadCountry(country))
  }

  if !(chars[2] >= '0' && chars[2] <= '9' && chars[3] >= '0' && chars[3] <= '9') {
    return Err(IbanError::BadCheck)
  }

  for c in chars {
    let is_alnum = (c >= '0' && c <= '9') || (c >= 'A' && c <= 'Z')
    if !is_alnum {
      let bad = StringBuilder()
      bad.write_char(c)
      return Err(IbanError::BadChar(bad.to_string()))
    }
  }

  let norm_len = norm.length().reinterpret_as_uint()
  match expected_length(country) {
    Some(exp) =>
      if norm_len != exp {
        return Err(IbanError::BadLength((norm_len, exp)))
      }
    None => ()
  }

  let mut rem = 0U
  let zero_code = '0'.to_int()
  let a_code = 'A'.to_int()

  fn feed(rem : UInt, c : Char) -> UInt {
    if c >= '0' && c <= '9' {
      let digit = (c.to_int() - zero_code).reinterpret_as_uint()
      mod97_step(rem, digit)
    } else {
      let v = (c.to_int() - a_code + 10).reinterpret_as_uint()
      let r1 = mod97_step(rem, v / 10U)
      mod97_step(r1, v % 10U)
    }
  }

  for i = 4; i < chars.length(); i = i + 1 {
    rem = feed(rem, chars[i])
  }
  for i = 0; i < 4; i = i + 1 {
    rem = feed(rem, chars[i])
  }

  if rem != 1U {
    return Err(IbanError::BadCheck)
  }

  let formatted_buf = StringBuilder()
  for i = 0; i < chars.length(); i = i + 1 {
    if i > 0 && i % 4 == 0 {
      formatted_buf.write_char(' ')
    }
    formatted_buf.write_char(chars[i])
  }

  let check_digits_buf = StringBuilder()
  check_digits_buf.write_char(chars[2])
  check_digits_buf.write_char(chars[3])

  let bban_buf = StringBuilder()
  for i = 4; i < chars.length(); i = i + 1 {
    bban_buf.write_char(chars[i])
  }

  Ok(IbanInfo::{
    country,
    check_digits: check_digits_buf.to_string(),
    bban: bban_buf.to_string(),
    formatted: formatted_buf.to_string(),
    length: norm_len,
  })
}
