// Implementation of money:amount@0.1.0 arithmetic interface in MoonBit

fn exponent(currency : String) -> Int? {
  match currency {
    "USD"
    | "EUR"
    | "GBP"
    | "CHF"
    | "CAD"
    | "AUD"
    | "CNY"
    | "INR"
    | "SEK"
    | "NOK"
    | "DKK"
    | "PLN" => Some(2)
    "JPY" | "KRW" => Some(0)
    "BHD" | "KWD" | "TND" => Some(3)
    _ => None
  }
}

fn checked_exponent(currency : String) -> Result[Int, MoneyError] {
  match exponent(currency) {
    Some(exp) => Ok(exp)
    None => Err(MoneyError::UnknownCurrency(currency))
  }
}

fn pow10(exp : Int) -> Int64 {
  let mut res = 1L
  for i = 0; i < exp; i = i + 1 {
    res = res * 10L
  }
  res
}

fn parse_int64(s : String) -> Int64? {
  if s.is_empty() {
    return None
  }
  let mut n = 0L
  let zero_code = '0'.to_int()
  for c in s {
    if c >= '0' && c <= '9' {
      let d = (c.to_int() - zero_code).to_int64()
      n = n * 10L + d
    } else {
      return None
    }
  }
  Some(n)
}

///|
pub fn parse(decimal : String, currency : String) -> Result[Amount, MoneyError] {
  let exp = match checked_exponent(currency) {
    Ok(e) => e
    Err(err) => return Err(err)
  }
  let (negative, digits) = if decimal.has_prefix("-") {
    (true, decimal[1:].to_owned())
  } else {
    (false, decimal)
  }

  let mut dot_idx = -1
  let len = digits.length()
  for i = 0; i < len; i = i + 1 {
    if digits[i].to_int().unsafe_to_char() == '.' {
      dot_idx = i
      break
    }
  }

  let (major_str, minor_str) = if dot_idx >= 0 {
    (
      digits[0:dot_idx].to_owned(),
      digits[dot_idx + 1:].to_owned(),
    )
  } else {
    (digits, "")
  }

  if major_str.is_empty() {
    return Err(MoneyError::UnknownCurrency(currency))
  }
  for c in major_str {
    if !(c >= '0' && c <= '9') {
      return Err(MoneyError::UnknownCurrency(currency))
    }
  }
  for c in minor_str {
    if !(c >= '0' && c <= '9') {
      return Err(MoneyError::UnknownCurrency(currency))
    }
  }
  if minor_str.length() != exp {
    return Err(MoneyError::UnknownCurrency(currency))
  }

  let major = match parse_int64(major_str) {
    Some(v) => v
    None => return Err(MoneyError::Overflow)
  }
  let minor = if minor_str.is_empty() {
    0L
  } else {
    match parse_int64(minor_str) {
      Some(v) => v
      None => return Err(MoneyError::Overflow)
    }
  }

  let scaled = major * pow10(exp)
  let mut units = scaled + minor
  if negative {
    units = -units
  }

  Ok(Amount::{ units, currency })
}

///|
pub fn format(a : Amount) -> Result[String, MoneyError] {
  let exp = match checked_exponent(a.currency) {
    Ok(e) => e
    Err(err) => return Err(err)
  }
  if exp == 0 {
    return Ok(a.units.to_string())
  }

  let negative = a.units < 0L
  let abs_units = if a.units < 0L { -a.units } else { a.units }
  let divisor = pow10(exp)
  let major = abs_units / divisor
  let minor = abs_units % divisor

  let buf = StringBuilder()
  if negative {
    buf.write_char('-')
  }
  buf.write_string(major.to_string())
  buf.write_char('.')

  let minor_str = minor.to_string()
  let pad = exp - minor_str.length()
  for i = 0; i < pad; i = i + 1 {
    buf.write_char('0')
  }
  buf.write_string(minor_str)
  Ok(buf.to_string())
}

///|
pub fn add(a : Amount, b : Amount) -> Result[Amount, MoneyError] {
  if a.currency != b.currency {
    return Err(MoneyError::CurrencyMismatch)
  }
  match checked_exponent(a.currency) {
    Ok(_) => ()
    Err(err) => return Err(err)
  }
  let units = a.units + b.units
  Ok(Amount::{ units, currency: a.currency })
}

///|
pub fn subtract(a : Amount, b : Amount) -> Result[Amount, MoneyError] {
  if a.currency != b.currency {
    return Err(MoneyError::CurrencyMismatch)
  }
  match checked_exponent(a.currency) {
    Ok(_) => ()
    Err(err) => return Err(err)
  }
  let units = a.units - b.units
  Ok(Amount::{ units, currency: a.currency })
}

///|
pub fn scale(a : Amount, factor : Int64) -> Result[Amount, MoneyError] {
  match checked_exponent(a.currency) {
    Ok(_) => ()
    Err(err) => return Err(err)
  }
  let units = a.units * factor
  Ok(Amount::{ units, currency: a.currency })
}

///|
pub fn allocate(
  total : Amount,
  shares : UInt
) -> Result[Array[Amount], MoneyError] {
  if shares == 0U {
    return Err(MoneyError::DivideByZero)
  }
  match checked_exponent(total.currency) {
    Ok(_) => ()
    Err(err) => return Err(err)
  }

  let n = shares.reinterpret_as_int().to_int64()
  let base = total.units / n
  let remainder = total.units - base * n

  let extra = if remainder >= 0L { 1L } else { -1L }
  let count = (if remainder < 0L { -remainder } else { remainder }).to_int()

  let out : Array[Amount] = []
  let total_shares = shares.reinterpret_as_int()
  for i = 0; i < total_shares; i = i + 1 {
    let units = if i < count { base + extra } else { base }
    out.push(Amount::{ units, currency: total.currency })
  }
  Ok(out)
}

///|
pub fn compare(a : Amount, b : Amount) -> Result[Int, MoneyError] {
  if a.currency != b.currency {
    return Err(MoneyError::CurrencyMismatch)
  }
  match checked_exponent(a.currency) {
    Ok(_) => ()
    Err(err) => return Err(err)
  }
  if a.units < b.units {
    Ok(-1)
  } else if a.units > b.units {
    Ok(1)
  } else {
    Ok(0)
  }
}
